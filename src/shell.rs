// SPDX-License-Identifier: GPL-3.0-or-later
use crate::{auth, connection::Profile, ssh};
use serde::{Deserialize, Serialize};
use std::{collections::VecDeque, fs::{self, File, OpenOptions}, io::{self, Read, Write}, os::unix::{fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt, FileTypeExt}, io::{AsRawFd, FromRawFd}, net::{UnixListener, UnixStream}}, path::{Path, PathBuf}, sync::{Arc, Mutex}, thread::{self, JoinHandle}, time::{Duration, Instant}};
use zeroize::Zeroizing;
use std::os::unix::ffi::OsStrExt;

const LIMIT: usize = 128 * 1024;
const SECRET_LIMIT: usize = 16 * 1024;
const DEADLINE: Duration = Duration::from_secs(10);
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata { pub launch_id: String, pub profile: Profile, pub pane_id: String, pub terminal_id: String }
#[derive(Clone, Debug)]
pub struct Prepared { pub launch_id: String, pub descriptor: PathBuf, pub profile: Profile, pub metadata: Option<Metadata> }
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Descriptor { launch_id: String, profile: Profile, auth_root: PathBuf, api_socket: PathBuf }
#[derive(Debug)]
pub enum Event { Attached(Metadata), Authentication(auth::Prompt), Ready, Closed { message: String, fatal: bool }, ControlLost(String) }
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum Notice {
    Attached { metadata: Metadata },
    Authentication { id: u64, kind: u8, title: String, detail: String, fingerprint: String, previous_fingerprint: String },
    Ready,
    Closed { message: String, fatal: bool },
}
fn private(path: &Path, directory: bool) -> Result<(), String> {
    let m = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o077 != 0 || m.file_type().is_symlink()
        || (directory && !m.is_dir()) { return Err("SSH pane path is not private and owned".into()); }
    Ok(())
}
fn load<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    private(path, false)?;
    let mut bytes = Vec::new();
    let file = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path).map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() { return Err("SSH pane descriptor is not a regular file".into()); }
    file.take((LIMIT + 1) as u64).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() > LIMIT { return Err("SSH pane descriptor exceeds limit".into()); }
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}
fn save<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() > LIMIT { return Err("SSH pane metadata exceeds limit".into()); }
    let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(path).map_err(|e| e.to_string())?;
    file.write_all(&bytes).map_err(|e| e.to_string())
}
fn directory(descriptor: &Path) -> Result<&Path, String> {
    let dir = descriptor.parent().ok_or("SSH pane descriptor has no directory")?;
    private(dir, true)?;
    Ok(dir)
}
fn descriptor(path: &Path) -> Result<Descriptor, String> {
    let dir = directory(path)?;
    let d: Descriptor = load(path)?;
    if d.launch_id.len() != 32 || !d.launch_id.bytes().all(|b| b.is_ascii_hexdigit())
        || dir.file_name().and_then(|s| s.to_str()) != Some(d.launch_id.as_str())
        || !d.auth_root.is_absolute() || !d.api_socket.is_absolute() {
        return Err("Invalid SSH launch identity".into());
    }
    Ok(d)
}
pub fn prepare(root: &Path, profile: Profile, auth_root: PathBuf, api_socket: PathBuf) -> Result<Prepared, String> {
    if !root.is_absolute() || !root.starts_with("/tmp") { return Err("SSH pane root must be under /tmp".into()); }
    fs::create_dir_all(&auth_root).map_err(|e| e.to_string())?;
    let auth_root = auth_root.canonicalize().map_err(|e| e.to_string())?;
    let api_socket = std::path::absolute(api_socket).map_err(|e| e.to_string())?;
    if !root.exists() { fs::DirBuilder::new().mode(0o700).create(root).map_err(|e| e.to_string())?; }
    private(root, true)?;
    let mut random = [0u8; 16];
    File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut random)).map_err(|e| e.to_string())?;
    let launch_id: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let dir = root.join(&launch_id);
    fs::DirBuilder::new().mode(0o700).create(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("descriptor.json");
    save(&path, &Descriptor { launch_id: launch_id.clone(), profile: profile.clone(), auth_root, api_socket })?;
    Ok(Prepared { launch_id, descriptor: path, profile, metadata: None })
}
fn same_metadata(a: &Metadata, b: &Metadata) -> bool {
    a.launch_id == b.launch_id && a.pane_id == b.pane_id && a.terminal_id == b.terminal_id
        && serde_json::to_value(&a.profile).ok() == serde_json::to_value(&b.profile).ok()
}
fn live(d: &Descriptor, m: &Metadata) -> Result<(), String> {
    if m.launch_id != d.launch_id || serde_json::to_value(&m.profile).map_err(|e| e.to_string())? != serde_json::to_value(&d.profile).map_err(|e| e.to_string())? {
        return Err("SSH pane launch metadata mismatch".into());
    }
    let result = crate::endpoint::api_request(&d.api_socket, "pane.get", serde_json::json!({"pane_id":m.pane_id}))?;
    if result["pane"]["terminal_id"].as_str() != Some(m.terminal_id.as_str()) || result["pane"]["pane_id"].as_str() != Some(m.pane_id.as_str()) {
        return Err("SSH pane runtime is no longer current".into());
    }
    Ok(())
}
pub fn discover(root: &Path) -> Result<Vec<Prepared>, String> {
    if !root.exists() { return Ok(Vec::new()); }
    private(root, true)?;
    let mut found = Vec::new();
    for entry in fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().join("descriptor.json");
        let Ok(d) = descriptor(&path) else { continue };
        let Ok(m) = load::<Metadata>(&entry.path().join("metadata.json")) else { continue };
        let socket = entry.path().join("control.sock");
        if private(&socket, false).is_err() || !fs::symlink_metadata(&socket).is_ok_and(|m| m.file_type().is_socket()) || live(&d, &m).is_err() { continue; }
        found.push(Prepared { launch_id: d.launch_id, descriptor: path, profile: d.profile, metadata: Some(m) });
    }
    Ok(found)
}
fn poll(fds: &mut [libc::pollfd], timeout: i32) -> Result<(), String> {
    let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
    if n < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted { return Err(io::Error::last_os_error().to_string()); }
    Ok(())
}
fn fd(fd: i32, events: i16) -> libc::pollfd { libc::pollfd { fd, events, revents: 0 } }
fn peer(socket: &UnixStream) -> Result<(), String> {
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of_val(&credentials) as libc::socklen_t;
    if unsafe { libc::getsockopt(socket.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&mut credentials as *mut libc::ucred).cast(), &mut len) } != 0
        || credentials.uid != unsafe { libc::geteuid() } { return Err("Foreign SSH controller peer".into()); }
    Ok(())
}
fn connect_control(path: &Path) -> io::Result<UnixStream> {
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let name = path.as_os_str().as_bytes();
    if name.len() >= address.sun_path.len() || name.contains(&0) { return Err(io::Error::new(io::ErrorKind::InvalidInput, "SSH control socket path is too long")); }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (target, source) in address.sun_path.iter_mut().zip(name) { *target = *source as libc::c_char; }
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK, 0) };
    if raw < 0 { return Err(io::Error::last_os_error()); }
    let socket = unsafe { UnixStream::from_raw_fd(raw) };
    let size = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + name.len() + 1) as libc::socklen_t;
    if unsafe { libc::connect(raw, (&address as *const libc::sockaddr_un).cast(), size) } != 0 { return Err(io::Error::last_os_error()); }
    Ok(socket)
}
struct Frame { bytes: Zeroizing<Vec<u8>>, sent: usize }
struct Wire { socket: UnixStream, input: Zeroizing<Vec<u8>>, output: VecDeque<Frame>, queued: usize, progress: Instant }
impl Wire {
    fn new(socket: UnixStream) -> Result<Self, String> {
        peer(&socket)?; socket.set_nonblocking(true).map_err(|e| e.to_string())?;
        Ok(Self { socket, input: Zeroizing::new(Vec::with_capacity(LIMIT + 4 + 8192)), output: VecDeque::new(), queued: 0, progress: Instant::now() })
    }
    fn enqueue(&mut self, bytes: Zeroizing<Vec<u8>>) -> Result<(), String> {
        if bytes.len() > LIMIT || self.queued + bytes.len() + 4 > LIMIT * 2 { return Err("SSH control queue exceeds limit".into()); }
        let mut framed = Zeroizing::new(Vec::with_capacity(bytes.len() + 4));
        framed.extend_from_slice(&(bytes.len() as u32).to_le_bytes()); framed.extend_from_slice(&bytes);
        if self.output.is_empty() { self.progress = Instant::now(); }
        self.queued += framed.len(); self.output.push_back(Frame { bytes: framed, sent: 0 }); Ok(())
    }
    fn notice(&mut self, notice: Notice) -> Result<(), String> { self.enqueue(Zeroizing::new(serde_json::to_vec(&notice).map_err(|e| e.to_string())?)) }
    fn flush(&mut self) -> Result<(), String> {
        while let Some(frame) = self.output.front_mut() {
            match self.socket.write(&frame.bytes[frame.sent..]) {
                Ok(0) => return Err("SSH controller closed".into()),
                Ok(n) => { frame.sent += n; self.queued -= n; self.progress = Instant::now(); if frame.sent == frame.bytes.len() { self.output.pop_front(); } },
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.to_string()),
            }
        }
        if !self.output.is_empty() && self.progress.elapsed() >= DEADLINE { return Err("SSH controller stopped accepting events".into()); }
        Ok(())
    }
    fn read(&mut self) -> Result<(Vec<Zeroizing<Vec<u8>>>, bool), String> {
        let mut bytes = Zeroizing::new([0u8; 8192]);
        let mut closed = false;
        match self.socket.read(&mut bytes[..]) {
            Ok(0) => closed = true,
            Ok(n) => self.input.extend_from_slice(&bytes[..n]),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {},
            Err(e) => return Err(e.to_string()),
        }
        let mut records = Vec::new();
        while self.input.len() >= 4 {
            let size = u32::from_le_bytes(self.input[..4].try_into().unwrap()) as usize;
            if size == 0 || size > LIMIT { return Err("Invalid SSH control frame size".into()); }
            if self.input.len() < size + 4 { break; }
            records.push(Zeroizing::new(self.input[4..size + 4].to_vec()));
            // Zeroize consumed secrets before shrinking the receive buffer.
            self.input[..size + 4].fill(0);
            self.input.copy_within(size + 4.., 0);
            let remaining = self.input.len() - size - 4;
            self.input[remaining..].fill(0); self.input.truncate(remaining);
        }
        if self.input.len() > LIMIT + 4 || (closed && !self.input.is_empty()) { return Err("Incomplete SSH control frame".into()); }
        Ok((records, closed))
    }
}
fn prompt_notice(p: &auth::Prompt) -> Notice {
    Notice::Authentication { id: p.id, kind: match p.kind { auth::PromptKind::NewHost => 0, auth::PromptKind::ChangedHost => 1, auth::PromptKind::Password => 2, auth::PromptKind::Passphrase => 3 }, title: p.title.clone(), detail: p.detail.clone(), fingerprint: p.fingerprint.clone(), previous_fingerprint: p.previous_fingerprint.clone() }
}
struct Tty { saved: libc::termios, flags: [i32; 3], signals: File, mask: libc::sigset_t }
impl Tty {
    fn new() -> Result<Self, String> {
        let mut saved = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(0, &mut saved) } != 0 { return Err("SSH pane requires a controlling terminal".into()); }
        let mut raw = saved; unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw) } != 0 { return Err(io::Error::last_os_error().to_string()); }
        let mut mask: libc::sigset_t = unsafe { std::mem::zeroed() };
        let mut previous = unsafe { std::mem::zeroed() };
        unsafe { libc::sigemptyset(&mut mask); libc::sigaddset(&mut mask, libc::SIGWINCH); libc::sigaddset(&mut mask, libc::SIGHUP); libc::sigaddset(&mut mask, libc::SIGTERM); }
        let result = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &mask, &mut previous) };
        if result != 0 { unsafe { libc::tcsetattr(0, libc::TCSANOW, &saved); } return Err("Cannot mask SSH pane signals".into()); }
        let signal_fd = unsafe { libc::signalfd(-1, &mask, libc::SFD_NONBLOCK | libc::SFD_CLOEXEC) };
        if signal_fd < 0 { unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut()); libc::tcsetattr(0, libc::TCSANOW, &saved); } return Err(io::Error::last_os_error().to_string()); }
        let flags = [0, 1, 2].map(|fd| unsafe { libc::fcntl(fd, libc::F_GETFL) });
        let tty = Self { saved, flags, signals: unsafe { File::from_raw_fd(signal_fd) }, mask: previous };
        for fd in 0..3 { if flags[fd as usize] < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags[fd as usize] | libc::O_NONBLOCK) } < 0 { return Err(io::Error::last_os_error().to_string()); } }
        Ok(tty)
    }
    fn size(&self) -> Result<(u16, u16, u16, u16), String> {
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(0, libc::TIOCGWINSZ, &mut size) } != 0 || size.ws_col == 0 || size.ws_row == 0 { return Err("SSH pane has invalid terminal dimensions".into()); }
        Ok((size.ws_col, size.ws_row, (size.ws_xpixel / size.ws_col).max(1), (size.ws_ypixel / size.ws_row).max(1)))
    }
}
impl Drop for Tty { fn drop(&mut self) { unsafe { libc::tcsetattr(0, libc::TCSANOW, &self.saved); for fd in 0..3 { libc::fcntl(fd, libc::F_SETFL, self.flags[fd as usize]); } libc::pthread_sigmask(libc::SIG_SETMASK, &self.mask, std::ptr::null_mut()); } } }
struct SocketCleanup(PathBuf);
impl Drop for SocketCleanup { fn drop(&mut self) { let _ = fs::remove_file(&self.0); } }

