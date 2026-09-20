// SPDX-License-Identifier: GPL-3.0-or-later
//! Native stock generation-1 endpoint for local and remote connections.
//! This owns a client connection only, never the server or its terminals.
use crate::client::{Command, EndpointEvent, Event, Focus, Input, Navigation, Pane, Tab};
use serde_json::{json, Value};
use std::{collections::{HashMap, HashSet}, fmt, future::Future, io,
    path::{Path, PathBuf}, sync::{Arc, Mutex as StdMutex, atomic::{AtomicBool, AtomicU64, Ordering}},
    thread::{self, JoinHandle}, time::Duration};
use tokio::{io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    net::UnixStream, sync::{Mutex, Notify, Semaphore, OwnedSemaphorePermit, mpsc, oneshot, watch}, time::{timeout, Instant}};
#[path = "endpoint/codec.rs"]
mod codec;
#[path = "endpoint/presentation.rs"]
mod presentation;
#[path = "endpoint/strict_json.rs"]
mod strict_json;
use codec::{Decoder, Surface, Patch, AssetKey};
const TIMEOUT: Duration = Duration::from_secs(10);
const API_LIMIT: usize = 8 * 1024 * 1024;
const OUTPUT_LIMIT: usize = 1024 * 1024;
const FRAME_LIMIT: usize = 32768;
type Result<T, E = Error> = std::result::Result<T, E>;
#[derive(Clone, Debug)]
struct Error { code: String, message: String }
impl Error {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self { Self { code: code.into(), message: message.into() } }
    fn protocol(message: impl Into<String>) -> Self { Self::new("protocol_mismatch", message) }
    fn fatal(&self) -> bool { matches!(self.code.as_str(), "authentication" | "protocol_mismatch" | "stale_boot" | "line_too_large" | "generation_exhausted" | "too_many_resources") }
    fn closes_connection(&self) -> bool { self.fatal() || matches!(self.code.as_str(), "io" | "timeout" | "upstream_closed") }
}
impl fmt::Display for Error { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "{}: {}", self.code, self.message) } }
impl From<io::Error> for Error { fn from(e: io::Error) -> Self { Self::new("io", e.to_string()) } }
impl From<serde_json::Error> for Error { fn from(e: serde_json::Error) -> Self { Self::protocol(e.to_string()) } }
async fn bounded<T>(future: impl Future<Output = Result<T>>) -> Result<T> { timeout(TIMEOUT, future).await.map_err(|_| Error::new("timeout", "Herdr operation made no progress for 10 seconds"))? }
fn decode(raw: &[u8]) -> Result<Value> {
    let value = serde_json::from_slice::<strict_json::Unique>(raw)?.0;
    if !value.is_object() { return Err(Error::protocol("Expected JSON object")); } Ok(value)
}
fn encode(value: &Value, limit: usize) -> Result<Vec<u8>> {
    let mut out = serde_json::to_vec(value)?; out.push(b'\n');
    if out.len() > limit { return Err(Error::new("line_too_large", "JSON record exceeds limit")); } Ok(out)
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> { value.get(key).and_then(Value::as_str).ok_or_else(|| Error::protocol(format!("Invalid string {key}"))) }
fn identity<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    let s = text(value, key)?;
    if s.is_empty() || s.len() > 256 || s.chars().any(char::is_control) { return Err(Error::protocol(format!("Invalid identity {key}"))); } Ok(s)
}
fn number(value: &Value, key: &str, min: u64, max: u64) -> Result<u64> { value.get(key).and_then(Value::as_u64).filter(|n| (min..=max).contains(n)).ok_or_else(|| Error::protocol(format!("Invalid integer {key}"))) }
fn boolean(value: &Value, key: &str) -> Result<bool> { value.get(key).and_then(Value::as_bool).ok_or_else(|| Error::protocol(format!("Invalid boolean {key}"))) }
fn array<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    value.get(key).and_then(Value::as_array).filter(|a| a.len() <= 512).ok_or_else(|| Error::new("too_many_resources", format!("Invalid {key}; maximum 512 resources")))
}
fn unwrap_response(mut response: Value, expected_id: &str) -> Result<Value> {
    if response.get("id").and_then(Value::as_str) != Some(expected_id) { return Err(Error::protocol("API response identity mismatch")); }
    if let Some(error) = response.get("error") { return Err(Error::new(text(error, "code")?, text(error, "message")?)); }
    let value = response.get_mut("result").map(Value::take).ok_or_else(|| Error::protocol("Missing API result"))?;
    if !value.is_object() { return Err(Error::protocol("API result is not an object")); } Ok(value)
}
async fn record<R: AsyncBufRead + Unpin>(reader: &mut R, limit: usize) -> Result<Option<Value>> {
    let mut raw = Vec::new();
    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() { return if raw.is_empty() { Ok(None) } else { Err(Error::protocol("Unterminated JSON record")) }; }
        let end = buffer.iter().position(|b| *b == b'\n').map(|n| n + 1);
        let n = end.unwrap_or(buffer.len());
        if n > limit - raw.len() { return Err(Error::new("line_too_large", "JSON record exceeds limit")); }
        raw.extend_from_slice(&buffer[..n]); reader.consume(n);
        if end.is_some() { return decode(&raw).map(Some); }
    }
}
async fn api(socket: &Path, method: &str, params: Value) -> Result<Value> {
    bounded(async {
        // Each request uses a fresh connection, so this fixed ID is scoped uniquely
        // without a global serial counter or persistent state.
        let request = encode(&json!({"id":"kherdr:local:api", "method":method, "params":params}), API_LIMIT)?;
        let mut stream = UnixStream::connect(socket).await?;
        stream.write_all(&request).await?;
        let response = record(&mut BufReader::new(stream), API_LIMIT).await?.ok_or_else(|| Error::new("upstream_closed", "API closed without response"))?;
        unwrap_response(response, "kherdr:local:api")
    }).await
}
/// One bounded, strictly decoded request; no retries of mutations or input.
pub fn api_request(socket: &Path, method: &str, params: Value) -> std::result::Result<Value, String> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
    runtime.block_on(api(socket, method, params)).map_err(|e| e.to_string())
}

