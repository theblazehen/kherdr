//! X11 selections on private, pure-Rust connections, never the UI's Xlib connection.
//!
//! Completions and errors run on worker threads. They must enqueue UI work without
//! waiting for the UI. `cancel` only signals; it never joins or calls a completion.

use std::cell::Cell;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt, CreateWindowAux, EventMask, GetPropertyReply,
    PropMode, Property, SelectionNotifyEvent, SelectionRequestEvent, Window,
    WindowClass, SELECTION_NOTIFY_EVENT,
};
use x11rb::protocol::Event;
use x11rb::reexports::x11rb_protocol::{parse_display, xauth};
use x11rb::rust_connection::{DefaultStream, PollMode, RustConnection, Stream};
use x11rb::utils::RawFdContainer;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, CURRENT_TIME, NONE};

/// Both transferred bytes and the decoded UTF-8 result are bounded by this cap.
pub const MAX_BYTES: usize = 512 * 1024;
const DEADLINE: Duration = Duration::from_secs(10);
const TICK: Duration = Duration::from_millis(10);
const WRITE_CHUNK: usize = 16 * 1024;

type Completion = Box<dyn FnOnce(Result<String, String>) + Send>;
type ErrorHandler = Arc<dyn Fn(String) + Send + Sync>;

struct Reader {
    cancel: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

/// At most one reader and one owner thread. The copy queue holds one payload.
pub struct Clipboard {
    copies: mpsc::SyncSender<String>,
    stopped: Arc<AtomicBool>,
    owner: Option<JoinHandle<()>>,
    reader: Mutex<Option<Reader>>,
}

impl Clipboard {
    /// Starts the owner service without opening an X connection on the caller.
    /// `on_error` reports asynchronous copy/ownership/selection-service failures.
    pub fn new(on_error: impl Fn(String) + Send + Sync + 'static) -> Result<Self, String> {
        let (copies, receiver) = mpsc::sync_channel(1);
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let on_error: ErrorHandler = Arc::new(on_error);
        let owner = thread::Builder::new()
            .name("clipboard-owner".into())
            .spawn(move || owner_loop(receiver, stop, on_error))
            .map_err(|e| format!("Cannot start clipboard owner: {e}"))?;
        Ok(Self {
            copies,
            stopped,
            owner: Some(owner),
            reader: Mutex::new(None),
        })
    }

    /// Admits a finite read. Busy, invalid limit, shutdown and spawn failures are
    /// returned immediately. All selection/decoding errors go to `completion`.
    /// A cancelled read still completes with an error; callers must additionally
    /// check their attachment, mode signature and paste epoch before using data.
    pub fn request(
        &self,
        primary: bool,
        limit: usize,
        completion: impl FnOnce(Result<String, String>) + Send + 'static,
    ) -> Result<(), String> {
        if limit == 0 || limit > MAX_BYTES {
            return Err(format!("Clipboard limit must be between 1 and {MAX_BYTES} bytes"));
        }
        if self.stopped.load(Ordering::Acquire) {
            return Err("Clipboard service is stopped".into());
        }
        let mut reader = self.reader.lock().map_err(|_| "Clipboard reader lock poisoned")?;
        if let Some(previous) = reader.as_ref() {
            if !previous.thread.is_finished() {
                return Err("A clipboard read is already pending".into());
            }
        }
        if let Some(previous) = reader.take() {
            previous.thread.join().map_err(|_| "Clipboard completion panicked")?;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let cancellation = cancel.clone();
        let stop = self.stopped.clone();
        let deadline = Instant::now() + DEADLINE;
        let completion: Completion = Box::new(completion);
        let thread = thread::Builder::new()
            .name("clipboard-reader".into())
            .spawn(move || {
                let result = read_selection(primary, limit, deadline, cancellation.clone(), stop.clone());
                // Cancellation can race with the last property reply/decoding.
                let result = if cancellation.load(Ordering::Acquire) || stop.load(Ordering::Acquire) {
                    Err("Clipboard read cancelled".into())
                } else if Instant::now() >= deadline {
                    Err("Clipboard read exceeded 10 seconds".into())
                } else {
                    result
                };
                completion(result);
            })
            .map_err(|e| format!("Cannot start clipboard reader: {e}"))?;
        *reader = Some(Reader { cancel, thread });
        Ok(())
    }

    /// Signals the current request; does not join, wait for X11, or run callbacks.
    pub fn cancel(&self) {
        let reader = self.reader.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(reader) = reader.as_ref() {
            reader.cancel.store(true, Ordering::Release);
        }
    }

    /// Queues real CLIPBOARD and PRIMARY ownership. Success means admission, not
    /// ownership acknowledgement; asynchronous failure is delivered to on_error.
    pub fn copy(&self, text: String) -> Result<(), String> {
        if text.len() > MAX_BYTES {
            return Err(format!("Clipboard copy exceeds {MAX_BYTES} bytes"));
        }
        if text.contains('\0') {
            return Err("Clipboard copy contains NUL".into());
        }
        if self.stopped.load(Ordering::Acquire) {
            return Err("Clipboard service is stopped".into());
        }
        self.copies.try_send(text).map_err(|e| match e {
            mpsc::TrySendError::Full(_) => "Clipboard copy queue is full".into(),
            mpsc::TrySendError::Disconnected(_) => "Clipboard owner has stopped".into(),
        })
    }

    /// Cancels network work and joins both threads, releasing both selections.
    /// Callbacks must never synchronously wait for the UI (see module contract).
    pub fn stop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.cancel();
        if let Some(owner) = self.owner.take() {
            let _ = owner.join();
        }
        let reader = self.reader.get_mut().unwrap_or_else(|e| e.into_inner());
        if let Some(reader) = reader.take() {
            let _ = reader.thread.join();
        }
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        self.stop();
    }
}

/// x11rb's normal Stream::poll waits forever. Bound *all* I/O, including setup,
/// synchronous replies and output backpressure, not just SelectionNotify waits.
#[derive(Debug)]
struct BoundedStream {
    inner: DefaultStream,
    deadline: Cell<Instant>,
    cancel: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl BoundedStream {
    fn check(&self) -> io::Result<()> {
        if self.cancel.load(Ordering::Acquire) || self.stop.load(Ordering::Acquire) {
            return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "Clipboard operation cancelled"));
        }
        if Instant::now() >= self.deadline.get() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "Clipboard operation exceeded 10 seconds"));
        }
        Ok(())
    }
}