pub fn run(path: &Path) -> Result<(), String> {
    let d = descriptor(path)?;
    let dir = directory(path)?;
    let mut tty = Tty::new()?;
    let pane_id = std::env::var("HERDR_PANE_ID").map_err(|_| "SSH helper was not launched by Herdr")?;
    let pane = crate::endpoint::api_request(&d.api_socket, "pane.get", serde_json::json!({"pane_id":pane_id}))?;
    let terminal_id = pane["pane"]["terminal_id"].as_str().filter(|s| !s.is_empty()).ok_or("Herdr did not identify the SSH terminal")?.to_string();
    let metadata = Metadata { launch_id: d.launch_id.clone(), profile: d.profile.clone(), pane_id, terminal_id };
    let socket_path = dir.join("control.sock");
    let listener = UnixListener::bind(&socket_path).map_err(|e| e.to_string())?;
    let _cleanup = SocketCleanup(socket_path.clone());
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    save(&dir.join("metadata.json"), &metadata)?;
    let start = Instant::now();
    let mut connection = loop {
        match listener.accept() {
            Ok((socket, _)) => break Wire::new(socket)?,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock && start.elapsed() < DEADLINE => poll(&mut [fd(listener.as_raw_fd(), libc::POLLIN), fd(tty.signals.as_raw_fd(), libc::POLLIN)], 25)?,
            Err(e) => return Err(format!("SSH controller did not attach: {e}")),
        }
    };
    connection.notice(Notice::Attached { metadata: metadata.clone() })?;
    let (cols, rows, cell_width, cell_height) = tty.size()?;
    let (mut transport, pipes) = ssh::Transport::start_session(d.profile.config, d.profile.name, d.auth_root,
        ssh::SessionRequest::Shell { cols, rows, cell_width, cell_height })?;
    let mut controller = Some(connection);
    let result = terminal_loop(&mut tty, &listener, &metadata, &transport.control, pipes, &mut controller);
    let cleanup = transport.stop();
    let failure = transport.control.failure();
    let result = result.and(cleanup).and_then(|()| failure.as_ref().map_or(Ok(()), |f| Err(f.message.clone())));
    if let Some(mut wire) = controller {
        let _ = wire.notice(Notice::Closed { message: result.as_ref().err().cloned().unwrap_or_else(|| "SSH shell closed".into()), fatal: failure.as_ref().is_some_and(|f| f.fatal) });
        let deadline = Instant::now();
        while !wire.output.is_empty() && deadline.elapsed() < DEADLINE {
            if wire.flush().is_err() { break; }
            let _ = poll(&mut [fd(wire.socket.as_raw_fd(), libc::POLLOUT)], 25);
        }
    }
    result
}

