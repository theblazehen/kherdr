//! Finite device-only selection owner; each success marker follows checked X I/O.
use std::cell::Cell;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ConnectionExt, CreateWindowAux, EventMask,
    PropMode, Property, SelectionNotifyEvent, SelectionRequestEvent, Window, WindowClass,
    SELECTION_NOTIFY_EVENT,
};
use x11rb::protocol::Event;
use x11rb::reexports::x11rb_protocol::{parse_display, xauth};
use x11rb::rust_connection::{DefaultStream, PollMode, RustConnection, Stream};
use x11rb::utils::RawFdContainer;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, CURRENT_TIME, NONE};

const MAX_INPUT: usize = 2 * 1024 * 1024;
const MAX_TRANSFERS: usize = 8;
const CHUNK: usize = 16 * 1024;
const TICK: Duration = Duration::from_millis(10);
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(10);
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Options {
    text: String,
    delay: Duration,
    hold: Duration,
    primary: bool,
    incr: bool,
}

fn options() -> Result<Options> {
    let mut args = std::env::args_os().skip(1);
    let command = args.next().ok_or("expected clipboard or clipboard-file")?;
    let millis = |value: Option<std::ffi::OsString>| -> Result<u64> {
        let value = value.ok_or("missing DELAY_MS or HOLD_MS")?;
        let value: u64 = value.to_str().ok_or("duration is not UTF-8")?.parse()?;
        if value > 30_000 { return Err("duration exceeds 30000 ms".into()); }
        Ok(value)
    };
    let delay = Duration::from_millis(millis(args.next())?);
    let hold = Duration::from_millis(millis(args.next())?);
    if hold.is_zero() { return Err("HOLD_MS must be positive".into()); }
    let input = args.next().ok_or("missing TEXT or PATH")?;
    let primary = match args.next() {
        None => false,
        Some(value) if value == "primary" => true,
        Some(_) => return Err("optional final argument must be primary".into()),
    };
    if args.next().is_some() { return Err("too many arguments".into()); }
    let text = if command == "clipboard" {
        input.into_string().map_err(|_| "TEXT is not UTF-8")?
    } else if command == "clipboard-file" {
        let mut file = File::open(input)?;
        if !file.metadata()?.is_file() { return Err("PATH must be a regular file".into()); }
        // Reserve exactly the cap: read_to_end must not grow a nearly-full input
        // to a second payload-sized allocation just to detect one extra byte.
        let mut bytes = Vec::with_capacity(MAX_INPUT);
        Read::by_ref(&mut file).take(MAX_INPUT as u64).read_to_end(&mut bytes)?;
        let mut extra = [0];
        if file.read(&mut extra)? != 0 { return Err("file exceeds 2 MiB".into()); }
        String::from_utf8(bytes).map_err(|_| "file is not UTF-8")?
    } else {
        return Err("expected clipboard or clipboard-file".into());
    };
    if text.len() > MAX_INPUT { return Err("input exceeds 2 MiB".into()); }
    if text.as_bytes().contains(&0) { return Err("input contains NUL".into()); }
    let incr = match std::env::var_os("KHERDR_CLIPBOARD_INCR") {
        None => false,
        Some(value) if value == "1" => true,
        Some(_) => return Err("KHERDR_CLIPBOARD_INCR must be unset or 1".into()),
    };
    Ok(Options { text, delay, hold, primary, incr })
}

// Bound setup, checked requests and output backpressure as well as the event
// loop. The default x11rb Stream::poll can otherwise wait indefinitely.
#[derive(Debug)]
struct BoundedStream {
    inner: DefaultStream,
    deadline: Cell<Instant>,
}

