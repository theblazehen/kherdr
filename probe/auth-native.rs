// Real production modules; see auth-server.py --help. No replacement transport.
#![allow(dead_code)]
#[path = "../src/atomic_file.rs"] mod atomic_file;
#[path = "../src/auth.rs"] mod auth;
#[path = "../src/connection.rs"] mod connection;
#[path = "../src/credentials.rs"] mod credentials;
#[path = "../src/trust.rs"] mod trust;
#[path = "../src/ssh.rs"] mod ssh;
#[path = "../src/client.rs"] mod client;
#[path = "../src/endpoint.rs"] mod endpoint;
use std::{io::{Read, Write}, path::PathBuf, time::{Duration, Instant}, thread};
use serde::Deserialize;
use serde_json::{Value, json};
use zeroize::Zeroizing;

#[derive(Deserialize)]
struct Step { kind: String, action: String, #[serde(default)] secret: String, #[serde(default)] remember_password: bool }
#[derive(Deserialize)]
struct Request {
    operation: String, root: PathBuf,
    connection_label: Option<String>,
    #[serde(default)] passphrase: String,
    legacy: Option<PathBuf>, config: Option<connection::Config>,
    key_path: Option<PathBuf>,
    #[serde(default)] steps: Vec<Step>,
    #[serde(default)] expect: String,
}
fn require(ok: bool, message: &str) -> Result<(), String> {
    if ok { Ok(()) } else { Err(message.into()) }
}
fn drain(pipe: &mut std::os::unix::net::UnixStream, output: &mut Vec<u8>) -> Result<(), String> {
    let mut bytes = [0; 8192];
    loop { match pipe.read(&mut bytes) {
        Ok(0) => break,
        Ok(n) => { output.extend_from_slice(&bytes[..n]); require(output.len() <= 2_000_000, "stream exceeded bound")?; },
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
        Err(e) => return Err(e.to_string()),
    }}
    Ok(())
}
fn run(mut request: Request) -> Result<Value, String> {
    std::fs::create_dir_all(&request.root).map_err(|e| e.to_string())?;
    if request.operation == "fifo-key" {
        use std::os::unix::fs::FileTypeExt;
        let path = request.key_path.ok_or("missing FIFO key path")?;
        require(std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?.file_type().is_fifo(),
                "FIFO probe requires an actual named pipe")?;
        let started = Instant::now();
        let rejected = credentials::load_private_key(&path, None).is_err();
        let elapsed = started.elapsed();
        require(rejected, "FIFO private key was not rejected")?;
        require(elapsed <= Duration::from_secs(1), "FIFO private-key rejection exceeded one second")?;
        return Ok(json!({"operation":"fifo-key", "rejected":rejected,
                         "elapsed_ms":elapsed.as_secs_f64() * 1000.0}));
    }
    if request.operation == "prepare" {
        let pass = Zeroizing::new(std::mem::take(&mut request.passphrase));
        let mut store = credentials::KeyStore::load(&request.root);
        let plain = store.generate("generated", None)?;
        let encrypted = store.generate("encrypted", Some(&pass))?;
        require(credentials::load_private_key(&encrypted.path, Some("deliberately-wrong")).is_err(), "wrong passphrase accepted")?;
        let imported = store.import_file("encrypted imported", &encrypted.path, Some(&pass))?;
        require(imported.fingerprint == encrypted.fingerprint && imported.encrypted, "encrypted import changed key or encryption")?;
        let legacy = store.import_file("legacy PEM imported", &request.legacy.ok_or("missing legacy key")?, None)?;
        let reloaded = credentials::KeyStore::load(&request.root);
        require(reloaded.error.is_none() && reloaded.entries().len() == 4, "key library did not persist")?;
        return Ok(json!({"keys": [plain, encrypted, imported, legacy]}));
    }
    if request.operation == "client-waiting-closed" {
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let queued = events.clone();
        let mut client = client::Client::start(request.config.ok_or("missing config")?,
            request.connection_label.unwrap_or_else(|| "Auth probe".into()), request.root,
            move |event| { queued.lock().map_err(|_| "probe event queue poisoned")?.push(event); Ok(()) }, || {})?;
        let started = Instant::now();
        // No GUI events are processed: the watchdog must retain fatality even
        // when the separately queued Event::Error has not been handled.
        let failure = loop {
            if let Some(failure) = client.failure() { break failure; }
            require(started.elapsed() < Duration::from_secs(15), "client watchdog did not observe login closure")?;
            thread::sleep(Duration::from_millis(10));
        };
        require(failure.fatal, &format!("client watchdog lost fatal authentication disposition before GUI delivery: {failure:?}"))?;
        let stopped = client.stop();
        require(stopped.is_err(), "client stop lost the original authentication error")?;
        let events = events.lock().map_err(|_| "probe event queue poisoned")?;
        let mut prompts = Vec::new();
        for event in events.iter() {
            match event {
                client::Event::Authentication(prompt) => {
                    require(matches!(prompt.kind, auth::PromptKind::Password), "unexpected client prompt")?;
                    require(client.answer_auth(auth::Answer { id: prompt.id, approved: true, remember_password: false, secret: Zeroizing::new("stale-answer".into()) }).is_err(), "client accepted credentials after closure")?;
                    prompts.push(json!({"kind":"Password", "detail":prompt.detail}));
                },
                client::Event::Ready => return Err("client became ready without credentials".into()),
                _ => {},
            }
        }
        require(prompts.len() == 1, "client did not queue exactly one unanswered credential prompt")?;
        return Ok(json!({"prompts":prompts, "fatal":failure.fatal, "failure_message":failure.message,
            "gui_events_processed":0, "waiting_closed_elapsed_ms":started.elapsed().as_secs_f64() * 1000.0}));
    }
    require(request.operation == "attempt", "unknown operation")?;
    if request.expect == "endpoint" {
        let (sender, receiver) = std::sync::mpsc::sync_channel(64);
        let mut endpoint = client::Client::start(request.config.ok_or("missing config")?,
            request.connection_label.unwrap_or_else(|| "Auth endpoint probe".into()), request.root,
            move |event| sender.try_send(event).map_err(|e| e.to_string()), || {})?;
        let mut steps = request.steps.into_iter();
        let mut prompts = Vec::new();
        let mut surface_bytes = 0;
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            let event = receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())).map_err(|e| e.to_string())?;
            match event {
                client::Event::Authentication(prompt) => {
                    let kind = format!("{:?}", prompt.kind);
                    let step = steps.next().ok_or("unexpected endpoint authentication prompt")?;
                    require(step.kind == kind && step.action == "approve", "unexpected endpoint authentication step")?;
                    prompts.push(json!({"kind":kind,"detail":prompt.detail,"fingerprint":prompt.fingerprint}));
                    endpoint.answer_auth(auth::Answer { id:prompt.id, approved:true, remember_password:step.remember_password,
                        secret:Zeroizing::new(step.secret) })?;
                },
                client::Event::Ready => endpoint.open(80,24,8,16)?,
                client::Event::Frame { bytes, complete, .. } => {
                    surface_bytes += bytes.len();
                    if !complete { continue; }
                    require(steps.next().is_none(), "expected endpoint prompt never appeared")?;
                    endpoint.stop()?;
                    return Ok(json!({"ready":true,"native_surface_bytes":surface_bytes,"prompts":prompts}));
                },
                client::Event::Error { message, .. } | client::Event::Unavailable(message) | client::Event::Disconnected(message) => return Err(message),
                _ => {},
            }
        }
    }
    let (mut transport, mut pipes) = ssh::Transport::start(request.config.ok_or("missing config")?, request.connection_label.unwrap_or_else(|| "Auth probe".into()), request.root)?;
    let mut prompts = Vec::new();
    let mut steps = request.steps.into_iter();
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut stdout = Vec::new(); let mut stderr = Vec::new();
    let mut cancelled = false;
    let mut cancel_started = None;
    let mut waiting = None;
    let mut ready = false;
    loop {
        require(Instant::now() < deadline, "attempt timed out")?;
        require(waiting.is_none_or(|(_, started): (u64, Instant)| started.elapsed() < Duration::from_secs(15)), "closed transport left credential prompt waiting")?;
        if let Some(prompt) = transport.control.prompt() {
            let kind = format!("{:?}", prompt.kind);
            let mut step = steps.next().ok_or("unexpected authentication prompt")?;
            require(step.kind == kind, "wrong authentication prompt kind")?;
            prompts.push(json!({"kind":kind,"detail":prompt.detail,"fingerprint":prompt.fingerprint,"previous_fingerprint":prompt.previous_fingerprint}));
            if step.action == "wait-closed" {
                require(waiting.is_none(), "second prompt appeared while awaiting closure")?;
                waiting = Some((prompt.id, Instant::now()));
            } else if step.action == "cancel" {
                cancel_started = Some(Instant::now());
                transport.control.cancel(); cancelled = true;
                require(transport.control.answer(auth::Answer { id: prompt.id, approved: true, remember_password: false, secret: Zeroizing::new(String::new()) }).is_err(), "stale answer accepted after cancellation")?;
            } else {
                require(step.action == "approve" || step.action == "reject", "invalid prompt action")?;
                transport.control.answer(auth::Answer { id: prompt.id, approved: step.action == "approve", remember_password: step.remember_password, secret: Zeroizing::new(std::mem::take(&mut step.secret)) })?;
            }
        }
        ready |= transport.control.ready();
        drain(&mut pipes.stdout, &mut stdout)?; drain(&mut pipes.stderr, &mut stderr)?;
        if transport.control.finished() {
            ready |= transport.control.ready();
            drain(&mut pipes.stdout, &mut stdout)?;
            drain(&mut pipes.stderr, &mut stderr)?;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let cancel_elapsed = cancel_started.map(|started| started.elapsed());
    require(steps.next().is_none(), "expected prompt never appeared")?;
    let failure = transport.control.failure();
    match request.expect.as_str() {
        "success" => { require(ready && failure.is_none(), &format!("SSH exec did not succeed: {failure:?}"))?; require(stdout == b"AUTH_NATIVE_STDOUT\n" && stderr == b"AUTH_NATIVE_STDERR\n", "SSH streams did not match real command output")?; },
        "failure" => { require(!ready && failure.as_ref().is_some_and(|f| f.fatal), "expected fatal auth rejection before exec")?; },
        "waiting-closed" => {
            let (id, _) = waiting.ok_or("credential wait was never reached")?;
            require(!ready && transport.control.finished() && failure.as_ref().is_some_and(|f| f.fatal), "session closure did not stop sign-in with a fatal failure")?;
            require(transport.control.prompt().is_none(), "closed transport retained a prompt")?;
            require(transport.control.answer(auth::Answer { id, approved: true, remember_password: false, secret: Zeroizing::new("stale-answer".into()) }).is_err(), "closed transport accepted stale credentials")?;
            require(stdout.is_empty() && stderr.is_empty(), "credential waiting unexpectedly executed the command")?;
        },
        "exit-failure" => {
            require(ready && failure.as_ref().is_some_and(|f| !f.fatal),
                &format!("nonzero remote exit was lost: {failure:?}"))?;
            require(transport.control.exit_status() == Some(7), "remote exit status was not preserved")?;
            require(stdout.is_empty() && stderr.is_empty(), "closed-stream command unexpectedly emitted output")?;
        },
        "cancel" => {
            require(cancelled && !ready && transport.control.finished(), "prompt cancellation did not finish before exec")?;
            require(cancel_elapsed.is_some_and(|elapsed| elapsed <= Duration::from_secs(1)), "prompt cancellation exceeded one second")?;
        },
        _ => return Err("invalid expected outcome".into()),
    }
    transport.stop()?;
    Ok(json!({"ready":ready,"finished":transport.control.finished(),"prompts":prompts,"fatal":failure.as_ref().map(|f| f.fatal),"stdout_bytes":stdout.len(),"stderr_bytes":stderr.len(),
        "failure_message":failure.as_ref().map(|f| &f.message),
        "waiting_closed_elapsed_ms":waiting.map(|(_, started)| started.elapsed().as_secs_f64() * 1000.0),
        "cancel_elapsed_ms":cancel_elapsed.map(|elapsed| elapsed.as_secs_f64() * 1000.0),
        "remote_exit_status":transport.control.exit_status()}))
}
fn main() {
    let mut input = Zeroizing::new(String::new());
    let result = std::io::stdin().take(262145).read_to_string(&mut input)
        .map_err(|e| e.to_string()).and_then(|_| {
            require(input.len() <= 262144, "request too large")?;
            let request: Request = serde_json::from_str(&input).map_err(|_| "invalid request".to_string())?;
            run(request)
        });
    match result {
        Ok(value) => { println!("{value}"); },
        Err(error) => { let _ = writeln!(std::io::stderr(), "AUTH_NATIVE_FAILED: {error}"); std::process::exit(1); },
    }
}