struct Buffer { bytes: Vec<u8>, at: usize, progress: Instant }
impl Buffer {
    fn new() -> Self { Self { bytes: Vec::with_capacity(32768), at: 0, progress: Instant::now() } }
    fn pending(&self) -> bool { self.at < self.bytes.len() }
    fn read(&mut self, source: i32) -> Result<bool, String> {
        if self.pending() { return Ok(false); }
        self.bytes.resize(32768, 0); self.at = 0;
        let n = unsafe { libc::read(source, self.bytes.as_mut_ptr().cast(), self.bytes.len()) };
        if n < 0 { self.bytes.clear(); let error = io::Error::last_os_error(); if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) { return Ok(false); } return Err(error.to_string()); }
        self.bytes.truncate(n as usize); self.progress = Instant::now(); Ok(n == 0)
    }
    fn write(&mut self, target: i32) -> Result<(), String> {
        if !self.pending() { return Ok(()); }
        let n = unsafe { libc::write(target, self.bytes[self.at..].as_ptr().cast(), self.bytes.len() - self.at) };
        if n > 0 { self.at += n as usize; self.progress = Instant::now(); }
        else if n == 0 { return Err("SSH pane output closed".into()); }
        else if !matches!(io::Error::last_os_error().kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) { return Err(io::Error::last_os_error().to_string()); }
        if self.pending() && self.progress.elapsed() >= DEADLINE { return Err("SSH pane I/O stalled".into()); }
        Ok(())
    }
}
fn terminal_loop(tty: &mut Tty, listener: &UnixListener, metadata: &Metadata, control: &ssh::Control, pipes: ssh::Pipes, controller: &mut Option<Wire>) -> Result<(), String> {
    let mut input = Buffer::new(); let mut output = Buffer::new(); let mut errors = Buffer::new();
    let mut ready = false; let mut prompt = None; let mut out_eof = false; let mut err_eof = false; let mut stopping = false;
    loop {
        if let Some(p) = control.prompt() { if let Some(wire) = controller { wire.notice(prompt_notice(&p))?; } prompt = Some(p.id); }
        if !ready && !stopping && control.ready() {
            // Flush the kernel PTY queue at the admission transition; never replay dialog keystrokes.
            if unsafe { libc::tcflush(0, libc::TCIFLUSH) } != 0 { return Err(io::Error::last_os_error().to_string()); }
            input.bytes.clear(); input.at = 0; prompt = None; ready = true;
            if let Some(wire) = controller { wire.notice(Notice::Ready)?; }
        }
        if let Some(wire) = controller {
            let exchange = (|| -> Result<bool, String> {
                wire.flush()?;
                let (records, closed) = wire.read()?;
                for record in records {
                    if record.len() < 33 || &record[1..33] != metadata.launch_id.as_bytes() { return Err("Foreign SSH launch control".into()); }
                    match record[0] {
                        2 if record.len() >= 43 && record.len() <= 43 + SECRET_LIMIT && !ready => {
                            let id = u64::from_le_bytes(record[33..41].try_into().unwrap());
                            if prompt != Some(id) || record[41] > 1 || record[42] > 1 { return Err("Stale SSH authentication answer".into()); }
                            let secret = Zeroizing::new(std::str::from_utf8(&record[43..]).map_err(|_| "Invalid SSH secret encoding")?.to_owned());
                            control.answer(auth::Answer { id, approved: record[41] == 1, remember_password: record[42] == 1, secret })?; prompt = None;
                        },
                        1 if record.len() == 33 => { stopping = true; control.cancel(); prompt = None; },
                        _ => return Err("Invalid SSH control command".into()),
                    }
                }
                Ok(closed)
            })();
            if !matches!(exchange, Ok(false)) { *controller = None; if !ready { control.cancel(); return Err("SSH sign-in controller detached".into()); } }
        }
        match listener.accept() {
            Ok((socket, _)) => {
                if controller.is_none() && ready && !stopping {
                    let mut wire = Wire::new(socket)?; wire.notice(Notice::Attached { metadata: metadata.clone() })?; wire.notice(Notice::Ready)?; *controller = Some(wire);
                }
            },
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {},
            Err(e) => return Err(e.to_string()),
        }
        let mut signal = [0u8; 128];
        while tty.signals.read(&mut signal).is_ok_and(|n| n == 128) {
            let kind = u32::from_ne_bytes(signal[..4].try_into().unwrap()) as i32;
            if kind == libc::SIGWINCH && !stopping && !control.finished() { let (c, r, w, h) = tty.size()?; control.resize(c, r, w, h)?; }
            else if kind != libc::SIGWINCH { stopping = true; control.cancel(); }
        }
        if !stopping && !control.finished() && input.read(0)? { stopping = true; control.cancel(); }
        if ready && !stopping && !control.finished() { input.write(pipes.stdin.as_raw_fd())?; } else { input.bytes.clear(); input.at = 0; }
        if !out_eof { out_eof = output.read(pipes.stdout.as_raw_fd())?; }
        if !err_eof { err_eof = errors.read(pipes.stderr.as_raw_fd())?; }
        output.write(1)?; errors.write(2)?;
        if control.finished() && out_eof && err_eof && !output.pending() && !errors.pending() { return Ok(()); }
        let mut fds = [fd(if input.pending() || stopping || control.finished() { -1 } else { 0 }, libc::POLLIN), fd(if output.pending() { 1 } else if !out_eof { pipes.stdout.as_raw_fd() } else { -1 }, if output.pending() { libc::POLLOUT } else { libc::POLLIN }),
            fd(if errors.pending() { 2 } else if !err_eof { pipes.stderr.as_raw_fd() } else { -1 }, if errors.pending() { libc::POLLOUT } else { libc::POLLIN }), fd(if input.pending() { pipes.stdin.as_raw_fd() } else { -1 }, libc::POLLOUT),
            fd(listener.as_raw_fd(), libc::POLLIN), fd(tty.signals.as_raw_fd(), libc::POLLIN), fd(controller.as_ref().map_or(-1, |w| w.socket.as_raw_fd()), libc::POLLIN | if controller.as_ref().is_some_and(|w| !w.output.is_empty()) { libc::POLLOUT } else { 0 })];
        // Control exposes readiness/prompts via a mutex, not a wake fd; SIGWINCH itself is event driven.
        poll(&mut fds, 25)?;
    }
}