struct Status { failure: StdMutex<Option<crate::ssh::Failure>> }
#[derive(Clone)]
pub(crate) struct Control { cancel: watch::Sender<bool>, status: Arc<Status> }
impl Control {
    pub(crate) fn cancel(&self) { let _ = self.cancel.send(true); }
    pub(crate) fn failure(&self) -> Option<crate::ssh::Failure> { self.status.failure.lock().ok()?.clone() }
}
pub(crate) enum Connection {
    Local { endpoint_socket: PathBuf },
    Remote { pipes: crate::ssh::Pipes, control: crate::ssh::Control },
}
pub(crate) type Delivery = (EndpointEvent, OwnedSemaphorePermit);
pub(crate) struct Transport { pub(crate) control: Control, worker: Option<JoinHandle<()>> }
impl Transport {
    pub(crate) fn start(connection: Connection) -> std::result::Result<(Self, mpsc::Sender<Command>, mpsc::Receiver<Delivery>), String> {
        let (commands, input) = mpsc::channel(1);
        let (sender, events) = mpsc::channel(64);
        let (cancel, mut cancelled) = watch::channel(false);
        let control = Control { cancel, status: Arc::new(Status { failure: StdMutex::new(None) }) };
        let status = control.clone();
        let worker = thread::Builder::new().name("herdr-endpoint".into()).spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
                runtime.block_on(async {
                    let output = Arc::new(Output { sender, capacity: Arc::new(Semaphore::new(OUTPUT_LIMIT)), progress: Arc::new(AtomicU64::new(0)) });
                    tokio::select! { biased;
                        _ = cancelled.changed() => Ok(()),
                        result = serve(connection, input, &output) => result,
                    }
                })
            }));
            let failure = match result { Ok(Ok(())) => None, Ok(Err(e)) => Some(crate::ssh::Failure { fatal: e.fatal(), message: e.to_string() }),
                Err(_) => Some(crate::ssh::Failure { fatal: true, message: "Native endpoint worker panicked".into() }) };
            if let Ok(mut stored) = status.status.failure.lock() { *stored = failure; }
        }).map_err(|e| e.to_string())?;
        Ok((Self { control, worker: Some(worker) }, commands, events))
    }
    pub(crate) fn stop(&mut self) -> std::result::Result<(), String> {
        self.control.cancel();
        if let Some(worker) = self.worker.take() { worker.join().map_err(|_| "Native endpoint worker panicked".to_string())?; } Ok(())
    }
}
impl Drop for Transport { fn drop(&mut self) { let _ = self.stop(); } }
struct Output { sender: mpsc::Sender<Delivery>, capacity: Arc<Semaphore>, progress: Arc<AtomicU64> }
impl Output {
    async fn emit(&self, event: Event) -> Result<()> { self.deliver(EndpointEvent::Ui(event)).await }
    async fn deliver(&self, event: EndpointEvent) -> Result<()> {
        let size = match &event {
            EndpointEvent::Ui(Event::Frame { bytes, .. }) => bytes.len() + 64,
            EndpointEvent::NavigationFailed { message,.. } => message.len()+64,
            EndpointEvent::Ui(Event::Error { message,.. }) | EndpointEvent::Ui(Event::Unavailable(message)) | EndpointEvent::Ui(Event::Disconnected(message)) => message.len()+64,
            EndpointEvent::Ui(Event::Navigation { workspaces,tabs,panes,focused_workspace,agents }) => 128 + focused_workspace.as_ref().map_or(0,String::len)
                + workspaces.iter().map(|w|w.workspace_id.len()+w.label.len()+w.status.len()+w.worktree_key.len()+w.worktree_label.len()+128).sum::<usize>()
                + agents.iter().map(|a|a.pane_id.len()+a.workspace_id.len()+a.tab_id.len()+a.name.len()+a.agent.len()+a.status.len()+144).sum::<usize>()
                + tabs.iter().map(|t| t.tab_id.len()+t.workspace_id.len()+t.workspace.len()+t.name.len()+t.status.len()+t.pane_id.len()+128).sum::<usize>()
                + panes.iter().map(|p| p.pane_id.len()+p.name.len()+p.agent.len()+p.status.len()+p.workspace_id.len()+p.workspace.len()+p.tab_id.len()+p.detail.len()+192).sum::<usize>(),
            EndpointEvent::Ui(Event::Layout{panes,..})=>128+panes.iter().map(|pane|pane.pane_id.len()+64).sum::<usize>(),
            _ => 16384,
        };
        if size > OUTPUT_LIMIT { return Err(Error::new("too_many_resources", "Endpoint UI metadata exceeds 1MiB")); }
        let permit = bounded(async { Arc::clone(&self.capacity).acquire_many_owned(size as u32).await.map_err(|_| Error::new("upstream_closed", "Endpoint output closed")) }).await?;
        bounded(async { self.sender.send((event,permit)).await.map_err(|_| Error::new("upstream_closed", "Endpoint output closed")) }).await?;
        self.progress.fetch_add(1,Ordering::Relaxed); Ok(())
    }
    async fn error(&self, error: &Error, fatal: bool) -> Result<()> {
        self.emit(Event::Error { message: format!("{}: {}",error.code,presentation::truncate(&error.message)), fatal }).await
    }
}
type Reader = Box<dyn AsyncRead + Unpin + Send>;
type Writer = Box<dyn AsyncWrite + Unpin + Send>;
struct Pending { boot: String, bytes: Vec<u8>, reply: oneshot::Sender<Result<Value>> }
struct Shared {
    state: Mutex<State>, writer: Mutex<Writer>, output: Arc<Output>,
    pending: StdMutex<HashMap<String, Pending>>, serial: AtomicU64, changed: Notify,
    busy: AtomicBool, progress: AtomicU64, received: watch::Sender<Instant>,
}
struct CachedAsset { id: u32, bytes: Vec<u8>, uploaded: bool }
struct State {
    methods: HashSet<String>, snapshot: Option<Value>, boot: Option<String>,
    navigation: Option<(Vec<crate::client::Workspace>, Vec<Tab>, Vec<Pane>, Option<String>, Vec<crate::client::Agent>)>,
    focus: Option<Option<String>>, generation: u64,
    dimensions: [u16; 4], started: bool, active: bool, stream_ready: bool,
    identity_refresh: bool, activation_pending: bool, floor: u64,
    surface: Option<Surface>, surface_revision: u64, popup: Option<String>,
    assets: HashMap<AssetKey, CachedAsset>, asset_serial: u32, retired: Vec<u32>,
    presented: Option<presentation::Presented>, layout:Option<(Vec<crate::client::SurfacePane>,Option<[u16;2]>)>,
}
impl State {
    fn new(methods: HashSet<String>) -> Self {
        Self { methods, snapshot: None, boot: None, navigation: None, focus: None, generation: 0,
            dimensions: [80, 24, 8, 16], started: false, active: false, stream_ready: false, identity_refresh: false,
            activation_pending: false, floor: 0, surface: None, surface_revision: 0, popup: None,
            assets: HashMap::new(), asset_serial: 0, retired: Vec::new(), presented: None, layout:None }
    }
    fn coherent(&self, surface: &Surface) -> bool {
        self.started && self.active && !self.activation_pending && self.snapshot.as_ref().is_some_and(|s| s["revision"].as_u64() == Some(surface.projection))
            && self.boot.as_deref() == Some(surface.boot.as_str()) && surface.projection >= self.floor
            && self.focus.as_ref().is_some_and(|id| id.as_ref().is_none_or(|id| surface.panes.iter().any(|p| p.focused && &p.id == id)))
            && [surface.frame.width, surface.frame.height] == self.dimensions[..2]
    }
    async fn publish_focus(&mut self, output: &Output) -> Result<()> {
        self.generation = self.generation.checked_add(1).ok_or_else(|| Error::new("generation_exhausted", "Focus generations exhausted"))?;
        self.stream_ready = false;
        self.presented = None;
        let pane = self.focus.as_ref().ok_or_else(|| Error::protocol("Focus before snapshot"))?;
        output.emit(Event::Focus(Focus { generation:self.generation, pane_id:pane.clone() })).await?;
        if self.started {
            for asset in self.assets.values_mut() { asset.uploaded = false; }
            output.emit(Event::Reset { generation:self.generation }).await?;
        } Ok(())
    }
    async fn frame(&mut self, output: &Output, bytes: &[u8]) -> Result<()> {
        if self.focus.as_ref().is_none_or(|pane| pane.is_none()) { return Ok(()); }
        for part in bytes.chunks(FRAME_LIMIT) {
            output.emit(Event::Frame { generation:self.generation, bytes:part.to_vec(), complete:false }).await?;
        }
        Ok(())
    }
    async fn commit_frame(&mut self, output: &Output) -> Result<()> {
        if self.focus.as_ref().is_none_or(|pane| pane.is_none()) { return Ok(()); }
        output.emit(Event::Frame { generation:self.generation, bytes:Vec::new(), complete:true }).await?;
        self.stream_ready = true;
        Ok(())
    }
}
impl Shared {
    fn serial(&self, prefix: &str) -> Result<String> {
        let n = self.serial.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1)).map_err(|_| Error::new("generation_exhausted", "Request IDs exhausted"))?;
        Ok(format!("{prefix}:{n}"))
    }
    async fn send(&self, bytes: Vec<u8>) -> Result<()> {
        if bytes.is_empty() || bytes.len() > 2 * 1024 * 1024 { return Err(Error::new("line_too_large", "Endpoint request exceeds 2MiB")); }
        let mut writer = self.writer.lock().await;
        bounded(async { writer.write_all(&(bytes.len() as u32).to_le_bytes()).await?; writer.write_all(&bytes).await?; Ok(()) }).await
    }
    async fn control(&self, kind: &str, data: &str) -> Result<()> { self.send(codec::control(kind, data)).await }
    async fn reply<T>(&self, future: impl Future<Output = Result<T>>) -> Result<T> {
        tokio::pin!(future);
        let mut progress = (self.progress.load(Ordering::Relaxed), self.output.progress.load(Ordering::Relaxed));
        loop {
            match timeout(TIMEOUT, &mut future).await {
                Ok(result) => return result,
                Err(_) => {
                    let next = (self.progress.load(Ordering::Relaxed), self.output.progress.load(Ordering::Relaxed));
                    if !self.busy.load(Ordering::Relaxed) || progress == next { return Err(Error::new("timeout", "Endpoint response stalled")); }
                    progress = next;
                }
            }
        }
    }
    async fn wait_state(&self, predicate: impl Fn(&State) -> bool) -> Result<()> {
        bounded(async { loop {
            let notified = self.changed.notified(); tokio::pin!(notified); notified.as_mut().enable();
            if predicate(&*self.state.lock().await) { return Ok(()); } notified.await;
        } }).await
    }
    async fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        self.wait_state(|s| s.boot.is_some()).await?;
        let boot = { let s = self.state.lock().await;
            if !s.methods.contains(method) { return Err(Error::new("unsupported_method", format!("Endpoint does not advertise {method}"))); }
            s.boot.clone().ok_or_else(|| Error::protocol("Missing endpoint boot"))?
        };
        let id = self.serial("kindle")?;
        let request = serde_json::to_string(&json!({"id":id, "method":method, "params":params}))?;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().map_err(|_| Error::protocol("Endpoint pending lock poisoned"))?.insert(id.clone(), Pending { boot: boot.clone(), bytes: Vec::new(), reply: tx });
        let mut raw = vec![15]; codec::string(&mut raw, &boot); codec::string(&mut raw, &request);
        let result = async {
            self.send(raw).await?;
            let response = self.reply(async { rx.await.map_err(|_| Error::new("upstream_closed", "Endpoint reply cancelled"))? }).await?;
            unwrap_response(response, &id)
        }.await;
        self.pending.lock().map_err(|_| Error::protocol("Endpoint pending lock poisoned"))?.remove(&id);
        result
    }

}
async fn packet(reader: &mut (impl AsyncRead + Unpin)) -> Result<Vec<u8>> {
    // Header may wait idle; health bounds idle liveness. Once a frame starts its
    // entire header/payload must arrive within the fixed frame deadline.
    let first = reader.read_u8().await?;
    bounded(async {
        let mut header = [first, 0, 0, 0]; reader.read_exact(&mut header[1..]).await?;
        let size = u32::from_le_bytes(header) as usize;
        if size == 0 || size > 32 * 1024 * 1024 { return Err(Error::new("line_too_large", "Endpoint packet exceeds 32MiB")); }
        let mut bytes = vec![0; size]; reader.read_exact(&mut bytes).await?; Ok(bytes)
    }).await
}
async fn connect(socket: &Path) -> Result<UnixStream> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match UnixStream::connect(socket).await {
            Ok(stream) => return Ok(stream),
            Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused) && Instant::now() < deadline => tokio::time::sleep(Duration::from_millis(50)).await,
            Err(e) => return Err(e.into()),
        }
    }
}
async fn serve(connection: Connection, input: mpsc::Receiver<Command>, output: &Arc<Output>) -> Result<()> {
    match connection {
        Connection::Local { endpoint_socket } => {
            let stream = connect(&endpoint_socket).await?;
            let (reader,writer) = stream.into_split();
            initialize(Box::new(reader),Box::new(writer),input,output,None).await
        },
        Connection::Remote { pipes, control } => {
            let mut stderr = UnixStream::from_std(pipes.stderr)?;
            let diagnostics = StdMutex::new(std::collections::VecDeque::with_capacity(4096));
            let drain = async { let mut bytes=[0u8;4096]; loop {
                let n=stderr.read(&mut bytes).await?; if n==0 { return std::future::pending::<Result<()>>().await; }
                let mut buffer=diagnostics.lock().map_err(|_| Error::protocol("Diagnostic lock poisoned"))?;
                for byte in &bytes[..n] { if buffer.len()==4096 { buffer.pop_front(); } buffer.push_back(*byte); }
            }};
            let session = async {
                while !control.ready() {
                    if control.finished() {
                        return Err(match control.failure() {
                            Some(error) => Error::new(if error.fatal { "authentication" } else { "upstream_closed" }, error.message),
                            None => Error::new("upstream_closed", "SSH closed before native endpoint startup"),
                        });
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                initialize(Box::new(UnixStream::from_std(pipes.stdout)?),Box::new(UnixStream::from_std(pipes.stdin)?),input,output,Some(&control)).await
            };
            let result=tokio::select! { result=session=>result, result=drain=>result };
            result.map_err(|mut error| { if let Ok(bytes)=diagnostics.lock() { if !bytes.is_empty() { error.message.push_str(": "); error.message.push_str(&String::from_utf8_lossy(&bytes.iter().copied().collect::<Vec<_>>())); } } error })
        }
    }
}
async fn initialize(mut reader: Reader, mut writer: Writer, input: mpsc::Receiver<Command>, output: &Arc<Output>, ssh: Option<&crate::ssh::Control>) -> Result<()> {
    let mut rejected = false;
    let negotiated: Result<HashSet<String>> = async {
    let hello = json!({"generation":1, "cell_width_px":8, "cell_height_px":16, "surface_size":{"cols":80,"rows":24},
        "pixel_mouse":false,"direct_graphics":false,"endpoint_keybindings":false,"mouse_capture":false,"surface_active":false,
        "snapshot_codecs":["shell.snapshot.v1"],"surface_codecs":["shell.surface.v1"],"input_codecs":["shell.input.semantic.v1"],"blob_codecs":["shell.blob.v1"]});
    let hello = codec::control("endpoint.hello.v1", &serde_json::to_string(&hello)?);
    bounded(async { writer.write_all(&(hello.len() as u32).to_le_bytes()).await?; writer.write_all(&hello).await?; Ok(()) }).await?;
    let raw = bounded(packet(&mut reader)).await?; let mut d = Decoder::new(&raw);
    if d.u32()? != 20 || d.string()? != "endpoint.welcome.v1" { return Err(Error::protocol("Invalid endpoint welcome")); }
    let welcome = decode(d.string()?.as_bytes())?; d.finish()?;
    if let Some(error) = welcome.get("error").filter(|v| !v.is_null()) { rejected = true; return Err(Error::new(text(error, "code")?, text(error, "message")?)); }
    if number(&welcome, "generation", 1, 1)? != 1 { return Err(Error::protocol("Unsupported endpoint generation")); }
    for (key, expected) in [("snapshot_codec","shell.snapshot.v1"),("surface_codec","shell.surface.v1"),("input_codec","shell.input.semantic.v1"),("blob_codec","shell.blob.v1")] {
        if text(&welcome,key)? != expected { return Err(Error::protocol("Unsupported endpoint codec")); }
    }
    let strings = |key| -> Result<HashSet<String>> {
        array(&welcome,key)?.iter().map(|v| v.as_str().filter(|s| s.len() <= 256).map(str::to_owned).ok_or_else(|| Error::protocol("Invalid endpoint capability"))).collect()
    };
    let methods = strings("methods")?; let capabilities = strings("capabilities")?;
    if !methods.contains("client_shell.surface.set") || !capabilities.contains("surface_interest") || !capabilities.contains("health_check") { return Err(Error::protocol("Endpoint lacks surface interest or health checks")); }
    let version = text(&welcome,"server_version")?;
    if version.is_empty() || version.len() > 64 { return Err(Error::protocol("Invalid server version")); }
    Ok(methods)
    }.await;
    let methods = match negotiated {
        Ok(methods) => methods,
        Err(error) => {
            if rejected || error.code == "protocol_mismatch" || ssh.is_some_and(|control| matches!(control.exit_status(), Some(126 | 127))) {
                output.emit(Event::Unavailable(error.to_string())).await?;
            }
            return Err(error);
        },
    };
    output.emit(Event::Ready).await?;
    let shared = Shared { state: Mutex::new(State::new(methods)), writer: Mutex::new(writer), output: Arc::clone(output),
        pending: StdMutex::new(HashMap::new()), serial: AtomicU64::new(1), changed: Notify::new(), busy: AtomicBool::new(false),
        progress: AtomicU64::new(0), received: watch::channel(Instant::now()).0 };
    tokio::select! {
        result = endpoint(&shared, &mut reader) => result,
        result = commands(&shared, input) => result,
        result = health(&shared) => result,
    }
}

async fn endpoint(shared: &Shared, reader: &mut Reader) -> Result<()> {
    loop {
        shared.busy.store(false, Ordering::Relaxed);
        let raw = packet(reader).await?;
        shared.received.send_replace(Instant::now());
        shared.busy.store(true, Ordering::Relaxed);
        let mut d = Decoder::new(&raw);
        match d.u32()? {
            20 => {
                let kind = d.string()?; let data = d.string()?; d.finish()?;
                match kind.as_str() {
                    "shell.snapshot.v1" => snapshot(shared,decode(data.as_bytes())?).await?,
                    _ => {}, // Unknown named controls are optional in generation 1.
                }
            },
            18 => {
                let boot = d.string()?; let id = d.string()?; let final_chunk = d.boolean()?; let bytes = d.blob()?; d.finish()?;
                let mut pending = shared.pending.lock().map_err(|_| Error::protocol("Pending state poisoned"))?;
                if let Some(entry) = pending.get_mut(&id) {
                    if entry.boot != boot { return Err(Error::protocol("Response belongs to another boot")); }
                    if bytes.len() > API_LIMIT - entry.bytes.len() { return Err(Error::new("line_too_large", "Endpoint response exceeds 8MiB")); }
                    entry.bytes.extend_from_slice(&bytes);
                    if final_chunk {
                        let entry = pending.remove(&id).ok_or_else(|| Error::protocol("Missing pending response"))?;
                        let response = decode(&entry.bytes)?; let _ = entry.reply.send(Ok(response));
                    }
                }
            },
            13 => { let surface=d.surface()?; d.finish()?; full_surface(shared,surface).await?; },
            19 => { let patch=d.patch()?; d.finish()?; surface_patch(shared,patch).await?; },
            15 => {
                let message = d.string()?; d.finish()?;
                shared.state.lock().await.stream_ready = false;
                shared.output.error(&Error::new("endpoint_error", presentation::truncate(&message)),false).await?;
            },
            3 => { let reason = d.optional_string()?; d.finish()?; return Err(Error::new("upstream_closed", reason.unwrap_or_else(|| "Herdr server stopped".into()))); },
            4 | 5 | 6 | 7 | 8 | 9 | 14 | 17 => {},
            tag => return Err(Error::protocol(format!("Unexpected endpoint message {tag}; no internal/full-TUI fallback"))),
        }
        shared.progress.fetch_add(1, Ordering::Relaxed);
    }
}

fn validate_snapshot(snapshot: &Value) -> Result<()> {
    for (kind,id_key) in [("workspaces","workspace_id"),("tabs","tab_id"),("panes","pane_id"),("agents","pane_id")] {
        let mut seen = HashSet::new();
        for resource in array(snapshot,kind)? {
            if !seen.insert(identity(resource,id_key)?) { return Err(Error::protocol(format!("Duplicate {kind} identity"))); }
            boolean(resource,"focused")?;
            if kind != "workspaces" { identity(resource,"workspace_id")?; }
            if matches!(kind,"panes" | "agents") { identity(resource,"tab_id")?; }
            if kind != "panes" { text(resource,"agent_status")?; }
            if matches!(kind,"workspaces" | "tabs") { text(resource,"label")?; boolean(resource,"custom_label")?; }
            for key in ["label","cwd","foreground_cwd","title","display_agent","terminal_title","terminal_title_stripped","agent","name"] {
                if resource.get(key).is_some_and(|v| !v.is_null() && !v.is_string()) { return Err(Error::protocol(format!("Invalid resource text {key}"))); }
            }
        }
    }
    for key in ["focused_pane_id","focused_tab_id","focused_workspace_id"] {
        if !snapshot.get(key).is_some_and(Value::is_null) { identity(snapshot,key)?; }
    }
    Ok(())
}
async fn snapshot(shared: &Shared, mut snapshot: Value) -> Result<()> {
    let boot = identity(&snapshot,"boot_id")?.to_owned(); let revision = number(&snapshot,"revision",1,u64::MAX)?;
    validate_snapshot(&snapshot)?;
    {
        let mut state = shared.state.lock().await;
        if state.boot.as_ref().is_some_and(|old| old != &boot) { return Err(Error::new("stale_boot","Endpoint boot changed; reconnect without replay")); }
        if state.snapshot.as_ref().is_some_and(|old| old["revision"].as_u64().is_some_and(|old| revision < old)) { return Err(Error::protocol("Snapshot revision regressed")); }
        state.boot = Some(boot.clone());
        state.identity_refresh = true;
        shared.changed.notify_waiters();
    }
    let Value::Array(panes) = snapshot["panes"].take() else { return Err(Error::protocol("Missing pane list")); };
    let mut panes=panes;
    let mut agents = HashMap::new();
    for agent in array(&snapshot,"agents")? { let id = identity(agent,"pane_id")?;
        if agents.insert(id,agent).is_some() { return Err(Error::protocol("Duplicate projected agent identity")); }
    }
    for pane in &mut panes {
        if let Some(agent) = agents.get(identity(pane,"pane_id")?) {
            for (key,value) in agent.as_object().ok_or_else(|| Error::protocol("Invalid agent object"))? {
                if !["pane_id","workspace_id","tab_id","focused"].contains(&key.as_str()) { pane[key] = value.clone(); }
            }
        }
        if pane.get("agent_status").is_none() { pane["agent_status"] = "unknown".into(); }
        presentation::normalize_pairs(pane,"state_labels")?; presentation::normalize_pairs(pane,"tokens")?;
    }
    snapshot["panes"] = panes.into();
    for workspace in snapshot["workspaces"].as_array_mut().ok_or_else(|| Error::protocol("Invalid workspaces"))? { presentation::normalize_pairs(workspace,"tokens")?; }
    let navigation = presentation::navigation(&snapshot)?;
    let focus = match array(&snapshot,"panes")?.iter().find(|p| p["pane_id"] == snapshot["focused_pane_id"]) {
        Some(pane) => Some(identity(pane,"pane_id")?.to_owned()),
        None => None,
    };
    let mut state = shared.state.lock().await;
    state.boot = Some(boot); state.snapshot = Some(snapshot);
    if state.navigation.as_ref() != Some(&navigation) {
        shared.output.emit(Event::Navigation { workspaces:navigation.0.clone(), tabs:navigation.1.clone(), panes:navigation.2.clone(), focused_workspace:navigation.3.clone(), agents:navigation.4.clone() }).await?;
        state.navigation = Some(navigation);
    }
    if state.focus.as_ref() != Some(&focus) { state.focus = Some(focus); state.publish_focus(&shared.output).await?; }
    if state.surface.as_ref().is_some_and(|s| s.projection != revision) { state.stream_ready = false; }
    state.identity_refresh = false;
    if let Some(surface) = state.surface.take() {
        let result = if state.coherent(&surface)
            && state.presented.as_ref().is_none_or(|old| old.revision != surface.revision) {
            publish_layout(&mut state,&shared.output,&surface).await?; state.render(&shared.output,&surface,true).await
        } else { Ok(()) };
        state.surface = Some(surface); result?;
    }
    shared.changed.notify_waiters(); Ok(())
}

async fn full_surface(shared: &Shared, mut surface: Surface) -> Result<()> {
    let mut state = shared.state.lock().await;
    if state.boot.as_deref() != Some(surface.boot.as_str()) { return Err(Error::new("stale_boot","Surface belongs to another boot")); }
    if surface.revision <= state.surface_revision { return Err(Error::protocol("Surface revision did not increase")); }
    state.surface_revision = surface.revision; state.ingest(&mut surface.scene)?;
    if state.coherent(&surface) { publish_layout(&mut state,&shared.output,&surface).await?; state.render(&shared.output,&surface,true).await?; }
    state.surface = Some(surface); shared.changed.notify_waiters(); Ok(())
}
async fn surface_patch(shared: &Shared, patch: Patch) -> Result<()> {
    let mut state = shared.state.lock().await;
    if state.boot.as_deref() != Some(patch.boot.as_str()) { return Err(Error::new("stale_boot","Patch belongs to another boot")); }
    let mut surface = state.surface.take().ok_or_else(|| Error::protocol("Patch has no full surface"))?;
    if patch.base != surface.revision || patch.projection != surface.projection || patch.revision <= state.surface_revision { return Err(Error::protocol("Invalid patch revision/base")); }
    for row in patch.rows {
        if row.y >= surface.frame.height || usize::from(row.x)+row.cells.len() > usize::from(surface.frame.width) { return Err(Error::protocol("Patch extends outside surface")); }
        let start = usize::from(row.y)*usize::from(surface.frame.width)+usize::from(row.x);
        for (target,cell) in surface.frame.cells[start..start+row.cells.len()].iter_mut().zip(row.cells) { *target = cell; }
    }
    surface.frame.cursor = patch.cursor; surface.revision = patch.revision; state.surface_revision = patch.revision;
    for pane in patch.panes { if let Some(old) = surface.panes.iter_mut().find(|old| old.id == pane.id) { *old = pane; } }
    if state.coherent(&surface) {
        publish_layout(&mut state,&shared.output,&surface).await?; state.render(&shared.output,&surface,false).await?;
    }
    state.surface = Some(surface); shared.changed.notify_waiters(); Ok(())
}
async fn publish_layout(state:&mut State,output:&Output,surface:&Surface)->Result<()> {
    let layout=(surface.panes.iter().map(|pane|crate::client::SurfacePane{pane_id:pane.id.clone(),rect:pane.rect,inner:pane.inner}).collect(),
        surface.popup.as_ref().map(|popup|[popup.frame.width,popup.frame.height]));
    if state.layout.as_ref()!=Some(&layout){output.emit(Event::Layout{panes:layout.0.clone(),popup:layout.1}).await?;state.layout=Some(layout);} Ok(())
}

async fn health(shared: &Shared) -> Result<()> {
    let mut received = shared.received.subscribe();
    loop {
        // Match stock EndpointHealth: any received packet proves liveness.
        // Probe only after five seconds idle, and bound silence after the probe.
        let deadline = *received.borrow_and_update() + Duration::from_secs(5);
        match tokio::time::timeout_at(deadline, received.changed()).await {
            Ok(Ok(())) => continue,
            Ok(Err(_)) => return Err(Error::new("upstream_closed", "Endpoint receive tracking closed")),
            Err(_) => {},
        }
        shared.control("endpoint.health.ping.v1", "").await?;
        shared.reply(async {
            received.changed().await.map_err(|_| Error::new("upstream_closed", "Endpoint receive tracking closed"))
        }).await?;
    }
}

async fn resize_endpoint(shared: &Shared) -> Result<()> {
    let [cols,rows,width,height] = shared.state.lock().await.dimensions;
    let mut bytes = vec![12]; for n in [width,height,cols,rows] { codec::uint(&mut bytes,u64::from(n)); } bytes.push(0); shared.send(bytes).await
}
async fn active(shared: &Shared, active: bool) -> Result<()> {
    {
        let mut state = shared.state.lock().await;
        state.stream_ready = false; state.active = active; state.activation_pending = true;
    }
    let result: Result<()> = async {
        if active { resize_endpoint(shared).await?; } else { shared.send(vec![18,0]).await?; }
        let reply = shared.rpc("client_shell.surface.set",json!({"active":active})).await?;
        if reply["type"] != "client_shell_surface_set" || boolean(&reply,"active")? != active { return Err(Error::protocol("Invalid surface-interest acknowledgement")); }
        let floor = number(&reply,"projection_revision",0,u64::MAX)?;
        shared.state.lock().await.floor = floor;
        if active {
            shared.send(vec![18,1]).await?;
            shared.wait_state(|state| state.snapshot.as_ref().is_some_and(|s| s["revision"].as_u64().is_some_and(|revision| revision >= floor))).await?;
            shared.state.lock().await.publish_focus(&shared.output).await?;
        }
        Ok(())
    }.await;
    let mut state = shared.state.lock().await; state.activation_pending = false;
    result?;
    if active {
        if let Some(surface) = state.surface.take() {
            let result = if state.coherent(&surface) { publish_layout(&mut state,&shared.output,&surface).await?; state.render(&shared.output,&surface,true).await } else { Ok(()) };
            state.surface = Some(surface); result?;
        }
    } Ok(())
}
async fn input_event(shared: &Shared, focus: &Focus, event: &Input) -> Result<()> {
    let target_pane=if let Input::Click{pane,..}=event{Some(pane.clone())}else{None};
    let (count,mut encoded) = match event {
        Input::Key(key) => (1,codec::key_event(key)?),
        Input::Text(text) | Input::Paste(text) => {
            if text.len()>512*1024 || text.contains('\0') { return Err(Error::new("invalid_command","Invalid input text")); }
            let mut bytes=vec![if matches!(event,Input::Text(_)) {1} else {3}]; codec::string(&mut bytes,text); (1,bytes)
        },
        Input::Scroll { up, lines } => { let mut bytes=vec![2,if *up {4} else {5},0,0,0,0,0]; codec::uint(&mut bytes,u64::from(*lines)); (1,bytes) },
        Input::Click{column,row,right,..}=>(2,codec::mouse_click(*column,*row,*right)),
    };
    let generation=focus.generation;
    let (pane,popup) = {
        let state=shared.state.lock().await;
        if state.generation!=generation || state.focus.as_ref()!=Some(&focus.pane_id) || !state.active || state.activation_pending { return Err(Error::new("stale_focus","Input belongs to another endpoint focus")); }
        (focus.pane_id.clone().ok_or_else(||Error::new("stale_focus","No input pane"))?,state.popup.clone())
    };
    let state=bounded(async { loop {
        let notified=shared.changed.notified(); tokio::pin!(notified); notified.as_mut().enable();
        let state=shared.state.lock().await;
        if state.generation!=generation || state.focus.as_ref().and_then(Option::as_ref)!=Some(&pane) || state.popup!=popup || !state.active || state.activation_pending {
            return Err(Error::new("stale_focus","Endpoint focus changed; input was not sent"));
        }
        if state.identity_refresh || !state.stream_ready { drop(state); notified.await; continue; }
        return Ok(state);
    }}).await?;
    if let Some(target)=target_pane.as_deref() {
        if state.surface.as_ref().is_none_or(|surface|!surface.panes.iter().any(|pane|pane.id==target)) {
            return Err(Error::new("stale_focus","Pointer target is no longer in the active surface"));
        }
    }
    // Holding presentation admission through the one send prevents a locally
    // observed topology transition from retargeting it. Stock routes to its
    // boot-scoped pane identity; no private terminal identity is fabricated.
    let target=target_pane.as_deref().unwrap_or(&pane);
    let mut bytes = vec![if popup.is_some() {14} else {13}]; codec::string(&mut bytes,popup.as_deref().unwrap_or(target)); bytes.push(count); bytes.append(&mut encoded);
    let result = shared.send(bytes).await;
    drop(state);
    result
}
async fn navigate(shared: &Shared, action: &Navigation) -> Result<()> {
    let previous_revision={
        let state = shared.state.lock().await;
        if !state.started || !state.active { return Err(Error::new("surface_inactive","Activate this endpoint before navigation")); }
        state.snapshot.as_ref().and_then(|snapshot|snapshot["revision"].as_u64()).ok_or_else(||Error::new("not_started","No endpoint snapshot"))?
    };
    let mut target = None;
    match action {
        Navigation::Focus { pane_id: pane } | Navigation::ClickFocus { pane_id: pane,.. } => {
            if let Navigation::ClickFocus { focus,column,row,right,.. }=action {
                // Stock forwards the click and explicitly focuses its pane.
                // Validate the captured focus/surface before either operation.
                input_event(shared,focus,&Input::Click { pane:pane.clone(),column:*column,row:*row,right:*right }).await?;
            }
            shared.state.lock().await.stream_ready=false;
            let reply=shared.rpc("pane.focus",json!({"pane_id":pane})).await?;
            if reply["type"]!="pane_info" || reply["pane"]["pane_id"]!=pane.as_str() { return Err(Error::protocol("Unexpected pane focus response")); }
            target=Some(Some(pane.clone()));
        },
        Navigation::FocusWorkspace(id)=>{ shared.rpc("workspace.focus",json!({"workspace_id":id})).await?; },
        Navigation::FocusTab(tab) | Navigation::CloseTab(tab) => { shared.rpc(if matches!(action, Navigation::FocusTab(_)) {"tab.focus"} else {"tab.close"},json!({"tab_id":tab})).await?; },
        Navigation::RenameWorkspace{id,label}=>{ shared.rpc("workspace.rename",json!({"workspace_id":id,"label":label})).await?; },
        Navigation::CloseWorkspace(id)=>{ shared.rpc("workspace.close",json!({"workspace_id":id,"close_group":true})).await?; },
        Navigation::NewWorktree{id,branch}=>{ shared.rpc("worktree.create",json!({"workspace_id":id,"branch":branch,"base":"HEAD","focus":true})).await?; },
        Navigation::OpenWorktree{id,path}=>{ shared.rpc("worktree.open",json!({"workspace_id":id,"path":path,"focus":true})).await?; },
        Navigation::RemoveWorktree(id)=>{ shared.rpc("worktree.remove",json!({"workspace_id":id,"force":false})).await?; },
        Navigation::RenameTab{id,label}=>{ shared.rpc("tab.rename",json!({"tab_id":id,"label":label})).await?; },
        Navigation::RenamePane{id,label}=>{ shared.rpc("pane.rename",json!({"pane_id":id,"label":label})).await?; },
        Navigation::SplitPane{id,workspace,right}=>{ shared.rpc("pane.split",json!({"workspace_id":workspace,"target_pane_id":id,"direction":if *right{"right"}else{"down"},"focus":true,"right_click":"herdr","env":{}})).await?; },
        Navigation::SwapPanes{source,target}=>{ shared.rpc("pane.swap",json!({"source_pane_id":source,"target_pane_id":target})).await?; },
        Navigation::ToggleRightClick{id,to_pane}=>{ shared.rpc("pane.input.set",json!({"pane_id":id,"right_click":if *to_pane{"pane"}else{"herdr"}})).await?; },
        Navigation::ZoomPane(id)=>{ shared.rpc("pane.zoom",json!({"pane_id":id,"mode":"toggle"})).await?; },
        Navigation::ClosePane(id)=>{ shared.rpc("pane.close",json!({"pane_id":id})).await?; },
        Navigation::ReloadConfig=>{ shared.rpc("server.reload_config",json!({})).await?; },
        Navigation::NewTab(workspace) => {
            let (method,params) = {
                let state = shared.state.lock().await;
                let snapshot = state.snapshot.as_ref().ok_or_else(|| Error::new("not_started","No endpoint snapshot"))?;
                let mut params = json!({"focus":true,"env":{"NO_COLOR":"1","CLICOLOR":"0","FORCE_COLOR":"0"}});
                if array(snapshot,"workspaces")?.is_empty() {
                    if !snapshot["focused_workspace_id"].is_null() { params["source_workspace_id"] = snapshot["focused_workspace_id"].clone(); }
                    ("workspace.create",params)
                } else { params["workspace_id"] = workspace.as_ref().map_or_else(||snapshot["focused_workspace_id"].clone(),|id|Value::String(id.clone())); ("tab.create",params) }
            };
            shared.rpc(method,params).await?;
        },
        Navigation::NewWorkspace => {
            let params = { let state=shared.state.lock().await; let snapshot=state.snapshot.as_ref().ok_or_else(||Error::new("not_started","No endpoint snapshot"))?;
                let mut params=json!({"focus":true,"env":{"NO_COLOR":"1","CLICOLOR":"0","FORCE_COLOR":"0"}});
                if !snapshot["focused_workspace_id"].is_null() { params["source_workspace_id"]=snapshot["focused_workspace_id"].clone(); } params };
            shared.rpc("workspace.create",params).await?;
        },
    }
    shared.wait_state(|state|state.snapshot.as_ref().and_then(|snapshot|snapshot["revision"].as_u64()).is_some_and(|revision|revision>previous_revision)
        && state.stream_ready && !state.identity_refresh).await?;
    if target.is_some() && shared.state.lock().await.focus != target { return Err(Error::new("stale_focus","Endpoint focus no longer matches selected runtime")); }
    Ok(())
}
async fn commands(shared: &Shared, mut input: mpsc::Receiver<Command>) -> Result<()> {
    while let Some(command)=input.recv().await {
        let navigation_id=match &command { Command::Navigate { id,.. }=>Some(*id), _=>None };
        let result: Result<()> = async {
            match command {
                Command::Start { dimensions, active: enabled } => {
                    shared.wait_state(|s|s.snapshot.is_some()).await?;
                    { let mut s=shared.state.lock().await; if s.started { return Err(Error::new("already_started","Endpoint already started")); } s.dimensions=dimensions; s.started=true; }
                    if enabled { active(shared,true).await?; } else { shared.state.lock().await.publish_focus(&shared.output).await?; }
                },
                Command::Resize(dimensions) => { shared.state.lock().await.dimensions=dimensions; resize_endpoint(shared).await?; },
                Command::Active(enabled)=>active(shared,enabled).await?,
                Command::Navigate { id, action }=>{ navigate(shared,&action).await?; shared.wait_state(|s|s.stream_ready && !s.identity_refresh).await?; shared.output.deliver(EndpointEvent::Ack(id)).await?; },
                Command::Input { focus,event }=>input_event(shared,&focus,&event).await?,
            } Ok(())
        }.await;
        if let Err(error)=result { if error.closes_connection() { return Err(error); }
            if let Some(id)=navigation_id { shared.output.deliver(EndpointEvent::NavigationFailed { id,message:presentation::truncate(&error.to_string()) }).await?; } else { shared.output.error(&error,false).await?; } }
    } Ok(())
}
#[cfg(test)]
#[path = "endpoint/input_tests.rs"]
mod input_tests;