impl BoundedStream {
    fn check(&self) -> io::Result<()> {
        if Instant::now() >= self.deadline.get() {
            Err(io::Error::new(io::ErrorKind::TimedOut, "X11 fixture deadline expired"))
        } else { Ok(()) }
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
            // SAFETY: one initialized pollfd lives throughout the call.
            let ready = unsafe { libc::poll(&mut fd, 1, 10) };
            if ready > 0 {
                if fd.revents & libc::POLLNVAL != 0 {
                    return Err(io::Error::new(io::ErrorKind::NotConnected, "invalid X11 socket"));
                }
                return Ok(());
            }
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted { return Err(error); }
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

fn local_socket(path: &str, abstract_socket: bool) -> io::Result<UnixStream> {
    // SAFETY: no pointer arguments; the returned descriptor is immediately owned.
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC, 0) };
    if raw < 0 { return Err(io::Error::last_os_error()); }
    // SAFETY: raw is a new uniquely-owned descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: zero initialization is valid for sockaddr_un.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    if path.len() + 1 > address.sun_path.len() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "X11 socket path too long"));
    }
    for (destination, byte) in address.sun_path[usize::from(abstract_socket)..].iter_mut().zip(path.bytes()) {
        *destination = byte as libc::c_char;
    }
    let length = std::mem::offset_of!(libc::sockaddr_un, sun_path) + path.len() + 1;
    // SAFETY: initialized address and length within the sockaddr_un allocation.
    let connected = unsafe { libc::connect(fd.as_raw_fd(), (&address as *const libc::sockaddr_un).cast(), length as libc::socklen_t) };
    if connected < 0 { return Err(io::Error::last_os_error()); }
    Ok(UnixStream::from(fd))
}

#[derive(Clone, Copy)]
enum Stage {
    Pending,
    Incr { offset: usize },
    FinalDelete { sequence: u16, published: bool },
}

struct Transfer {
    id: u64,
    request: SelectionRequestEvent,
    property: Atom,
    due: Instant,
    deadline: Instant,
    stage: Stage,
}

struct Owner {
    connection: RustConnection<BoundedStream>,
    window: Window,
    selection: Atom,
    targets: Atom,
    utf8: Atom,
    incr: Atom,
    chunk: usize,
    options: Options,
    latin1_len: Option<usize>,
    start: Instant,
    end: Instant,
    owns: bool,
    next_id: u64,
    transfers: Vec<Transfer>,
}