impl Stream for BoundedStream {
    fn poll(&self, mode: PollMode) -> io::Result<()> {
        loop {
            self.check()?;
            let mut fd = libc::pollfd {
                fd: self.inner.as_raw_fd(),
                events: (if mode.readable() { libc::POLLIN } else { 0 })
                    | (if mode.writable() { libc::POLLOUT } else { 0 }),
                revents: 0,
            };
            // SAFETY: fd points to one initialized pollfd for the duration of poll.
            let ready = unsafe { libc::poll(&mut fd, 1, 10) };
            if ready > 0 {
                if fd.revents & libc::POLLNVAL != 0 {
                    return Err(io::Error::new(io::ErrorKind::NotConnected, "Invalid X11 socket"));
                }
                return Ok(());
            }
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
        }
    }

    fn read(&self, bytes: &mut [u8], fds: &mut Vec<RawFdContainer>) -> io::Result<usize> {
        self.check()?;
        self.inner.read(bytes, fds)
    }

    fn write(&self, bytes: &[u8], fds: &mut Vec<RawFdContainer>) -> io::Result<usize> {
        self.check()?;
        self.inner.write(bytes, fds)
    }
}

type XConnection = RustConnection<BoundedStream>;

struct Selection {
    connection: XConnection,
    window: Window,
    clipboard: Atom,
    targets: Atom,
    utf8: Atom,
    incr: Atom,
    property: Atom,
}

