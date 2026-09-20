// SPDX-License-Identifier: GPL-3.0-or-later
use crate::{auth::{Answer, Prompt, PromptKind}, connection::{AuthMethod, Config}, credentials, trust};
use russh::{client, ChannelMsg, keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate}};
use std::{borrow::Cow, ffi::CStr, fmt, future::Future, os::unix::net::UnixStream, path::PathBuf, sync::{Arc, Mutex}, thread::{self, JoinHandle}, time::{Duration, Instant}};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, sync::{oneshot, watch}};

#[derive(Clone, Debug)]
pub struct Failure { pub message: String, pub fatal: bool }
impl fmt::Display for Failure { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.message) } }
impl std::error::Error for Failure {}
impl From<russh::Error> for Failure { fn from(e: russh::Error) -> Self { Self::network(format!("SSH: {e}")) } }
impl From<std::io::Error> for Failure { fn from(e: std::io::Error) -> Self { Self::network(format!("SSH I/O: {e}")) } }
impl Failure {
    fn fatal(message: impl Into<String>) -> Self { Self { message: message.into(), fatal: true } }
    fn network(message: impl Into<String>) -> Self { Self { message: message.into(), fatal: false } }
}
struct Pending { id: u64, sender: oneshot::Sender<Answer> }
struct Status {
    ready: bool, finished: bool, failure: Option<Failure>, next_id: u64,
    prompt: Option<Prompt>, pending: Option<Pending>, cancelled: bool, exit_status: Option<u32>,
}
#[derive(Clone)]
pub struct Control { status: Arc<Mutex<Status>>, cancel: watch::Sender<bool>, size: Option<watch::Sender<(u16, u16, u16, u16)>> }
#[derive(Clone, Debug)]
pub enum SessionRequest {
    Exec { command: String },
    Shell { cols: u16, rows: u16, cell_width: u16, cell_height: u16 },
}
impl Control {
    pub fn resize(&self, cols: u16, rows: u16, cell_width: u16, cell_height: u16) -> Result<(), String> {
        if cols == 0 || rows == 0 || cell_width == 0 || cell_height == 0 { return Err("SSH terminal dimensions must be nonzero".into()); }
        let s = self.status.lock().map_err(|_| "SSH authentication state poisoned")?;
        if s.cancelled || s.finished { return Err("SSH session is closed".into()); }
        self.size.as_ref().ok_or("SSH session has no PTY")?.send((cols, rows, cell_width, cell_height))
            .map_err(|_| "SSH session is closed".into())
    }
    pub fn answer(&self, answer: Answer) -> Result<(), String> {
        let mut s = self.status.lock().map_err(|_| "SSH authentication state poisoned")?;
        if s.cancelled || s.finished || !s.pending.as_ref().is_some_and(|p| p.id == answer.id) {
            return Err("SSH authentication prompt is no longer current".into());
        }
        let pending = s.pending.take().ok_or("SSH authentication prompt expired")?;
        s.prompt = None;
        pending.sender.send(answer).map_err(|_| "SSH authentication prompt expired".into())
    }
    pub fn cancel(&self) {
        if let Ok(mut s) = self.status.lock() { s.cancelled = true; s.pending = None; s.prompt = None; }
        let _ = self.cancel.send(true);
    }
    pub fn prompt(&self) -> Option<Prompt> { self.status.lock().ok()?.prompt.take() }
    pub fn ready(&self) -> bool { self.status.lock().is_ok_and(|s| s.ready) }
    pub fn finished(&self) -> bool { self.status.lock().map_or(true, |s| s.finished) }
    pub fn failure(&self) -> Option<Failure> { self.status.lock().ok()?.failure.clone() }
    pub(crate) fn exit_status(&self) -> Option<u32> { self.status.lock().ok()?.exit_status }
    async fn ask(&self, mut prompt: Prompt) -> Result<Answer, Failure> {
        let (sender, receiver) = oneshot::channel();
        let id = {
            let mut s = self.status.lock().map_err(|_| Failure::fatal("SSH authentication state poisoned"))?;
            if s.cancelled { return Err(Failure::fatal("SSH connection cancelled")); }
            if s.pending.is_some() { return Err(Failure::fatal("Concurrent SSH authentication request")); }
            let id = s.next_id;
            s.next_id = id.checked_add(1).ok_or_else(|| Failure::fatal("SSH prompt IDs exhausted"))?;
            prompt.id = id;
            s.pending = Some(Pending { id, sender }); s.prompt = Some(prompt);
            id
        };
        let response = tokio::time::timeout(Duration::from_secs(300), receiver).await;
        if let Ok(mut s) = self.status.lock() {
            if s.pending.as_ref().is_some_and(|p| p.id == id) { s.pending = None; s.prompt = None; }
        }
        let answer = response.map_err(|_| Failure::fatal("SSH sign-in prompt expired after 5 minutes. Connect again to try a fresh sign-in."))?
            .map_err(|_| Failure::fatal("SSH connection cancelled"))?;
        if !answer.approved { return Err(Failure::fatal("SSH connection cancelled")); }
        Ok(answer)
    }
}
pub struct Transport { pub control: Control, worker: Option<JoinHandle<()>> }
pub struct Pipes { pub stdin: UnixStream, pub stdout: UnixStream, pub stderr: UnixStream }
impl Transport {
    pub fn start(config: Config, connection_label: String, root: PathBuf) -> Result<(Self, Pipes), String> {
        let request = SessionRequest::Exec { command: config.remote_command.clone() };
        Self::start_session(config, connection_label, root, request)
    }
    pub fn start_session(config: Config, connection_label: String, root: PathBuf, request: SessionRequest) -> Result<(Self, Pipes), String> {
        let (size, dimensions) = match &request {
            SessionRequest::Shell { cols, rows, cell_width, cell_height } => {
                if [*cols, *rows, *cell_width, *cell_height].contains(&0) { return Err("SSH terminal dimensions must be nonzero".into()); }
                let (tx, rx) = watch::channel((*cols, *rows, *cell_width, *cell_height));
                (Some(tx), Some(rx))
            },
            SessionRequest::Exec { .. } => (None, None),
        };
        let (stdin, input) = UnixStream::pair().map_err(|e| e.to_string())?;
        let (stdout, output) = UnixStream::pair().map_err(|e| e.to_string())?;
        let (stderr, error) = UnixStream::pair().map_err(|e| e.to_string())?;
        for pipe in [&stdin, &input, &stdout, &output, &stderr, &error] { pipe.set_nonblocking(true).map_err(|e| e.to_string())?; }
        let (cancel, mut cancelled) = watch::channel(false);
        let control = Control { cancel, size, status: Arc::new(Mutex::new(Status { ready: false, finished: false, failure: None,
            next_id: 1, prompt: None, pending: None, cancelled: false, exit_status: None })) };
        let shared = control.clone();
        let worker = thread::Builder::new().name("herdr-ssh".into()).spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
                let result = runtime.block_on(async {
                    tokio::select! {
                        biased;
                        _ = cancelled.changed() => Ok(()),
                        result = connect(config, connection_label, root, shared.clone(), request, dimensions, input, output, error) => result,
                    }
                });
                // DNS resolution can use Tokio's blocking pool. Cancelling its
                // future closes all async transport tasks without waiting for a
                // libc resolver call which cannot itself be interrupted.
                runtime.shutdown_background();
                result
            }));
            let failure = match result { Ok(Ok(())) => None, Ok(Err(e)) => Some(e),
                Err(_) => Some(Failure::fatal("SSH worker panicked")) };
            if let Ok(mut s) = shared.status.lock() { s.finished = true; s.failure = failure; s.pending = None; s.prompt = None; }
        }).map_err(|e| format!("Cannot start SSH worker: {e}"))?;
        Ok((Self { control, worker: Some(worker) }, Pipes { stdin, stdout, stderr }))
    }
    pub fn stop(&mut self) -> Result<(), String> {
        self.control.cancel();
        if let Some(worker) = self.worker.take() { worker.join().map_err(|_| "SSH worker panicked")?; }
        Ok(())
    }
}
impl Drop for Transport { fn drop(&mut self) { let _ = self.stop(); } }