struct State { running: bool, ready: bool, attached: bool, closing: bool, prompt: Option<u64>, failure: Option<String>, outgoing: VecDeque<Zeroizing<Vec<u8>>>, queued: usize }
struct Cancellation(Option<Box<dyn FnOnce() + Send>>);
impl Cancellation {
    fn cancel(&mut self) -> Result<(), String> { self.0.take().map_or(Ok(()), |f| std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|_| "SSH delivery cancellation panicked".into())) }
}
impl Drop for Cancellation { fn drop(&mut self) { let _ = self.cancel(); } }
pub struct Client { state: Arc<Mutex<State>>, launch_id: String, wake: UnixStream, worker: Option<JoinHandle<Result<(), String>>>, cancellation: Cancellation }
impl Client {
    pub fn attach(prepared: &Prepared, notify: impl Fn(Event) -> Result<(), String> + Send + Sync + 'static, cancel_notify: impl FnOnce() + Send + 'static) -> Result<Self, String> {
        let cancellation = Cancellation(Some(Box::new(cancel_notify)));
        let prepared = prepared.clone(); let d = descriptor(&prepared.descriptor)?;
        if d.launch_id != prepared.launch_id || serde_json::to_value(&d.profile).map_err(|e| e.to_string())? != serde_json::to_value(&prepared.profile).map_err(|e| e.to_string())? { return Err("SSH prepared launch changed".into()); }
        let launch_id = prepared.launch_id.clone();
        let (wake, reader) = UnixStream::pair().map_err(|e| e.to_string())?;
        wake.set_nonblocking(true).map_err(|e| e.to_string())?; reader.set_nonblocking(true).map_err(|e| e.to_string())?;
        let state = Arc::new(Mutex::new(State { running: true, ready: false, attached: false, closing: false, prompt: None, failure: None, outgoing: VecDeque::new(), queued: 0 }));
        let shared = state.clone();
        let worker = thread::Builder::new().name("ssh-pane-control".into()).spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| controller_loop(&prepared, &d, &shared, reader, &notify))).unwrap_or_else(|_| Err("SSH control callback or worker panicked".into()));
            let report = if let Ok(mut s) = shared.lock() { let running = s.running; s.ready = false; s.prompt = None; s.running = false; s.outgoing.clear(); s.queued = 0; if let Err(error) = &result { s.failure = Some(error.clone()); } running } else { false };
            if report { if let Err(error) = &result { let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| notify(Event::ControlLost(error.clone())))); } }
            result
        }).map_err(|e| e.to_string())?;
        Ok(Self { state, launch_id, wake, worker: Some(worker), cancellation })
    }
    pub fn answer_auth(&self, answer: auth::Answer) -> Result<(), String> {
        if answer.secret.len() > SECRET_LIMIT { return Err("SSH secret exceeds 16 KiB".into()); }
        let mut s = self.state.lock().map_err(|_| "SSH controller state poisoned")?;
        if !s.running || !s.attached || s.closing || s.prompt != Some(answer.id) { return Err("SSH authentication prompt is no longer current".into()); }
        let mut bytes = Zeroizing::new(Vec::with_capacity(43 + answer.secret.len())); bytes.push(2); bytes.extend_from_slice(self.launch_id.as_bytes()); bytes.extend_from_slice(&answer.id.to_le_bytes()); bytes.push(u8::from(answer.approved)); bytes.push(u8::from(answer.remember_password)); bytes.extend_from_slice(answer.secret.as_bytes());
        self.enqueue(&mut s, bytes)?; s.prompt = None; Ok(())
    }
    fn enqueue(&self, s: &mut State, bytes: Zeroizing<Vec<u8>>) -> Result<(), String> {
        if s.queued + bytes.len() > LIMIT { return Err("SSH control queue is full".into()); }
        match (&self.wake).write(&[1]) { Ok(_) => {}, Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}, Err(e) => return Err(e.to_string()) }
        s.queued += bytes.len(); s.outgoing.push_back(bytes); Ok(())
    }
    pub fn ready(&self) -> bool { self.state.lock().is_ok_and(|s| s.running && s.ready && !s.closing) }
    pub fn failure(&self) -> Option<String> { self.state.lock().ok()?.failure.clone() }
    /// Queue explicit cancellation. Retain the controller until Closed; stop is a
    /// detach operation and revokes control commands which have not been sent.
    pub fn close(&self) -> Result<(), String> {
        let mut s = self.state.lock().map_err(|_| "SSH controller state poisoned")?;
        if !s.running || !s.attached { return Err("SSH controller is not attached".into()); }
        let mut bytes = Zeroizing::new(Vec::with_capacity(33)); bytes.push(1); bytes.extend_from_slice(self.launch_id.as_bytes());
        self.enqueue(&mut s, bytes)?; s.closing = true; s.ready = false; s.prompt = None; Ok(())
    }
    pub fn stop(&mut self) -> Result<(), String> {
        if self.worker.as_ref().is_some_and(|w| w.thread().id() == thread::current().id()) { return Err("Cannot join SSH controller from its callback".into()); }
        if let Ok(mut s) = self.state.lock() { s.running = false; s.ready = false; s.prompt = None; s.outgoing.clear(); s.queued = 0; }
        let _ = (&self.wake).write(&[1]);
        let cancelled = self.cancellation.cancel();
        let joined = self.worker.take().map_or(Ok(()), |w| w.join().map_err(|_| "SSH controller panicked".to_string()).and_then(|r| r));
        cancelled.and(joined)
    }
}
impl Drop for Client { fn drop(&mut self) { let _ = self.stop(); } }
fn controller_loop(prepared: &Prepared, d: &Descriptor, state: &Mutex<State>, mut wake: UnixStream, notify: &impl Fn(Event) -> Result<(), String>) -> Result<(), String> {
    let socket = directory(&prepared.descriptor)?.join("control.sock"); let started = Instant::now();
    let mut wire = loop {
        if !state.lock().map_err(|_| "SSH controller state poisoned")?.running { return Ok(()); }
        match connect_control(&socket) {
            Ok(stream) => break Wire::new(stream)?,
            Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused | io::ErrorKind::WouldBlock) && started.elapsed() < DEADLINE => poll(&mut [fd(wake.as_raw_fd(), libc::POLLIN)], 25)?,
            Err(e) => return Err(format!("Cannot attach SSH pane: {e}")),
        }
    };
    let mut attached = false; let mut closed_notice = false; let mut bytes = [0u8; 128];
    loop {
        {
            let mut s = state.lock().map_err(|_| "SSH controller state poisoned")?;
            if !s.running { return Ok(()); }
            while let Some(record) = s.outgoing.pop_front() { s.queued -= record.len(); wire.enqueue(record)?; }
        }
        wire.flush()?;
        let (records, closed) = wire.read()?;
        for record in records {
            let notice: Notice = serde_json::from_slice(&record).map_err(|e| e.to_string())?;
            let event = match notice {
                Notice::Attached { metadata } if !attached => {
                    live(d, &metadata)?;
                    if prepared.metadata.as_ref().is_some_and(|expected| !same_metadata(expected, &metadata)) { return Err("SSH reattachment identity changed".into()); }
                    attached = true; state.lock().map_err(|_| "SSH controller state poisoned")?.attached = true;
                    Event::Attached(metadata)
                },
                Notice::Authentication { id, kind, title, detail, fingerprint, previous_fingerprint } if attached => {
                    let mut s = state.lock().map_err(|_| "SSH controller state poisoned")?;
                    if s.closing { continue; }
                    if s.ready || s.prompt.is_some() || id == 0 { return Err("Invalid SSH prompt transition".into()); } s.prompt = Some(id);
                    Event::Authentication(auth::Prompt { id, kind: match kind { 0 => auth::PromptKind::NewHost, 1 => auth::PromptKind::ChangedHost, 2 => auth::PromptKind::Password, 3 => auth::PromptKind::Passphrase, _ => return Err("Invalid SSH prompt kind".into()) }, title, detail, fingerprint, previous_fingerprint })
                },
                Notice::Ready if attached => { let mut s = state.lock().map_err(|_| "SSH controller state poisoned")?; if s.closing { continue; } s.ready = true; s.prompt = None; Event::Ready },
                Notice::Closed { message, fatal } if attached => { let mut s = state.lock().map_err(|_| "SSH controller state poisoned")?; s.ready = false; s.prompt = None; closed_notice = true; Event::Closed { message, fatal } },
                _ => return Err("Invalid SSH controller event order".into()),
            };
            notify(event)?;
        }
        if closed_notice { return Ok(()); }
        if closed { return Err("SSH helper control connection closed".into()); }
        if !attached && started.elapsed() >= DEADLINE { return Err("SSH helper did not identify its pane".into()); }
        poll(&mut [fd(wire.socket.as_raw_fd(), libc::POLLIN | if wire.output.is_empty() { 0 } else { libc::POLLOUT }), fd(wake.as_raw_fd(), libc::POLLIN)], 100)?;
        while wake.read(&mut bytes).is_ok_and(|n| n != 0) {}
    }
}
