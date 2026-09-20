// SPDX-License-Identifier: GPL-3.0-or-later
use crate::{auth, connection::Profile, ssh};
use std::{io::{self, Read}, path::PathBuf, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}, thread::{self, JoinHandle}, time::{Duration, Instant}};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action { Inspect, Start }
#[derive(Debug)]
pub enum Event {
    Authentication(auth::Prompt),
    Inspected { version: String, binary: String, session_running: bool, sessions: Vec<String>, start_allowed: bool, detail: String },
    Completed { binary: String, detail: String },
    Failed { message: String },
}
#[derive(serde::Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Inspected { version: String, binary: String, session_running: bool, sessions: Vec<String>, start_allowed: bool, detail: String },
    Completed { binary: String, detail: String },
    Failed { message: String },
}
pub struct Setup {
    control: ssh::Control,
    cancelled: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<String>>>,
    worker: Option<JoinHandle<()>>,
    cancel_notify: Option<Box<dyn FnOnce() + Send>>,
}
impl Setup {
    /// Start may only be called after explicit UI confirmation.
    /// notify must use bounded admission; cancel_notify must unblock it before join.
    pub fn start(profile: Profile, auth_root: PathBuf, action: Action,
        notify: impl Fn(Event) -> Result<(), String> + Send + 'static,
        cancel_notify: impl FnOnce() + Send + 'static) -> Result<Self, String> {
        fn quote(value: &str) -> String { format!("'{}'", value.replace('\'', "'\\''")) }
        if profile.herdr_binary.contains('\0') || profile.herdr_session.contains('\0') { return Err("Setup arguments contain NUL".into()); }
        let action_name = match action { Action::Inspect => "inspect", Action::Start => "start" };
        let command = format!("python3 -c {} {} {} {}", quote(include_str!("../tools/setup-herdr.py")), quote(action_name), quote(&profile.herdr_binary), quote(&profile.herdr_session));
        let (mut transport, mut pipes) = ssh::Transport::start_session(profile.config, profile.name, auth_root, ssh::SessionRequest::Exec { command })?;
        let control = transport.control.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancellation = cancelled.clone();
        let failure = Arc::new(Mutex::new(None));
        let failed = failure.clone();
        let worker = thread::Builder::new().name("herdr-setup".into()).spawn(move || {
            // Keep remote stdin open until this operation's owner ends. Python
            // observes EOF as cancellation, including a lost SSH connection.
            let _input_owner = pipes.stdin;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), String> {
                let mut output = Vec::new();
                let mut errors = Vec::new();
                let mut eof = [false; 2];
                let mut operation_started = None;
                let auth_deadline = Instant::now() + Duration::from_secs(360);
                loop {
                    if cancellation.load(Ordering::Acquire) { return Ok(()); }
                    if let Some(prompt) = transport.control.prompt() { notify(Event::Authentication(prompt))?; }
                    if transport.control.ready() && operation_started.is_none() { operation_started = Some(Instant::now()); }
                    if operation_started.is_some_and(|time| time.elapsed() > Duration::from_secs(180))
                        || (operation_started.is_none() && Instant::now() > auth_deadline) { return Err("Setup timed out; inspect host state before retrying a mutation".into()); }
                    for (index, (stream, destination)) in [(&mut pipes.stdout, &mut output), (&mut pipes.stderr, &mut errors)].into_iter().enumerate() {
                        if eof[index] { continue; }
                        let mut bytes = [0u8; 4096];
                        match stream.read(&mut bytes) {
                            Ok(0) => eof[index] = true,
                            Ok(count) => {
                                if destination.len() + count > 65536 { return Err("Setup output exceeded 64 KiB".into()); }
                                destination.extend_from_slice(&bytes[..count]);
                            }
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock || error.kind() == io::ErrorKind::Interrupted => {},
                            Err(error) => return Err(format!("Setup output: {error}")),
                        }
                    }
                    if eof.iter().all(|eof| *eof) && transport.control.finished() { break; }
                    thread::sleep(Duration::from_millis(20));
                }
                let reply: Reply = serde_json::from_slice(&output).map_err(|_| {
                    transport.control.failure().map(|failure| failure.message).unwrap_or_else(|| format!("Setup returned no valid result. Python 3 is required. {}", String::from_utf8_lossy(&errors)))
                })?;
                let event = match reply {
                    Reply::Inspected { version, binary, session_running, sessions, start_allowed, detail } if action == Action::Inspect => Event::Inspected { version, binary, session_running, sessions, start_allowed, detail },
                    Reply::Completed { binary, detail } if action != Action::Inspect => Event::Completed { binary, detail },
                    Reply::Failed { message } => return Err(message),
                    _ => return Err("Setup response did not match the approved action".into()),
                };
                if let Some(error) = transport.control.failure() { return Err(error.message); }
                notify(event)
            }));
            let error = match result { Ok(Ok(())) => None, Ok(Err(error)) => Some(error), Err(_) => Some("Setup worker panicked".into()) };
            if let Some(message) = error {
                if let Ok(mut failure) = failed.lock() { *failure = Some(message.clone()); }
                if !cancellation.load(Ordering::Acquire) { let _ = notify(Event::Failed { message }); }
            }
            let _ = transport.stop();
        }).map_err(|error| format!("Cannot start setup worker: {error}"))?;
        Ok(Self { control, cancelled, failure, worker: Some(worker), cancel_notify: Some(Box::new(cancel_notify)) })
    }
    pub fn answer_auth(&self, answer: auth::Answer) -> Result<(), String> { self.control.answer(answer) }
    pub fn failure(&self) -> Option<String> { self.failure.lock().ok()?.clone() }
    pub fn stop(&mut self) -> Result<(), String> {
        self.cancelled.store(true, Ordering::Release);
        self.control.cancel();
        let cancelled = self.cancel_notify.take().map(|cancel| std::panic::catch_unwind(std::panic::AssertUnwindSafe(cancel)));
        let joined = self.worker.take().map(JoinHandle::join);
        if cancelled.is_some_and(|result| result.is_err()) { return Err("Setup delivery cancellation panicked".into()); }
        if joined.is_some_and(|result| result.is_err()) { return Err("Setup worker panicked".into()); }
        Ok(())
    }
}
impl Drop for Setup { fn drop(&mut self) { let _ = self.stop(); } }