struct Handler { control: Control, path: PathBuf, host: String, port: u16 }
impl client::Handler for Handler {
    type Error = Failure;
    async fn check_server_key(&mut self, offered: &PublicKeyOrCertificate) -> Result<bool, Failure> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = offered else {
            return Err(Failure::fatal("SSH host certificates are not supported; configure a plain host key"));
        };
        let verification = trust::check(&self.path, &self.host, self.port, key).map_err(Failure::fatal)?;
        let (kind, title, detail, previous) = match &verification {
            trust::Verification::Trusted => return Ok(true),
            trust::Verification::Unknown => (PromptKind::NewHost, "Trust this SSH host?", "Verify this fingerprint with the server owner before trusting it.", String::new()),
            trust::Verification::Changed { previous } => (PromptKind::ChangedHost, "SSH host key changed", "This may indicate an impersonation attack. Independently verify the replacement fingerprint before continuing.", previous.join("\n")),
        };
        self.control.ask(Prompt { id: 0, kind, title: title.into(),
            detail: format!("{}:{}\n{detail}", self.host, self.port),
            fingerprint: key.fingerprint(HashAlg::Sha256).to_string(), previous_fingerprint: previous }).await?;
        trust::remember(&self.path, &self.host, self.port, key, &verification).map_err(Failure::fatal)?;
        Ok(true)
    }
}