impl Owner {
    fn connect(options: Options) -> Result<Self> {
        let setup_deadline = Instant::now() + Duration::from_secs(5);
        let display = parse_display::parse_display(Some(":0"))?;
        let path = display.connect_instruction().find_map(|address| match address {
            parse_display::ConnectAddress::Socket(path) => Some(path),
            _ => None,
        }).ok_or("DISPLAY=:0 has no local socket address")?;
        let socket = local_socket(&path, true).or_else(|_| local_socket(&path, false))?;
        let (inner, (family, address)) = DefaultStream::from_unix_stream(socket)?;
        let (auth_name, auth_data) = match xauth::get_auth(family, &address, display.display) {
            Ok(auth) => auth.unwrap_or_default(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (Vec::new(), Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let connection = RustConnection::connect_to_stream_with_auth_info(
            BoundedStream { inner, deadline: Cell::new(setup_deadline) },
            display.screen.into(), auth_name, auth_data,
        )?;
        let root = connection.setup().roots.get(usize::from(display.screen)).ok_or("missing X11 screen")?.root;
        let window = connection.generate_id()?;
        connection.create_window(COPY_DEPTH_FROM_PARENT, window, root, 0, 0, 1, 1, 0,
            WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new())?.check()?;
        let atom = |name: &[u8]| -> Result<Atom> { Ok(connection.intern_atom(false, name)?.reply()?.atom) };
        let selection = if options.primary { AtomEnum::PRIMARY.into() } else { atom(b"CLIPBOARD")? };
        let targets = atom(b"TARGETS")?;
        let utf8 = atom(b"UTF8_STRING")?;
        let incr = atom(b"INCR")?;
        let chunk = usize::from(connection.setup().maximum_request_length)
            .checked_sub(6).ok_or("X11 core request limit too small")? * 4;
        if chunk < 12 { return Err("X11 core request limit too small for TARGETS".into()); }
        connection.set_selection_owner(window, selection, CURRENT_TIME)?.check()?;
        if connection.get_selection_owner(selection)?.reply()?.owner != window {
            return Err("X11 selection ownership was not acquired".into());
        }
        let latin1_len = options.text.chars().try_fold(0usize, |count, ch| {
            if u32::from(ch) <= 255 { Some(count + 1) } else { None }
        });
        let start = Instant::now();
        let end = start + options.hold;
        connection.stream().deadline.set(end);
        Ok(Self { connection, window, selection, targets, utf8, incr, chunk: chunk.min(CHUNK),
            options, latin1_len, start, end, owns: true, next_id: 1,
            transfers: Vec::with_capacity(MAX_TRANSFERS) })
    }

    fn marker(&self, name: &str, transfer: &Transfer, reason: &str) -> Result<()> {
        let bytes = if transfer.request.target == self.utf8 { self.options.text.len() }
            else { self.latin1_len.unwrap_or(0) };
        let mut out = io::stderr().lock();
        writeln!(out, "{name} id={} elapsed_ms={} requestor={} target={} property={} bytes={} mode={} reason={reason}",
            transfer.id, self.start.elapsed().as_millis(), transfer.request.requestor,
            transfer.request.target, transfer.property, bytes, if self.options.incr { "INCR" } else { "PROPERTY" })?;
        out.flush()?;
        Ok(())
    }

    fn notify(&self, request: &SelectionRequestEvent, property: Atom) -> Result<()> {
        self.connection.send_event(false, request.requestor, EventMask::NO_EVENT, SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT, sequence: 0, time: request.time,
            requestor: request.requestor, selection: request.selection, target: request.target, property,
        })?.check()?;
        self.connection.flush()?;
        Ok(())
    }

    fn still_owner(&self) -> Result<()> {
        if !self.owns || self.connection.get_selection_owner(self.selection)?.reply()?.owner != self.window {
            return Err("selection ownership lost".into());
        }
        Ok(())
    }

    fn accept(&mut self, request: SelectionRequestEvent) -> Result<()> {
        let property = if request.property == NONE { request.target } else { request.property };
        // One active conversion per (requestor, property); never overwrite an
        // INCR handshake, even with an unrelated TARGETS request.
        if !self.owns || request.owner != self.window || request.selection != self.selection
            || self.transfers.iter().any(|t| t.request.requestor == request.requestor && t.property == property) {
            let _ = self.notify(&request, NONE); // Requestor may already be gone.
            return Ok(());
        }
        if request.target == self.targets {
            let targets = [self.targets, self.utf8, AtomEnum::STRING.into()];
            let count = if self.latin1_len.is_some() { 3 } else { 2 };
            let result = (|| -> Result<()> {
                self.connection.change_property32(PropMode::REPLACE, request.requestor, property,
                    AtomEnum::ATOM, &targets[..count])?.check()?;
                self.notify(&request, property)
            })();
            if result.is_err() { let _ = self.notify(&request, NONE); }
            return Ok(());
        }
        if !(request.target == self.utf8 || (request.target == u32::from(AtomEnum::STRING) && self.latin1_len.is_some()))
            || self.transfers.len() == MAX_TRANSFERS {
            let _ = self.notify(&request, NONE);
            return Ok(());
        }
        let due = Instant::now() + self.options.delay;
        let transfer = Transfer { id: self.next_id, request, property, due,
            deadline: self.end.min(due + TRANSFER_TIMEOUT), stage: Stage::Pending };
        self.next_id += 1;
        self.marker("REQUEST", &transfer, "accepted")?;
        // Watch destruction during the delay too, not just during INCR.
        let subscribed: Result<()> = self.connection.change_window_attributes(request.requestor,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE | EventMask::STRUCTURE_NOTIFY))
            .map_err(Into::into).and_then(|cookie| cookie.check().map_err(Into::into));
        if let Err(error) = subscribed {
            self.marker("CANCELLED", &transfer, "requestor-unavailable")?;
            eprintln!("clipboard-device-owner: subscription failed: {error}");
            let _ = self.notify(&request, NONE);
        } else { self.transfers.push(transfer); }
        Ok(())
    }

    // Offsets are in the single stored UTF-8 payload. Latin-1 is transcoded one
    // bounded stack chunk at a time, never retained as a second input-sized Vec.
    fn write_chunk(&self, transfer: &Transfer, offset: usize, mode: PropMode) -> Result<usize> {
        let text = &self.options.text;
        if transfer.request.target == self.utf8 {
            let end = (offset + self.chunk).min(text.len());
            self.connection.change_property8(mode, transfer.request.requestor, transfer.property,
                transfer.request.target, &text.as_bytes()[offset..end])?.check()?;
            Ok(end)
        } else {
            let mut bytes = [0u8; CHUNK];
            let mut used = 0;
            let mut end = offset;
            for ch in text[offset..].chars().take(self.chunk) {
                bytes[used] = u32::from(ch) as u8;
                used += 1;
                end += ch.len_utf8();
            }
            self.connection.change_property8(mode, transfer.request.requestor, transfer.property,
                transfer.request.target, &bytes[..used])?.check()?;
            Ok(end)
        }
    }

    fn publish(&self, transfer: &mut Transfer) -> Result<bool> {
        self.still_owner()?;
        if self.options.incr {
            let bytes = if transfer.request.target == self.utf8 { self.options.text.len() }
                else { self.latin1_len.ok_or("STRING is not lossless")? };
            self.connection.change_property32(PropMode::REPLACE, transfer.request.requestor,
                transfer.property, self.incr, &[bytes as u32])?.check()?;
            self.notify(&transfer.request, transfer.property)?;
            transfer.stage = Stage::Incr { offset: 0 };
            Ok(false)
        } else {
            let mut offset = self.write_chunk(transfer, 0, PropMode::REPLACE)?;
            while offset < self.options.text.len() {
                offset = self.write_chunk(transfer, offset, PropMode::APPEND)?;
            }
            self.still_owner()?;
            self.notify(&transfer.request, transfer.property)?;
            Ok(true)
        }
    }

    fn deleted(&self, transfer: &mut Transfer) -> Result<bool> {
        if matches!(transfer.stage, Stage::Pending) { return Ok(false); }
        if let Stage::FinalDelete { published, .. } = transfer.stage {
            // NEW_VALUE for our checked terminating write followed by DELETE
            // proves consumption. The reader may already have destroyed its
            // window, so a GetProperty/ownership round trip here is both
            // unnecessary and racy. Old queued DELETEs precede that NEW_VALUE.
            return Ok(published);
        }
        // Ignore stale DELETE events whose property has since been replaced.
        let property = self.connection.get_property(false, transfer.request.requestor,
            transfer.property, AtomEnum::ANY, 0, 0)?.reply()?;
        if property.type_ != NONE { return Ok(false); }
        self.still_owner()?;
        match transfer.stage {
            Stage::Incr { offset } if offset < self.options.text.len() => {
                let offset = self.write_chunk(transfer, offset, PropMode::REPLACE)?;
                transfer.stage = Stage::Incr { offset };
            }
            Stage::Incr { .. } => {
                let cookie = self.connection.change_property8(PropMode::REPLACE, transfer.request.requestor,
                    transfer.property, transfer.request.target, &[])?;
                let sequence = cookie.sequence_number() as u16;
                cookie.check()?;
                self.connection.flush()?;
                transfer.stage = Stage::FinalDelete { sequence, published: false };
            }
            Stage::FinalDelete { .. } => unreachable!("handled before querying the requestor"),
            Stage::Pending => {}
        }
        Ok(false)
    }

    fn finish(&self, transfer: &Transfer, success: bool, reason: &str) -> Result<()> {
        self.marker(if success { "REPLY" } else { "CANCELLED" }, transfer, reason)?;
        if !success {
            // Cleanup errors (notably BadWindow) cannot turn cancellation into
            // success or terminate the fixture. Never send a second notify for INCR.
            if let Ok(cookie) = self.connection.delete_property(transfer.request.requestor, transfer.property) {
                let _ = cookie.check();
            }
            if matches!(transfer.stage, Stage::Pending) { let _ = self.notify(&transfer.request, NONE); }
        }
        if !self.transfers.iter().any(|t| t.request.requestor == transfer.request.requestor) {
            if let Ok(cookie) = self.connection.change_window_attributes(transfer.request.requestor,
                &ChangeWindowAttributesAux::new().event_mask(EventMask::NO_EVENT)) {
                let _ = cookie.check();
            }
        }
        Ok(())
    }

    fn run(&mut self) -> Result<()> {
        {
            let mut out = io::stderr().lock();
            writeln!(out, "READY selection={} bytes={} delay_ms={} hold_ms={} mode={}",
                if self.options.primary { "PRIMARY" } else { "CLIPBOARD" }, self.options.text.len(),
                self.options.delay.as_millis(), self.options.hold.as_millis(),
                if self.options.incr { "INCR" } else { "PROPERTY" })?;
            out.flush()?;
        }
        while Instant::now() < self.end {
            // Bound event processing per tick so traffic cannot starve deadlines.
            for _ in 0..64 {
                if Instant::now() >= self.end { break; }
                let Some(event) = self.connection.poll_for_event()? else { break; };
                match event {
                    Event::SelectionRequest(request) => self.accept(request)?,
                    Event::SelectionClear(event) if event.selection == self.selection => {
                        self.owns = false;
                        while let Some(transfer) = self.transfers.pop() {
                            self.finish(&transfer, false, "ownership-lost")?;
                        }
                    }
                    Event::DestroyNotify(event) => {
                        let mut index = 0;
                        while index < self.transfers.len() {
                            if self.transfers[index].request.requestor == event.window {
                                let transfer = self.transfers.swap_remove(index);
                                self.finish(&transfer, false, "requestor-destroyed")?;
                            } else { index += 1; }
                        }
                    }
                    Event::PropertyNotify(event) if event.state == Property::NEW_VALUE => {
                        if let Some(transfer) = self.transfers.iter_mut().find(|t|
                            t.request.requestor == event.window && t.property == event.atom) {
                            if let Stage::FinalDelete { sequence, published } = &mut transfer.stage {
                                if event.sequence == *sequence { *published = true; }
                            }
                        }
                    }
                    Event::PropertyNotify(event) if event.state == Property::DELETE => {
                        if let Some(index) = self.transfers.iter().position(|t|
                            t.request.requestor == event.window && t.property == event.atom) {
                            let mut transfer = self.transfers.swap_remove(index);
                            if Instant::now() >= transfer.deadline {
                                self.finish(&transfer, false, "deadline")?;
                                continue;
                            }
                            self.connection.stream().deadline.set(transfer.deadline);
                            match self.deleted(&mut transfer) {
                                Ok(true) => self.finish(&transfer, true, "incr-consumed")?,
                                Ok(false) => self.transfers.push(transfer),
                                Err(error) => {
                                    self.finish(&transfer, false, "incr-failed")?;
                                    eprintln!("clipboard-device-owner: INCR failed: {error}");
                                }
                            }
                            self.connection.stream().deadline.set(self.end);
                        }
                    }
                    Event::Error(error) => eprintln!("clipboard-device-owner: asynchronous X11 error: {error:?}"),
                    _ => {}
                }
            }
            let mut index = 0;
            while index < self.transfers.len() && Instant::now() < self.end {
                let now = Instant::now();
                if now >= self.transfers[index].deadline {
                    let transfer = self.transfers.swap_remove(index);
                    self.finish(&transfer, false, "deadline")?;
                } else if matches!(self.transfers[index].stage, Stage::Pending) && now >= self.transfers[index].due {
                    let mut transfer = self.transfers.swap_remove(index);
                    self.connection.stream().deadline.set(transfer.deadline);
                    match self.publish(&mut transfer) {
                        Ok(true) => self.finish(&transfer, true, "property-published")?,
                        Ok(false) => { self.transfers.insert(index, transfer); index += 1; }
                        Err(error) => {
                            self.finish(&transfer, false, "publication-failed")?;
                            eprintln!("clipboard-device-owner: publication failed: {error}");
                        }
                    }
                    self.connection.stream().deadline.set(self.end);
                } else { index += 1; }
            }
            std::thread::sleep(TICK.min(self.end.saturating_duration_since(Instant::now())));
        }
        Ok(())
    }
}

fn main() -> std::process::ExitCode {
    let result = (|| -> Result<()> {
        let mut owner = Owner::connect(options()?)?;
        let result = owner.run();
        // Also account for every accepted request when the connection fails.
        // Closing the connection automatically destroys our window, releases the
        // selection, and removes subscriptions, without potentially blocking I/O.
        while let Some(transfer) = owner.transfers.pop() {
            owner.marker("CANCELLED", &transfer, if result.is_ok() { "hold-expired" } else { "connection-failed" })?;
        }
        result
    })();
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("clipboard-device-owner: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