impl Selection {
    fn connect(deadline: Instant, cancel: Arc<AtomicBool>, stop: Arc<AtomicBool>) -> Result<Self, String> {
        let display = parse_display::parse_display(None).map_err(error)?;
        // The app runs against the Kindle's local server. Do not introduce a
        // blocking DNS/TCP connect into what promises a finite worker lifecycle.
        let path = display.connect_instruction().find_map(|address| match address {
            parse_display::ConnectAddress::Socket(path) => Some(path),
            _ => None,
        }).ok_or("Clipboard requires a local Unix X11 DISPLAY")?;
        let mut last_error = None;
        let mut socket = None;
        for abstract_socket in [true, false] {
            match local_socket(&path, abstract_socket) {
                Ok(stream) => { socket = Some(stream); break; }
                Err(e) => last_error = Some(e),
            }
        }
        let socket = socket.ok_or_else(|| format!("Cannot connect to X11: {}", last_error.unwrap()))?;
        let (inner, (family, address)) = DefaultStream::from_unix_stream(socket).map_err(error)?;
        let (auth_name, auth_data) = match xauth::get_auth(family, &address, display.display) {
            Ok(auth) => auth.unwrap_or_default(),
            // No authority file is normal for the Kindle's local X server.
            // Permission, malformed-file and other errors are not suppressed.
            Err(e) if e.kind() == io::ErrorKind::NotFound => (Vec::new(), Vec::new()),
            Err(e) => return Err(error(e)),
        };
        let stream = BoundedStream { inner, deadline: Cell::new(deadline), cancel, stop };
        stream.check().map_err(error)?;
        let connection = RustConnection::connect_to_stream_with_auth_info(
            stream, display.screen.into(), auth_name, auth_data,
        ).map_err(error)?;
        let screen = &connection.setup().roots[usize::from(display.screen)];
        let window = connection.generate_id().map_err(error)?;
        connection.create_window(
            COPY_DEPTH_FROM_PARENT, window, screen.root, 0, 0, 1, 1, 0,
            WindowClass::INPUT_OUTPUT, 0,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        ).map_err(error)?.check().map_err(error)?;
        let atom = |name: &[u8]| -> Result<Atom, String> {
            Ok(connection.intern_atom(false, name).map_err(error)?.reply().map_err(error)?.atom)
        };
        let clipboard = atom(b"CLIPBOARD")?;
        let targets = atom(b"TARGETS")?;
        let utf8 = atom(b"UTF8_STRING")?;
        let incr = atom(b"INCR")?;
        let property = atom(b"_KHERDR_SELECTION")?;
        Ok(Self { connection, window, clipboard, targets, utf8, incr, property })
    }

    fn next_event(&self) -> Result<Event, String> {
        loop {
            self.connection.stream().check().map_err(error)?;
            if let Some(event) = self.connection.poll_for_event().map_err(error)? {
                if let Event::Error(e) = event {
                    return Err(format!("X11 selection error: {e:?}"));
                }
                return Ok(event);
            }
            thread::sleep(TICK);
        }
    }

    fn property(&self, limit: usize) -> Result<GetPropertyReply, String> {
        // The extra word distinguishes an exact limit from an oversized value.
        let words = (limit / 4 + 1) as u32;
        let reply = self.connection.get_property(
            false, self.window, self.property, AtomEnum::ANY, 0, words,
        ).map_err(error)?.reply().map_err(error)?;
        if reply.bytes_after != 0 || reply.value.len() > limit {
            return Err(format!("Clipboard selection exceeds {limit} bytes"));
        }
        Ok(reply)
    }

    fn delete_property(&self) -> Result<(), String> {
        self.connection.delete_property(self.window, self.property).map_err(error)?.check().map_err(error)?;
        self.connection.flush().map_err(error)
    }
}

/// AF_UNIX connect is nonblocking too: a full listen backlog must not trap stop.
fn local_socket(path: &str, abstract_socket: bool) -> io::Result<UnixStream> {
    // SAFETY: socket has no pointer arguments; returned fd is immediately owned.
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC, 0) };
    if raw < 0 { return Err(io::Error::last_os_error()); }
    // SAFETY: raw is a newly created, uniquely owned descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: all-zero sockaddr_un is valid before filling its fields.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let offset = usize::from(abstract_socket);
    if path.len() + 1 > address.sun_path.len() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "X11 socket path is too long"));
    }
    for (destination, byte) in address.sun_path[offset..].iter_mut().zip(path.bytes()) {
        *destination = byte as libc::c_char;
    }
    let length = std::mem::offset_of!(libc::sockaddr_un, sun_path) + path.len() + 1;
    // SAFETY: address is initialized and length stays within sockaddr_un.
    let connected = unsafe { libc::connect(fd.as_raw_fd(), (&address as *const libc::sockaddr_un).cast(), length as libc::socklen_t) };
    if connected < 0 {
        // AF_UNIX has no TCP handshake: EAGAIN/EINPROGRESS is an explicit setup
        // failure instead of waiting indefinitely for a server listen backlog.
        return Err(io::Error::last_os_error());
    }
    Ok(UnixStream::from(fd))
}

fn error(e: impl std::fmt::Display) -> String { format!("X11 clipboard: {e}") }