// Only interactive host verification pauses this network setup deadline. The
// independent prompt timeout and outer cancellation still apply throughout.
async fn handshake<T>(control: &Control, future: impl Future<Output = Result<T, Failure>>) -> Result<T, Failure> {
    tokio::pin!(future);
    let mut remaining = Duration::from_secs(20);
    let mut previous = Instant::now();
    let mut was_prompting = false;
    loop {
        tokio::select! {
            result = &mut future => return result,
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                let now = Instant::now();
                let prompting = control.status.lock().map_err(|_| Failure::fatal("SSH authentication state poisoned"))?.pending.is_some();
                if !prompting && !was_prompting { remaining = remaining.saturating_sub(now.duration_since(previous)); }
                if remaining.is_zero() { return Err(Failure::network("SSH connection handshake timed out")); }
                previous = now; was_prompting = prompting;
            }
        }
    }
}
async fn network<T>(future: impl Future<Output = Result<T, russh::Error>>) -> Result<T, Failure> {
    tokio::time::timeout(Duration::from_secs(20), future).await
        .map_err(|_| Failure::network("SSH operation timed out"))?.map_err(Failure::from)
}
async fn authentication_result(session: &mut client::Handle<Handler>, result: Result<client::AuthResult, Failure>) -> Result<client::AuthResult, Failure> {
    if session.is_closed() {
        // A closed authentication reply queue may appear as AuthFailure or
        // SendError. The terminated session owns the actual disconnect cause.
        return match tokio::time::timeout(Duration::from_secs(1), session).await {
            Ok(completion) => Err(sign_in_closed(completion)),
            Err(_) => Err(sign_in_closed(Ok(()))),
        };
    }
    result.map_err(|error| Failure::fatal(format!("Sign-in did not finish. Connect again to try a fresh sign-in. {}", error.message)))
}
fn sign_in_closed(result: Result<(), Failure>) -> Failure {
    let mut message = String::from("SSH connection closed before sign-in finished. Connect again to try a fresh sign-in.");
    if let Err(error) = result { message.push_str(&format!(" {}", error.message)); }
    Failure::fatal(message)
}
async fn ask_credential(control: &Control, session: &mut client::Handle<Handler>, prompt: Prompt) -> Result<Answer, Failure> {
    // Keep driving the real session while the person types. A server login
    // deadline often arrives as plain EOF, which is not proof of a timeout.
    // The transport's termination path clears the prompt and rejects stale answers.
    tokio::select! {
        biased;
        result = session => Err(sign_in_closed(result)),
        answer = control.ask(prompt) => answer,
    }
}
fn username(config: &Config) -> Result<String, Failure> {
    if let Some(user) = &config.user { return Ok(user.clone()); }
    // Match the prior SSH client's effective OS-user default, not an environment hint.
    let mut buffer = vec![0u8; 16384];
    let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found = std::ptr::null_mut();
    let code = unsafe { libc::getpwuid_r(libc::getuid(), &mut passwd, buffer.as_mut_ptr().cast(), buffer.len(), &mut found) };
    if code != 0 || found.is_null() { return Err(Failure::fatal("Cannot determine local username; set SSH Username in Connections")); }
    unsafe { CStr::from_ptr(passwd.pw_name) }.to_str().map(str::to_owned)
        .map_err(|_| Failure::fatal("Local username is not UTF-8; set SSH Username in Connections"))
}
fn credential_prompt(kind: PromptKind, title: &str, detail: String) -> Prompt {
    Prompt { id: 0, kind, title: title.into(), detail, fingerprint: String::new(), previous_fingerprint: String::new() }
}
async fn connect(config: Config, connection_label: String, root: PathBuf, control: Control, request: SessionRequest,
    mut dimensions: Option<watch::Receiver<(u16, u16, u16, u16)>>, input: UnixStream, output: UnixStream, error: UnixStream) -> Result<(), Failure> {
    let user = username(&config)?;
    let connection_detail = format!("{connection_label}\n{user}@{}:{}", config.host, config.port);
    let mut options = client::Config { nodelay: true, window_size: 256 * 1024, channel_buffer_size: 8,
        keepalive_interval: (config.keepalive != 0).then(|| Duration::from_secs(config.keepalive.into())), ..Default::default() };
    options.preferred.compression = if config.compression {
        Cow::Owned(vec![russh::compression::ZLIB_LEGACY, russh::compression::ZLIB, russh::compression::NONE])
    } else { Cow::Borrowed(&[russh::compression::NONE]) };
    let handler = Handler { control: control.clone(), path: root.join(".ssh/known_hosts"), host: config.host.clone(), port: config.port };
    let mut session = handshake(&control, client::connect(Arc::new(options), (config.host.as_str(), config.port), handler)).await
        .map_err(|error| {
            // Host verification runs inside russh's handshake callback, so the
            // handle cannot yet be monitored while that prompt is displayed.
            // If setup fails after interaction, never restart that interaction
            // automatically or infer that a generic close proves expiry.
            if !error.fatal && control.status.lock().is_ok_and(|s| s.next_id > 1) {
                Failure::fatal(format!("Sign-in did not finish. Connect again to try a fresh sign-in. {}", error.message))
            } else { error }
        })?;
    match config.auth_method {
        AuthMethod::Password => {
            let mut accepted = false;
            // This is after the handshake's trusted-host verification callback.
            let mut passwords = credentials::PasswordStore::load(&root).map_err(Failure::fatal)?;
            let mut rejected_saved = false;
            if let Some(password) = passwords.password(&config.host, config.port, &user) {
                let result = network(session.authenticate_password(&user, password)).await;
                accepted = authentication_result(&mut session, result).await?.success();
                if !accepted {
                    passwords.reject(&config.host, config.port, &user).map_err(Failure::fatal)?;
                    rejected_saved = true;
                }
            }
            for attempt in 0..3 {
                if accepted { break; }
                let detail = format!("{connection_detail}{}", if rejected_saved && attempt == 0 { "\nSaved password rejected and forgotten. Enter your current password." }
                    else if attempt == 0 { "" } else { "\nPassword rejected. Try again." });
                let answer = ask_credential(&control, &mut session, credential_prompt(PromptKind::Password, "SSH password", detail)).await?;
                // russh owns one required String copy; our UI answer remains zeroizing.
                let result = network(session.authenticate_password(&user, answer.secret.as_str())).await;
                let result = authentication_result(&mut session, result).await?;
                if result.success() {
                    if answer.remember_password {
                        passwords.remember(&config.host, config.port, &user, answer.secret).map_err(Failure::fatal)?;
                    }
                    accepted = true; break;
                }
            }
            if !accepted { return Err(Failure::fatal("SSH password rejected after 3 attempts")); }
        }
        AuthMethod::Key => {
            let path = config.identity.as_ref().map(PathBuf::from).or_else(|| {
                ["id_dropbear", "id_ed25519", "id_rsa"].into_iter().map(|name| root.join(".ssh").join(name)).find(|path| path.is_file())
            }).ok_or_else(|| Failure::fatal("Choose or import an SSH key in Connections"))?;
            let metadata = credentials::inspect_private_key(&path, None).map_err(Failure::fatal)?;
            let key = if metadata.encrypted {
                let store = credentials::KeyStore::load(&root);
                let canonical = path.canonicalize().map_err(|e| Failure::fatal(format!("Cannot resolve SSH key: {e}")))?;
                let name = store.entries().iter().find(|entry| entry.path.canonicalize().is_ok_and(|path| path == canonical))
                    .map_or(metadata.name.as_str(), |entry| entry.name.as_str());
                let mut unlocked = None;
                for attempt in 0..3 {
                    let detail = format!("{connection_detail}\nUnlock {name}{}", if attempt == 0 { "" } else { "\nPassphrase rejected or key could not be unlocked. Try again." });
                    let mut prompt = credential_prompt(PromptKind::Passphrase, "Unlock SSH key", detail);
                    prompt.fingerprint = metadata.fingerprint.clone();
                    let answer = ask_credential(&control, &mut session, prompt).await?;
                    let result = credentials::load_private_key(&path, Some(answer.secret.as_str()));
                    drop(answer);
                    if let Ok(key) = result { unlocked = Some(key); break; }
                }
                unlocked.ok_or_else(|| Failure::fatal("SSH key could not be unlocked after 3 attempts"))?
            } else { credentials::load_private_key(&path, None).map_err(Failure::fatal)? };
            let hash = network(session.best_supported_rsa_hash()).await
                .map_err(|error| Failure::fatal(format!("Sign-in did not finish. Connect again to try a fresh sign-in. {}", error.message)))?.flatten();
            let result = network(session.authenticate_publickey(&user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))).await;
            let result = authentication_result(&mut session, result).await?;
            if !result.success() { return Err(Failure::fatal("SSH key rejected. Authorize its public key on the server or choose another key in Connections.")); }
        }
    }
    let mut channel = network(session.channel_open_session()).await?;
    let shell = matches!(&request, SessionRequest::Shell { .. });
    match request {
        SessionRequest::Exec { command } => {
            let command = format!("exec sh -c '{}'", command.replace('\'', "'\\''"));
            network(channel.exec(true, command)).await?;
        },
        SessionRequest::Shell { cols, rows, cell_width, cell_height } => {
            network(channel.request_pty(true, "xterm-256color", cols.into(), rows.into(),
                u32::from(cols) * u32::from(cell_width), u32::from(rows) * u32::from(cell_height), &[])).await?;
            tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    match channel.wait().await {
                        Some(ChannelMsg::Success) => return Ok::<(), Failure>(()),
                        Some(ChannelMsg::WindowAdjusted { .. }) => {},
                        _ => return Err(Failure::fatal("SSH server refused the terminal PTY")),
                    }
                }
            }).await.map_err(|_| Failure::network("SSH PTY request timed out"))??;
            network(channel.request_shell(true)).await?;
        },
    }
    // Readiness requires the server's acceptance, not just a queued request.
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Success) => return Ok::<(), Failure>(()),
                // russh already updates its send window before forwarding this
                // informational event. It is not a reply to our session request.
                Some(ChannelMsg::WindowAdjusted { .. }) => {},
                Some(ChannelMsg::Failure) => return Err(Failure::fatal("SSH server refused the session request")),
                Some(ChannelMsg::Data { .. }) => return Err(Failure::network("SSH stdout data arrived before session acceptance")),
                Some(ChannelMsg::ExtendedData { .. }) => return Err(Failure::network("SSH extended data arrived before session acceptance")),
                Some(ChannelMsg::Eof) => return Err(Failure::network("SSH EOF before session acceptance")),
                Some(ChannelMsg::Close) | None => return Err(Failure::network("SSH channel closed before session acceptance")),
                Some(ChannelMsg::ExitStatus { .. }) => return Err(Failure::network("SSH exit status before session acceptance")),
                Some(ChannelMsg::ExitSignal { .. }) => return Err(Failure::network("SSH exit signal before session acceptance")),
                Some(ChannelMsg::OpenFailure(_)) => return Err(Failure::network("SSH channel open failure before session acceptance")),
                _ => return Err(Failure::network("Unexpected SSH control message before session acceptance")),
            }
        }
    }).await.map_err(|_| Failure::network("SSH session request timed out"))??;
    let mut input = tokio::net::UnixStream::from_std(input)?;
    let mut output = tokio::net::UnixStream::from_std(output)?;
    let mut error = tokio::net::UnixStream::from_std(error)?;
    control.status.lock().map_err(|_| Failure::fatal("SSH authentication state poisoned"))?.ready = true;
    let (mut reader, writer) = channel.split();
    let upload = async {
        let mut bytes = [0u8; 8192];
        loop {
            let n = tokio::select! {
                result = input.read(&mut bytes) => result?,
                size = async {
                    let Some(sizes) = &mut dimensions else {
                        return std::future::pending::<Result<(u16, u16, u16, u16), Failure>>().await;
                    };
                    sizes.changed().await.map_err(|_| Failure::network("SSH resize control closed"))?;
                    let size = *sizes.borrow_and_update();
                    Ok(size)
                } => {
                    let (cols, rows, width, height) = size?;
                    network(writer.window_change(cols.into(), rows.into(), u32::from(cols) * u32::from(width),
                        u32::from(rows) * u32::from(height))).await?;
                    continue;
                },
            };
            if n == 0 { writer.eof().await?; return Ok::<(), Failure>(()); }
            // Independent of stdout pressure; send each admitted chunk exactly once.
            tokio::time::timeout(Duration::from_secs(10), writer.data(&bytes[..n])).await
                .map_err(|_| Failure::network("SSH stopped accepting input"))??;
        }
    };
    let download = async {
        let mut exit = None;
        while let Some(message) = reader.wait().await {
            match message {
                ChannelMsg::Data { data } => output.write_all(&data).await?,
                ChannelMsg::ExtendedData { data, ext: 1 } => {
                    if shell { output.write_all(&data).await?; } else { error.write_all(&data).await?; }
                },
                ChannelMsg::ExtendedData { .. } => return Err(Failure::network("Unsupported SSH extended data stream")),
                ChannelMsg::ExitStatus { exit_status } => {
                    control.status.lock().map_err(|_| Failure::fatal("SSH state poisoned"))?.exit_status = Some(exit_status);
                    exit = Some(exit_status);
                },
                ChannelMsg::ExitSignal { .. } => return Err(Failure::network("SSH session terminated by signal")),
                // SSH EOF ends channel data, not control messages: the remote
                // exit status may still follow before the final channel close.
                ChannelMsg::Eof => {},
                ChannelMsg::Close => break,
                _ => {},
            }
        }
        if exit.is_some_and(|status| status != 0) { return Err(Failure::network(format!("SSH session exited with status {}", exit.unwrap_or_default()))); }
        Ok::<(), Failure>(())
    };
    tokio::pin!(download);
    tokio::select! {
        // A terminated russh session carries the actual protocol/network
        // failure; prefer that over a consequent channel-send error.
        biased;
        result = &mut session => {
            result?;
            // A clean session completion can leave already-received channel
            // data and exit status queued. Drain them before closing adapters.
            download.await
        },
        result = upload => result,
        result = &mut download => result,
    }
}