fn read_selection(
    primary: bool, limit: usize, deadline: Instant,
    cancel: Arc<AtomicBool>, stop: Arc<AtomicBool>,
) -> Result<String, String> {
    let x = Selection::connect(deadline, cancel, stop)?;
    let selection = if primary { AtomEnum::PRIMARY.into() } else { x.clipboard };
    let owner = x.connection.get_selection_owner(selection).map_err(error)?.reply().map_err(error)?.owner;
    if owner == NONE { return Err("X11 selection has no owner".into()); }
    // Try UTF8_STRING first. Only an explicit refusal allows Latin-1 fallback;
    // timeout, oversize, malformed UTF-8 or NUL must never turn into a new paste.
    for target in [x.utf8, AtomEnum::STRING.into()] {
        x.delete_property()?;
        x.connection.convert_selection(x.window, selection, target, x.property, CURRENT_TIME)
            .map_err(error)?.check().map_err(error)?;
        x.connection.flush().map_err(error)?;
        let notify = loop {
            if let Event::SelectionNotify(event) = x.next_event()? {
                if event.requestor == x.window && event.selection == selection && event.target == target {
                    break event;
                }
            }
        };
        if notify.property == NONE {
            if target == x.utf8 {
                let current = x.connection.get_selection_owner(selection).map_err(error)?.reply().map_err(error)?.owner;
                if current != owner { return Err("Clipboard owner changed during conversion".into()); }
                continue;
            }
            return Err("Selection owner refused UTF8_STRING and STRING".into());
        }
        if notify.property != x.property { return Err("SelectionNotify named an unexpected property".into()); }
        // INCR's header is four bytes even if the caller's text limit is smaller.
        let first = x.property(limit.max(4))?;
        let bytes = if first.type_ == x.incr {
            if first.format != 32 || first.value.len() != 4 {
                return Err("Malformed INCR selection header".into());
            }
            let advertised = first.value32().and_then(|mut values| values.next())
                .ok_or("Missing INCR selection size")? as usize;
            if advertised > limit { return Err(format!("INCR selection advertises {advertised} bytes; limit is {limit}")); }
            x.delete_property()?;
            let mut bytes = Vec::new();
            loop {
                match x.next_event()? {
                    Event::PropertyNotify(event) if event.window == x.window && event.atom == x.property && event.state == Property::NEW_VALUE => {}
                    _ => continue,
                }
                let chunk = x.property(limit - bytes.len())?;
                if chunk.type_ != target || chunk.format != 8 {
                    return Err("INCR chunk has an unexpected text type or format".into());
                }
                let done = chunk.value.is_empty();
                bytes.extend_from_slice(&chunk.value);
                x.delete_property()?;
                if done { break; }
            }
            if bytes.len() < advertised {
                return Err("INCR selection ended below its advertised minimum size".into());
            }
            bytes
        } else {
            if first.type_ != target || first.format != 8 {
                return Err("Selection has an unexpected text type or format".into());
            }
            if first.value.len() > limit { return Err(format!("Clipboard selection exceeds {limit} bytes")); }
            x.delete_property()?;
            first.value
        };
        if bytes.contains(&0) { return Err("Clipboard selection contains NUL".into()); }
        if target == x.utf8 {
            return String::from_utf8(bytes).map_err(|e| format!("Clipboard selection is not valid UTF-8: {e}"));
        }
        // ICCCM STRING is Latin-1, not UTF-8 and not the process locale.
        let expanded = bytes.len() + bytes.iter().filter(|&&byte| byte >= 128).count();
        if expanded > limit { return Err(format!("Decoded STRING selection exceeds {limit} UTF-8 bytes")); }
        let mut text = String::with_capacity(expanded);
        for byte in bytes { text.push(char::from(byte)); }
        return Ok(text);
    }
    Err("No supported selection target".into())
}

struct Owner {
    x: Selection,
    text: String,
    latin1: Option<Vec<u8>>,
    clipboard: bool,
    primary: bool,
}

impl Owner {
    fn acquire(text: String, stop: Arc<AtomicBool>) -> Result<Self, String> {
        let x = Selection::connect(Instant::now() + DEADLINE, Arc::new(AtomicBool::new(false)), stop)?;
        for selection in [x.clipboard, AtomEnum::PRIMARY.into()] {
            x.connection.set_selection_owner(x.window, selection, CURRENT_TIME).map_err(error)?.check().map_err(error)?;
            if x.connection.get_selection_owner(selection).map_err(error)?.reply().map_err(error)?.owner != x.window {
                return Err("Cannot acquire X11 clipboard selection".into());
            }
        }
        let latin1 = text.chars().map(|c| u8::try_from(u32::from(c))).collect::<Result<Vec<_>, _>>().ok();
        Ok(Self { x, text, latin1, clipboard: true, primary: true })
    }

    fn notify(&self, request: &SelectionRequestEvent, property: Atom) -> Result<(), String> {
        let event = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT, sequence: 0, time: request.time,
            requestor: request.requestor, selection: request.selection, target: request.target, property,
        };
        self.x.connection.send_event(false, request.requestor, EventMask::NO_EVENT, event)
            .map_err(error)?.check().map_err(error)?;
        self.x.connection.flush().map_err(error)
    }

    fn serve(&self, request: SelectionRequestEvent) -> Result<(), String> {
        let x = &self.x;
        let owns = (request.selection == x.clipboard && self.clipboard)
            || (request.selection == u32::from(AtomEnum::PRIMARY) && self.primary);
        if request.owner != x.window || !owns { return self.notify(&request, NONE); }
        let property = if request.property == NONE { request.target } else { request.property };
        let result = (|| {
            if request.target == x.targets {
                let offered = [x.targets, x.utf8, AtomEnum::STRING.into()];
                let offered = &offered[..if self.latin1.is_some() { 3 } else { 2 }];
                x.connection.change_property32(PropMode::REPLACE, request.requestor, property, AtomEnum::ATOM, offered)
                    .map_err(error)?.check().map_err(error)?;
            } else {
                let bytes = if request.target == x.utf8 {
                    self.text.as_bytes()
                } else if request.target == u32::from(AtomEnum::STRING) {
                    match self.latin1.as_deref() {
                        Some(bytes) => bytes,
                        None => return self.notify(&request, NONE),
                    }
                } else { return self.notify(&request, NONE); };
                // Like handoff/kindle-xinput.c, append bounded core requests and
                // announce only after *every* append has been acknowledged. No
                // BIG-REQUESTS dependency or partial successful paste on error.
                let maximum = usize::from(x.connection.setup().maximum_request_length)
                    .checked_sub(6).ok_or("X11 request limit cannot hold a text property")? * 4;
                let chunk = maximum.min(WRITE_CHUNK);
                if chunk == 0 { return Err("X11 text property request limit is zero".into()); }
                if bytes.is_empty() {
                    x.connection.change_property8(PropMode::REPLACE, request.requestor, property, request.target, &[])
                        .map_err(error)?.check().map_err(error)?;
                } else {
                    for (index, bytes) in bytes.chunks(chunk).enumerate() {
                        x.connection.change_property8(
                            if index == 0 { PropMode::REPLACE } else { PropMode::APPEND },
                            request.requestor, property, request.target, bytes,
                        ).map_err(error)?.check().map_err(error)?;
                    }
                }
            }
            self.notify(&request, property)
        })();
        if let Err(original) = result {
            let cleanup = x.connection.delete_property(request.requestor, property)
                .map_err(error).and_then(|cookie| cookie.check().map_err(error));
            let notify = self.notify(&request, NONE);
            return Err(match (cleanup, notify) {
                (Ok(()), Ok(())) => original,
                (cleanup, notify) => format!("{original}; property cleanup: {cleanup:?}; refusal: {notify:?}"),
            });
        }
        Ok(())
    }
}

fn owner_loop(receiver: mpsc::Receiver<String>, stop: Arc<AtomicBool>, on_error: ErrorHandler) {
    let mut owner: Option<Owner> = None;
    while !stop.load(Ordering::Acquire) {
        match receiver.recv_timeout(TICK) {
            Ok(text) => match Owner::acquire(text, stop.clone()) {
                Ok(replacement) => owner = Some(replacement),
                Err(e) => if !stop.load(Ordering::Acquire) { on_error(e); },
            },
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        let Some(active) = owner.as_mut() else { continue; };
        active.x.connection.stream().deadline.set(Instant::now() + DEADLINE);
        let mut failed = false;
        // Event floods cannot starve copy commands or shutdown indefinitely.
        for _ in 0..64 {
            if stop.load(Ordering::Acquire) { break; }
            let event = match active.x.connection.poll_for_event() {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(e) => { on_error(error(e)); failed = true; break; }
            };
            match event {
                Event::SelectionRequest(request) => {
                    if let Err(e) = active.serve(request) {
                        if !stop.load(Ordering::Acquire) { on_error(e); }
                        failed = true;
                        break;
                    }
                }
                Event::SelectionClear(event) if event.owner == active.x.window => {
                    if event.selection == active.x.clipboard { active.clipboard = false; }
                    if event.selection == u32::from(AtomEnum::PRIMARY) { active.primary = false; }
                }
                Event::Error(e) => { on_error(format!("X11 clipboard owner error: {e:?}")); failed = true; break; }
                _ => {}
            }
        }
        if failed || (!active.clipboard && !active.primary) { owner = None; }
    }
    // Closing the private connection destroys the owner window and all selection
    // ownership, including on protocol error; no flushing/joining X11 is needed.
}
