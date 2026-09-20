use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::{CStr, c_ulong};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::ptr;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{EventLoopProxy, Key, Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{EventLoopError, LogicalPosition, PhysicalSize, PlatformError, Rgb8Pixel, SharedString};
use x11_dl::{keysym, xlib};

// The Kindle window manager scopes DM to the controlling window's geometry
// and restores the next window's mode on focus/lifetime changes. KB selects
// its native fast keyboard waveform without competing framebuffer ioctls.
// The app tray opens Amazon Quick Settings through the stock chrome service.
// PC:N removes only persistent chrome; the firmware still owns overlay layout.
const TITLE: &CStr = c"L:A_N:application_ID:net.fabiszewski.kherdr_PC:N_O:URL_DM:KB";
const CALLBACK_CAPACITY: usize = 1024;
type Callback = Box<dyn FnOnce() + Send>;

thread_local! {
    // Available only while dispatching the matching native button release.
    static POINTER_RELEASE_MS: Cell<i32> = const { Cell::new(0) };
    static FOCUS_HANDLER: RefCell<Option<Box<dyn Fn(bool)>>> = const { RefCell::new(None) };
    static COPY_HANDLER: RefCell<Option<Box<dyn Fn(&str)>>> = const { RefCell::new(None) };
    static PHYSICAL_MODIFIERS: Cell<[u8; 8]> = const { Cell::new([0; 8]) };
}

const MODIFIER_KEYS: [Key; 8] = [Key::Shift, Key::ShiftR, Key::Control, Key::ControlR,
    Key::Alt, Key::AltGr, Key::Meta, Key::MetaR];

pub fn physical_modifiers() -> Vec<SharedString> {
    let held = PHYSICAL_MODIFIERS.with(Cell::get);
    MODIFIER_KEYS.into_iter().zip(held).filter_map(|(key, count)| (count != 0).then(|| key.into())).collect()
}

fn track_modifier(text: &str, pressed: bool) {
    let mut chars = text.chars();
    let Some(character) = chars.next() else { return; };
    if chars.next().is_some() { return; }
    if let Some(index) = MODIFIER_KEYS.iter().position(|key| char::from(*key) == character) {
        PHYSICAL_MODIFIERS.with(|state| {
            let mut held = state.get();
            held[index] = if pressed { held[index].saturating_add(1) } else { held[index].saturating_sub(1) };
            state.set(held);
        });
    }
}

pub fn pointer_release_duration_ms() -> i32 {
    POINTER_RELEASE_MS.with(Cell::get)
}

pub fn on_focus_changed(handler: impl Fn(bool) + 'static) {
    FOCUS_HANDLER.with(|slot| *slot.borrow_mut() = Some(Box::new(handler)));
}

pub fn on_clipboard_copy(handler: impl Fn(&str) + 'static) {
    COPY_HANDLER.with(|slot| *slot.borrow_mut() = Some(Box::new(handler)));
}

fn focus_changed(active: bool) {
    if !active { PHYSICAL_MODIFIERS.with(|state| state.set([0; 8])); }
    FOCUS_HANDLER.with(|slot| {
        if let Some(handler) = slot.borrow().as_ref() { handler(active); }
    });
}

pub fn install() -> Result<(), Box<dyn std::error::Error>> {
    let native = NativeWindow::new()?;
    let wake = Arc::new(Wake::new()?);
    let termination = TerminationSignals::new(wake.clone())?;
    slint::platform::set_platform(Box::new(XPlatform {
        window: MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer),
        native: RefCell::new(Some(native)),
        created: Cell::new(false),
        termination,
        wake,
        started: Instant::now(),
    }))?;
    Ok(())
}

fn failure(message: impl Into<String>) -> PlatformError {
    PlatformError::Other(message.into())
}

fn gray(rgb: Rgb8Pixel) -> usize {
    if rgb.r == rgb.g && rgb.g == rgb.b {
        usize::from(rgb.r)
    } else {
        ((u32::from(rgb.r) * 77 + u32::from(rgb.g) * 150 + u32::from(rgb.b) * 29 + 128) >> 8) as usize
    }
}

const TERMINATION_SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];
static SIGNAL_OWNER: AtomicBool = AtomicBool::new(false);
static SIGNAL_FD: AtomicI32 = AtomicI32::new(-1);
static SIGNAL_HANDLERS: AtomicUsize = AtomicUsize::new(0);
static TERMINATION_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn termination_signal(_: libc::c_int) {
    // Access only lock-free atomics and async-signal-safe write. Preserve the
    // interrupted thread's errno, including when an already-readable fd is full.
    let errno = unsafe { libc::__errno_location() };
    let saved_errno = unsafe { *errno };
    SIGNAL_HANDLERS.fetch_add(1, Ordering::SeqCst);
    let fd = SIGNAL_FD.load(Ordering::SeqCst);
    if fd >= 0 {
        TERMINATION_REQUESTED.store(true, Ordering::SeqCst);
        let value = 1_u64;
        loop {
            let result = unsafe { libc::write(fd, (&value as *const u64).cast(), 8) };
            if result >= 0 || unsafe { *errno } != libc::EINTR {
                break;
            }
        }
    }
    SIGNAL_HANDLERS.fetch_sub(1, Ordering::SeqCst);
    unsafe { *errno = saved_errno };
}

struct TerminationSignals {
    // Keep the fd alive until it is unpublished and in-flight writers finish.
    _wake: Arc<Wake>,
    previous: [Option<libc::sigaction>; TERMINATION_SIGNALS.len()],
}

impl TerminationSignals {
    fn new(wake: Arc<Wake>) -> io::Result<Self> {
        SIGNAL_OWNER.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| io::Error::new(io::ErrorKind::AlreadyExists, "termination handlers already installed"))?;
        let mut signals = Self {
            _wake: wake,
            previous: [const { None }; TERMINATION_SIGNALS.len()],
        };
        TERMINATION_REQUESTED.store(false, Ordering::SeqCst);
        SIGNAL_FD.store(signals._wake.fd.as_raw_fd(), Ordering::SeqCst);
        for (index, signal) in TERMINATION_SIGNALS.into_iter().enumerate() {
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = termination_signal as *const () as usize;
            // Only the handler's temporary mask changes. In particular, worker
            // threads and spawned SSH children do not inherit blocked signals.
            if unsafe { libc::sigfillset(&mut action.sa_mask) } < 0 {
                return Err(io::Error::last_os_error());
            }
            action.sa_flags = libc::SA_RESTART;
            let mut previous = std::mem::MaybeUninit::<libc::sigaction>::uninit();
            if unsafe { libc::sigaction(signal, &action, previous.as_mut_ptr()) } < 0 {
                return Err(io::Error::last_os_error());
            }
            signals.previous[index] = Some(unsafe { previous.assume_init() });
        }
        Ok(signals)
    }

    fn requested(&self) -> bool {
        TERMINATION_REQUESTED.load(Ordering::SeqCst)
    }
}

impl Drop for TerminationSignals {
    fn drop(&mut self) {
        SIGNAL_FD.store(-1, Ordering::SeqCst);
        for (signal, previous) in TERMINATION_SIGNALS.into_iter().zip(&self.previous) {
            let Some(previous) = previous else { continue };
            let mut current = std::mem::MaybeUninit::<libc::sigaction>::uninit();
            if unsafe { libc::sigaction(signal, ptr::null(), current.as_mut_ptr()) } < 0 {
                eprintln!("kherdr: reading signal {signal} disposition: {}", io::Error::last_os_error());
                continue;
            }
            // Do not overwrite a handler another component installed after us.
            if unsafe { current.assume_init() }.sa_sigaction == termination_signal as *const () as usize
                && unsafe { libc::sigaction(signal, previous, ptr::null_mut()) } < 0
            {
                eprintln!("kherdr: restoring signal {signal} disposition: {}", io::Error::last_os_error());
            }
        }
        // With sequentially consistent ordering, a writer that saw the old fd
        // is counted here. Later handlers see -1 and cannot hit a recycled fd.
        while SIGNAL_HANDLERS.load(Ordering::SeqCst) != 0 {
            std::thread::yield_now();
        }
        SIGNAL_OWNER.store(false, Ordering::SeqCst);
    }
}

struct Queue {
    callbacks: VecDeque<Callback>,
    quit: bool,
    stopped: bool,
    failure: Option<String>,
}

// Only this state crosses threads. The Display, Xlib calls, and Slint window
// always remain on the installing/event-loop thread; XInitThreads is not needed.
struct Wake {
    fd: OwnedFd,
    queue: Mutex<Queue>,
}

impl Wake {
    fn new() -> io::Result<Self> {
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
            queue: Mutex::new(Queue {
                callbacks: VecDeque::with_capacity(CALLBACK_CAPACITY),
                quit: false,
                stopped: false,
                failure: None,
            }),
        })
    }

    fn signal(&self) -> io::Result<()> {
        let value = 1_u64;
        loop {
            let result = unsafe {
                libc::write(self.fd.as_raw_fd(), (&value as *const u64).cast(), 8)
            };
            if result == 8 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::Interrupted => continue,
                // A saturated eventfd is already readable, so no wake is lost.
                io::ErrorKind::WouldBlock => return Ok(()),
                _ => return Err(error),
            }
        }
    }

    fn drain(&self) -> Result<(), PlatformError> {
        let mut value = 0_u64;
        loop {
            let result = unsafe {
                libc::read(self.fd.as_raw_fd(), (&mut value as *mut u64).cast(), 8)
            };
            if result == 8 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => return Ok(()),
                _ => return Err(failure(format!("reading event-loop wake descriptor: {error}"))),
            }
        }
    }

    fn stop(&self) {
        let callbacks = match self.queue.lock() {
            Ok(mut queue) => {
                queue.stopped = true;
                std::mem::take(&mut queue.callbacks)
            }
            Err(_) => {
                eprintln!("kherdr: event-loop callback queue poisoned during shutdown");
                return;
            }
        };
        // Destruct captured application state without holding the queue lock.
        drop(callbacks);
    }
}

struct Proxy(Arc<Wake>);

impl EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), EventLoopError> {
        self.enqueue(None)
    }

    fn invoke_from_event_loop(&self, event: Callback) -> Result<(), EventLoopError> {
        self.enqueue(Some(event))
    }
}

impl Proxy {
    fn enqueue(&self, callback: Option<Callback>) -> Result<(), EventLoopError> {
        let mut queue = self.0.queue.lock().map_err(|_| EventLoopError::EventLoopTerminated)?;
        if queue.stopped || queue.failure.is_some() || queue.quit {
            return Err(EventLoopError::EventLoopTerminated);
        }
        if let Some(callback) = callback {
            if queue.callbacks.len() == CALLBACK_CAPACITY {
                // Slint has no queue-full error. Fail the loop explicitly rather
                // than silently losing accepted callbacks or blocking the GUI.
                queue.failure = Some("event-loop callback queue exceeded 1024 entries".into());
            } else {
                queue.callbacks.push_back(callback);
            }
        } else {
            queue.quit = true;
        }
        if let Err(error) = self.0.signal() {
            queue.failure = Some(format!("signalling event-loop wake descriptor: {error}"));
            eprintln!("kherdr: {}", queue.failure.as_deref().unwrap_or("wake failure"));
        }
        if queue.failure.is_some() {
            Err(EventLoopError::EventLoopTerminated)
        } else {
            Ok(())
        }
    }
}

struct XPlatform {
    window: Rc<MinimalSoftwareWindow>,
    native: RefCell<Option<NativeWindow>>,
    created: Cell<bool>,
    // Remain installed after run_event_loop returns, through main's cleanup.
    termination: TerminationSignals,
    wake: Arc<Wake>,
    started: Instant,
}

impl Platform for XPlatform {
    fn set_clipboard_text(&self, text: &str, clipboard: slint::platform::Clipboard) {
        if clipboard == slint::platform::Clipboard::DefaultClipboard {
            COPY_HANDLER.with(|slot| {
                if let Some(handler) = slot.borrow().as_ref() { handler(text); }
            });
        }
    }

    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        if self.created.replace(true) {
            return Err(failure("the Kindle platform supports exactly one application window"));
        }
        Ok(self.window.clone())
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Proxy(self.wake.clone())))
    }

    fn duration_since_start(&self) -> Duration {
        self.started.elapsed()
    }

    fn cursor_flash_cycle(&self) -> Duration {
        Duration::ZERO
    }

    fn run_event_loop(&self) -> Result<(), PlatformError> {
        let mut native = self.native.borrow_mut().take()
            .ok_or_else(|| failure("the Kindle event loop cannot be run twice"))?;
        let result = self.run(&mut native);
        self.wake.stop();
        eprintln!(
            "kherdr: window-exit renders={} puts={} pixels={} exposes={} resizes={}",
            native.renders, native.puts, native.pixels, native.exposes, native.resizes,
        );
        result
    }
}

impl XPlatform {
    fn run(&self, native: &mut NativeWindow) -> Result<(), PlatformError> {
        if self.termination.requested() {
            return Ok(());
        }
        if !self.created.get() {
            return Err(failure("create the Slint application before running its event loop"));
        }
        self.window.try_dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: 1.0 })?;
        self.window.set_size(native.size);
        self.window.request_redraw();
        if !self.window.is_visible() {
            return Ok(());
        }
        unsafe {
            (native.connection.xlib.XMapRaised)(native.connection.display, native.id);
            (native.connection.xlib.XFlush)(native.connection.display);
        }
        eprintln!(
            "kherdr: window-ready id=0x{:x} size={}x{} depth={} bpp={} palette={} backend=core-xlib-software",
            native.id, native.size.width, native.size.height, native.depth,
            native.image.as_ref().map_or(0, |image| unsafe { (*image.raw).bits_per_pixel }),
            native.palette_count,
        );

        loop {
            if self.termination.requested() {
                return Ok(());
            }
            self.wake.drain()?;
            // Only consume callbacks already queued at the start of this turn.
            // A streaming callback can reschedule itself; consuming those new
            // callbacks here used to defer X input and presentation for 1024
            // consecutive UI batches. Newly queued work runs after this draw,
            // without sleeping or delaying admission.
            let callback_count = self.wake.queue.lock()
                .map_err(|_| failure("event-loop callback queue poisoned"))?
                .callbacks.len();
            for _ in 0..callback_count.max(1) {
                if self.termination.requested() {
                    return Ok(());
                }
                let callback = {
                    let mut queue = self.wake.queue.lock()
                        .map_err(|_| failure("event-loop callback queue poisoned"))?;
                    if let Some(error) = queue.failure.take() {
                        queue.stopped = true;
                        return Err(failure(error));
                    }
                    if queue.quit {
                        return Ok(());
                    }
                    queue.callbacks.pop_front()
                };
                match callback {
                    Some(callback) => callback(),
                    None => break,
                }
            }
            slint::platform::update_timers_and_animations();
            for _ in 0..256 {
                if unsafe { (native.connection.xlib.XPending)(native.connection.display) } == 0 {
                    break;
                }
                let mut event = std::mem::MaybeUninit::<xlib::XEvent>::uninit();
                unsafe {
                    (native.connection.xlib.XNextEvent)(native.connection.display, event.as_mut_ptr());
                    if !native.event(event.assume_init(), &self.window)? {
                        return Ok(());
                    }
                }
            }
            if !self.window.is_visible() {
                return Ok(());
            }
            // The native window/WM is authoritative, not the root component's
            // preferred desktop size or a programmatic set_size request.
            if self.window.size() != native.size {
                self.window.set_size(native.size);
            }
            if native.mapped {
                native.draw(&self.window);
            }
            unsafe { (native.connection.xlib.XFlush)(native.connection.display) };

            let queued = {
                let queue = self.wake.queue.lock()
                    .map_err(|_| failure("event-loop callback queue poisoned"))?;
                !queue.callbacks.is_empty() || queue.quit || queue.failure.is_some()
            };
            if queued || unsafe { (native.connection.xlib.XPending)(native.connection.display) } > 0 {
                continue;
            }
            let mut deadline = slint::platform::duration_until_next_timer_update();
            if native.mapped && self.window.has_active_animations() {
                // Only real animations schedule frames; an idle UI sleeps forever.
                let frame = Duration::from_millis(16);
                deadline = Some(deadline.map_or(frame, |timer| timer.min(frame)));
                self.window.request_redraw();
            }
            let timeout = deadline.map_or(-1, |duration| {
                duration.as_millis().saturating_add(u128::from(duration.subsec_nanos() % 1_000_000 != 0))
                    .min(i32::MAX as u128) as i32
            });
            let mut fds = [
                libc::pollfd {
                    fd: unsafe { (native.connection.xlib.XConnectionNumber)(native.connection.display) },
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd { fd: self.wake.fd.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            ];
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(failure(format!("polling X11/event-loop descriptors: {error}")));
                }
            }
            for (name, fd) in ["X11", "event-loop wake"].iter().zip(&fds) {
                if fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                    return Err(failure(format!("{name} descriptor disconnected: poll flags {}", fd.revents)));
                }
            }
        }
    }
}

impl Drop for XPlatform {
    fn drop(&mut self) {
        self.wake.stop();
    }
}

struct Connection {
    xlib: xlib::Xlib,
    display: *mut xlib::Display,
}

impl Drop for Connection {
    fn drop(&mut self) {
        unsafe { (self.xlib.XCloseDisplay)(self.display) };
    }
}

struct Image {
    connection: Rc<Connection>,
    raw: *mut xlib::XImage,
}

impl Image {
    fn new(connection: Rc<Connection>, visual: *mut xlib::Visual, depth: i32, size: PhysicalSize)
        -> Result<Self, PlatformError>
    {
        let raw = unsafe {
            (connection.xlib.XCreateImage)(connection.display, visual, depth as u32,
                xlib::ZPixmap, 0, ptr::null_mut(), size.width, size.height, 32, 0)
        };
        if raw.is_null() {
            return Err(failure("XCreateImage failed"));
        }
        let image = Self { connection, raw };
        let layout = unsafe { &mut *raw };
        if !matches!(layout.bits_per_pixel, 8 | 16 | 24 | 32)
            || !matches!(layout.byte_order, xlib::LSBFirst | xlib::MSBFirst)
            || layout.xoffset != 0 || layout.bytes_per_line <= 0
            || (layout.bytes_per_line as usize) < size.width as usize * (layout.bits_per_pixel as usize / 8)
        {
            return Err(failure(format!("unsupported XImage layout: bpp={} stride={} order={} offset={}",
                layout.bits_per_pixel, layout.bytes_per_line, layout.byte_order, layout.xoffset)));
        }
        let bytes = (layout.bytes_per_line as usize).checked_mul(size.height as usize)
            .filter(|bytes| *bytes <= isize::MAX as usize)
            .ok_or_else(|| failure("XImage allocation size overflow"))?;
        // XDestroyImage calls free(data). Never hand it a Rust Vec allocation.
        layout.data = unsafe { libc::calloc(1, bytes).cast() };
        if layout.data.is_null() {
            return Err(failure(format!("cannot allocate {bytes} bytes for XImage")));
        }
        Ok(image)
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        unsafe { (self.connection.xlib.XDestroyImage)(self.raw) };
    }
}

#[derive(Clone, Copy)]
struct Rect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl Rect {
    fn full(size: PhysicalSize) -> Self {
        Self { x: 0, y: 0, width: size.width, height: size.height }
    }

    fn clipped(x: i32, y: i32, width: u32, height: u32, size: PhysicalSize) -> Option<Self> {
        let right = (i64::from(x) + i64::from(width)).clamp(0, i64::from(size.width)) as u32;
        let bottom = (i64::from(y) + i64::from(height)).clamp(0, i64::from(size.height)) as u32;
        let x = x.max(0) as u32;
        let y = y.max(0) as u32;
        (right > x && bottom > y).then(|| Self { x, y, width: right - x, height: bottom - y })
    }

    fn union(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Self { x, y, width: (self.x + self.width).max(other.x + other.width) - x,
            height: (self.y + self.height).max(other.y + other.height) - y }
    }

    fn contains(self, other: Self) -> bool {
        self.x <= other.x && self.y <= other.y
            && self.x + self.width >= other.x + other.width
            && self.y + self.height >= other.y + other.height
    }
}

struct NativeWindow {
    connection: Rc<Connection>,
    id: xlib::Window,
    gc: xlib::GC,
    visual: *mut xlib::Visual,
    depth: i32,
    colormap: xlib::Colormap,
    allocated_colors: Vec<c_ulong>,
    gray_pixels: [c_ulong; 256],
    palette_count: usize,
    wm_protocols: xlib::Atom,
    wm_delete: xlib::Atom,
    size: PhysicalSize,
    image: Option<Image>,
    buffer: Vec<Rgb8Pixel>,
    new_buffer: bool,
    mapped: bool,
    exposure: Option<Rect>,
    pressed_keys: [Option<SharedString>; 256],
    pressed_buttons: [Option<u32>; 3],
    renders: u64,
    puts: u64,
    pixels: u64,
    exposes: u64,
    resizes: u64,
}

impl NativeWindow {
    fn new() -> Result<Self, PlatformError> {
        let xlib = xlib::Xlib::open().map_err(|error| failure(format!("loading core Xlib: {error}")))?;
        let display = unsafe { (xlib.XOpenDisplay)(ptr::null()) };
        if display.is_null() {
            return Err(failure("XOpenDisplay failed; check DISPLAY and X authority"));
        }
        let connection = Rc::new(Connection { xlib, display });
        let x = &connection.xlib;
        let screen = unsafe { (x.XDefaultScreen)(display) };
        let visual = unsafe { (x.XDefaultVisual)(display, screen) };
        if visual.is_null() {
            return Err(failure("XDefaultVisual returned null"));
        }
        let width = unsafe { (x.XDisplayWidth)(display, screen) };
        let height = unsafe { (x.XDisplayHeight)(display, screen) };
        let depth = unsafe { (x.XDefaultDepth)(display, screen) };
        let colormap = unsafe { (x.XDefaultColormap)(display, screen) };
        let mut native = Self {
            connection: connection.clone(), id: 0, gc: ptr::null_mut(), visual, depth, colormap,
            allocated_colors: Vec::new(), gray_pixels: [0; 256], palette_count: 0,
            wm_protocols: 0, wm_delete: 0, size: PhysicalSize::new(0, 0), image: None,
            buffer: Vec::new(), new_buffer: true, mapped: false, exposure: None,
            pressed_keys: std::array::from_fn(|_| None),
            pressed_buttons: [None; 3],
            renders: 0, puts: 0, pixels: 0, exposes: 0, resizes: 0,
        };
        native.palette()?;
        native.resize(width, height)?;
        let root = unsafe { (x.XRootWindow)(display, screen) };
        native.id = unsafe {
            (x.XCreateSimpleWindow)(display, root, 0, 0, width as u32, height as u32,
                0, native.gray_pixels[0], native.gray_pixels[255])
        };
        if native.id == 0 {
            return Err(failure("XCreateSimpleWindow failed"));
        }
        native.gc = unsafe { (x.XCreateGC)(display, native.id, 0, ptr::null_mut()) };
        if native.gc.is_null() {
            return Err(failure("XCreateGC failed"));
        }
        unsafe {
            (x.XSetGraphicsExposures)(display, native.gc, xlib::False);
            (x.XStoreName)(display, native.id, TITLE.as_ptr());
            let mut class = xlib::XClassHint {
                res_name: c"kherdr".as_ptr().cast_mut(),
                res_class: c"Kherdr".as_ptr().cast_mut(),
            };
            (x.XSetClassHint)(display, native.id, &mut class);
            native.wm_protocols = (x.XInternAtom)(display, c"WM_PROTOCOLS".as_ptr(), xlib::False);
            native.wm_delete = (x.XInternAtom)(display, c"WM_DELETE_WINDOW".as_ptr(), xlib::False);
            if (x.XSetWMProtocols)(display, native.id, &mut native.wm_delete, 1) == 0 {
                return Err(failure("XSetWMProtocols failed"));
            }
            (x.XSelectInput)(display, native.id,
                xlib::ExposureMask | xlib::StructureNotifyMask | xlib::VisibilityChangeMask
                | xlib::ButtonPressMask | xlib::ButtonReleaseMask | xlib::PointerMotionMask
                | xlib::EnterWindowMask | xlib::LeaveWindowMask | xlib::KeyPressMask
                | xlib::KeyReleaseMask | xlib::FocusChangeMask);
        }
        Ok(native)
    }

    fn palette(&mut self) -> Result<(), PlatformError> {
        let visual = unsafe { &*self.visual };
        if visual.class == xlib::TrueColor && matches!(self.depth, 16 | 24 | 32) {
            for (gray, pixel) in self.gray_pixels.iter_mut().enumerate() {
                *pixel = channel(gray as u8, visual.red_mask)
                    | channel(gray as u8, visual.green_mask) | channel(gray as u8, visual.blue_mask);
            }
            return Ok(());
        }
        if self.depth != 8 || !matches!(visual.class,
            xlib::StaticGray | xlib::GrayScale | xlib::StaticColor | xlib::PseudoColor)
            || !(1..=256).contains(&visual.map_entries)
        {
            return Err(failure(format!("unsupported X visual class={} depth={} entries={}",
                visual.class, self.depth, visual.map_entries)));
        }
        let x = &self.connection.xlib;
        let display = self.connection.display;
        let mut colors = Vec::with_capacity(256);
        if matches!(visual.class, xlib::StaticGray | xlib::StaticColor) {
            for pixel in 0..visual.map_entries {
                colors.push(xlib::XColor { pixel: pixel as c_ulong, red: 0, green: 0, blue: 0,
                    flags: 0, pad: 0 });
            }
            unsafe { (x.XQueryColors)(display, self.colormap, colors.as_mut_ptr(), colors.len() as i32) };
        } else {
            // Hold a read-only allocation for every successful request so another
            // client cannot mutate/recycle pixels used by the backing image.
            for gray in 0..=255_u16 {
                let mut color = xlib::XColor { pixel: 0, red: gray * 257, green: gray * 257,
                    blue: gray * 257, flags: (xlib::DoRed | xlib::DoGreen | xlib::DoBlue) as _, pad: 0 };
                if unsafe { (x.XAllocColor)(display, self.colormap, &mut color) } != 0 {
                    self.allocated_colors.push(color.pixel);
                    colors.push(color);
                }
            }
        }
        if colors.is_empty() {
            return Err(failure("the default X colormap has no allocatable grayscale colors"));
        }
        self.palette_count = colors.len();
        for (gray, pixel) in self.gray_pixels.iter_mut().enumerate() {
            let target = gray as i64 * 257;
            let mut nearest = &colors[0];
            let mut nearest_distance = i64::MAX;
            for color in &colors {
                let r = i64::from(color.red) - target;
                let g = i64::from(color.green) - target;
                let b = i64::from(color.blue) - target;
                let distance = r * r + g * g + b * b;
                if distance < nearest_distance {
                    nearest = color;
                    nearest_distance = distance;
                }
            }
            *pixel = nearest.pixel;
        }
        if self.gray_pixels[0] == self.gray_pixels[255] {
            return Err(failure("the X colormap cannot represent distinct black and white pixels"));
        }
        Ok(())
    }

    fn resize(&mut self, width: i32, height: i32) -> Result<(), PlatformError> {
        if !(1..=32767).contains(&width) || !(1..=32767).contains(&height) {
            return Err(failure(format!("unsupported window dimensions {width}x{height}")));
        }
        let size = PhysicalSize::new(width as u32, height as u32);
        if size == self.size {
            return Ok(());
        }
        let image = Image::new(self.connection.clone(), self.visual, self.depth, size)?;
        let count = (width as usize).checked_mul(height as usize)
            .ok_or_else(|| failure("software framebuffer size overflow"))?;
        let mut buffer = Vec::new();
        buffer.try_reserve_exact(count)
            .map_err(|error| failure(format!("allocating software framebuffer: {error}")))?;
        buffer.resize(count, Rgb8Pixel { r: 255, g: 255, b: 255 });
        self.image = Some(image);
        self.buffer = buffer;
        self.size = size;
        self.new_buffer = true;
        self.exposure = Some(Rect::full(size));
        self.resizes += 1;
        Ok(())
    }

    fn draw(&mut self, window: &MinimalSoftwareWindow) {
        window.draw_if_needed(|renderer| {
            if self.new_buffer {
                renderer.set_repaint_buffer_type(RepaintBufferType::NewBuffer);
            }
            let dirty = renderer.render(&mut self.buffer, self.size.width as usize);
            self.renders += 1;
            if self.new_buffer {
                renderer.set_repaint_buffer_type(RepaintBufferType::ReusedBuffer);
                self.new_buffer = false;
                self.convert(Rect::full(self.size));
                self.present(Rect::full(self.size));
                self.exposure = None;
            } else {
                for (origin, size) in dirty.iter() {
                    if let Some(rect) = Rect::clipped(origin.x, origin.y, size.width, size.height, self.size) {
                        if let Some(changed) = self.convert(rect) {
                            // Expose restores the cached image after all damage
                            // has been converted; don't send its pixels twice.
                            if !self.exposure.is_some_and(|exposure| exposure.contains(changed)) {
                                self.present(changed);
                            }
                        }
                    }
                }
            }
        });
        // Exposed pixels must be copied even when Slint's scene is not dirty.
        if !self.new_buffer {
            if let Some(rect) = self.exposure.take() {
                self.present(rect);
            }
        }
    }

    fn convert(&mut self, rect: Rect) -> Option<Rect> {
        let image = self.image.as_ref()?;
        let layout = unsafe { &*image.raw };
        let bytes_per_pixel = layout.bits_per_pixel as usize / 8;
        let stride = layout.bytes_per_line as usize;
        let data = unsafe {
            std::slice::from_raw_parts_mut(layout.data.cast::<u8>(), stride * self.size.height as usize)
        };
        let mut left = rect.x + rect.width;
        let mut right = rect.x;
        let mut top = rect.y + rect.height;
        let mut bottom = rect.y;
        for y in rect.y..rect.y + rect.height {
            let source = y as usize * self.size.width as usize + rect.x as usize;
            let source = &self.buffer[source..source + rect.width as usize];
            let start = y as usize * stride + rect.x as usize * bytes_per_pixel;
            let target = &mut data[start..start + rect.width as usize * bytes_per_pixel];
            let mut row_left = rect.width;
            let mut row_right = 0;
            if bytes_per_pixel == 1 {
                // Kindle's native 8-bit visual needs one lookup and store, not
                // a per-pixel byte-order branch and variable-width byte loop.
                for (x, (rgb, target)) in source.iter().zip(target).enumerate() {
                    let pixel = self.gray_pixels[gray(*rgb)] as u8;
                    if *target != pixel {
                        *target = pixel;
                        row_left = row_left.min(x as u32);
                        row_right = x as u32 + 1;
                    }
                }
            } else {
                for (x, (rgb, target)) in source.iter().zip(target.chunks_exact_mut(bytes_per_pixel)).enumerate() {
                    let pixel = self.gray_pixels[gray(*rgb)];
                    let mut changed = false;
                    for (byte, target) in target.iter_mut().enumerate() {
                        let shift = if layout.byte_order == xlib::LSBFirst { byte } else { bytes_per_pixel - 1 - byte } * 8;
                        let value = (pixel >> shift) as u8;
                        changed |= *target != value;
                        *target = value;
                    }
                    if changed {
                        row_left = row_left.min(x as u32);
                        row_right = x as u32 + 1;
                    }
                }
            }
            if row_left < row_right {
                left = left.min(rect.x + row_left);
                right = right.max(rect.x + row_right);
                top = top.min(y);
                bottom = y + 1;
            }
        }
        (left < right).then(|| Rect { x: left, y: top, width: right - left, height: bottom - top })
    }

    fn present(&mut self, rect: Rect) {
        let Some(image) = self.image.as_ref() else { return };
        unsafe {
            (self.connection.xlib.XPutImage)(self.connection.display, self.id, self.gc, image.raw,
                rect.x as i32, rect.y as i32, rect.x as i32, rect.y as i32, rect.width, rect.height);
        }
        self.puts += 1;
        self.pixels += u64::from(rect.width) * u64::from(rect.height);
    }

    unsafe fn event(&mut self, event: xlib::XEvent, window: &MinimalSoftwareWindow)
        -> Result<bool, PlatformError>
    {
        // XNextEvent initialized the union; read only the member named by type_.
        let kind = unsafe { event.type_ };
        if kind == xlib::MappingNotify {
            let mut mapping = unsafe { event.mapping };
            unsafe { (self.connection.xlib.XRefreshKeyboardMapping)(&mut mapping) };
            return Ok(true);
        }
        if unsafe { event.any.window } != self.id {
            return Ok(true);
        }
        let mut release_duration_ms = 0;
        let translated = match kind {
            xlib::Expose => {
                let expose = unsafe { event.expose };
                self.exposes += 1;
                if let Some(rect) = Rect::clipped(expose.x, expose.y, expose.width.max(0) as u32,
                    expose.height.max(0) as u32, self.size)
                {
                    self.exposure = Some(self.exposure.map_or(rect, |old| old.union(rect)));
                }
                None
            }
            xlib::ConfigureNotify => {
                let configure = unsafe { event.configure };
                if configure.width > 0 && configure.height > 0 {
                    self.resize(configure.width, configure.height)?;
                    window.set_size(self.size);
                    window.request_redraw();
                }
                None
            }
            xlib::MapNotify => {
                self.mapped = true;
                self.exposure = Some(Rect::full(self.size));
                window.request_redraw();
                None
            }
            xlib::UnmapNotify => {
                self.mapped = false;
                self.pressed_buttons.fill(None);
                focus_changed(false);
                window.try_dispatch_event(WindowEvent::PointerExited)?;
                None
            }
            xlib::VisibilityNotify => {
                if unsafe { event.visibility.state } != xlib::VisibilityFullyObscured {
                    self.exposure = Some(Rect::full(self.size));
                    window.request_redraw();
                }
                None
            }
            xlib::DestroyNotify => { self.id = 0; return Ok(false); }
            xlib::ClientMessage => {
                let message = unsafe { event.client_message };
                if message.message_type == self.wm_protocols && message.format == 32
                    && message.data.get_long(0) as c_ulong == self.wm_delete
                {
                    Some(WindowEvent::CloseRequested)
                } else { None }
            }
            xlib::MotionNotify => {
                let motion = unsafe { event.motion };
                Some(WindowEvent::PointerMoved { position: LogicalPosition::new(motion.x as f32, motion.y as f32) })
            }
            xlib::EnterNotify => {
                let crossing = unsafe { event.crossing };
                Some(WindowEvent::PointerMoved { position: LogicalPosition::new(crossing.x as f32, crossing.y as f32) })
            }
            xlib::LeaveNotify => {
                self.pressed_buttons.fill(None);
                Some(WindowEvent::PointerExited)
            }
            xlib::ButtonPress | xlib::ButtonRelease => {
                let button = unsafe { event.button };
                let position = LogicalPosition::new(button.x as f32, button.y as f32);
                match button.button {
                    4..=7 if kind == xlib::ButtonPress => Some(WindowEvent::PointerScrolled {
                        position,
                        delta_x: match button.button { 6 => 80.0, 7 => -80.0, _ => 0.0 },
                        delta_y: match button.button { 4 => 80.0, 5 => -80.0, _ => 0.0 },
                    }),
                    1..=3 => {
                        let pressed = &mut self.pressed_buttons[button.button as usize - 1];
                        if kind == xlib::ButtonPress {
                            *pressed = Some(button.time as u32);
                        } else if let Some(start) = pressed.take() {
                            // X time is a wrapping 32-bit millisecond clock, not
                            // the time Slint finally admits a queued event. A
                            // slow render may delay both ends of a real hold.
                            // Reject backwards/ambiguous timestamps (>24 days).
                            release_duration_ms = i32::try_from(
                                (button.time as u32).wrapping_sub(start),
                            ).unwrap_or(0);
                        }
                        let button = match button.button {
                            1 => PointerEventButton::Left,
                            2 => PointerEventButton::Middle,
                            _ => PointerEventButton::Right,
                        };
                        Some(if kind == xlib::ButtonPress { WindowEvent::PointerPressed { position, button } }
                            else { WindowEvent::PointerReleased { position, button } })
                    }
                    _ => None,
                }
            }
            xlib::KeyPress | xlib::KeyRelease => {
                let mut key = unsafe { event.key };
                let code = key.keycode as usize;
                if code >= self.pressed_keys.len() { return Ok(true); }
                if kind == xlib::KeyRelease {
                    // Core X auto-repeat is Release+Press with identical time.
                    // Consume neither here; preserve key state for the next Press.
                    if unsafe { (self.connection.xlib.XPending)(self.connection.display) } > 0 {
                        let mut next = std::mem::MaybeUninit::<xlib::XEvent>::uninit();
                        unsafe { (self.connection.xlib.XPeekEvent)(self.connection.display, next.as_mut_ptr()) };
                        let next = unsafe { next.assume_init() };
                        if unsafe { next.type_ } == xlib::KeyPress {
                            let next_key = unsafe { next.key };
                            if next_key.window == key.window && next_key.keycode == key.keycode && next_key.time == key.time {
                                return Ok(true);
                            }
                        }
                    }
                    self.pressed_keys[code].take().map(|text| {
                        track_modifier(&text, false);
                        WindowEvent::KeyReleased { text }
                    })
                } else if let Some(text) = self.pressed_keys[code].clone() {
                    Some(WindowEvent::KeyPressRepeated { text })
                } else {
                    let text = key_text(&self.connection, &mut key);
                    self.pressed_keys[code] = text.clone();
                    if let Some(text) = &text { track_modifier(text, true); }
                    text.map(|text| WindowEvent::KeyPressed { text })
                }
            }
            xlib::FocusIn => {
                focus_changed(true);
                Some(WindowEvent::WindowActiveChanged(true))
            }
            xlib::FocusOut => {
                self.pressed_buttons.fill(None);
                focus_changed(false);
                window.try_dispatch_event(WindowEvent::PointerExited)?;
                for text in &mut self.pressed_keys {
                    if let Some(text) = text.take() {
                        window.try_dispatch_event(WindowEvent::KeyReleased { text })?;
                    }
                }
                Some(WindowEvent::WindowActiveChanged(false))
            }
            _ => None,
        };
        if let Some(event) = translated {
            POINTER_RELEASE_MS.with(|duration| {
                duration.set(release_duration_ms);
                let result = window.try_dispatch_event(event);
                duration.set(0);
                result
            })?;
        }
        Ok(true)
    }
}

impl Drop for NativeWindow {
    fn drop(&mut self) {
        self.image.take();
        unsafe {
            let x = &self.connection.xlib;
            let display = self.connection.display;
            if !self.gc.is_null() { (x.XFreeGC)(display, self.gc); }
            if self.id != 0 { (x.XDestroyWindow)(display, self.id); }
            if !self.allocated_colors.is_empty() {
                (x.XFreeColors)(display, self.colormap, self.allocated_colors.as_mut_ptr(),
                    self.allocated_colors.len() as i32, 0);
            }
            (x.XFlush)(display);
        }
    }
}

fn channel(gray: u8, mask: c_ulong) -> c_ulong {
    if mask == 0 { return 0; }
    let shift = mask.trailing_zeros();
    let maximum = (mask >> shift) as u64;
    ((((u64::from(gray) * maximum + 127) / 255) as c_ulong) << shift) & mask
}

fn key_text(connection: &Connection, key: &mut xlib::XKeyEvent) -> Option<SharedString> {
    let mut symbol = 0;
    let mut bytes = [0_i8; 32];
    // Slint tracks modifier key events itself. Keep Shift/Caps/NumLock for
    // printable lookup but avoid XLookupString turning Ctrl+C into byte 0x03.
    let mut lookup = *key;
    lookup.state &= !(xlib::ControlMask | xlib::Mod1Mask | xlib::Mod4Mask);
    let length = unsafe { (connection.xlib.XLookupString)(&mut lookup, bytes.as_mut_ptr().cast(),
        bytes.len() as i32, &mut symbol, ptr::null_mut()) };
    let special = match symbol as u32 {
        keysym::XK_BackSpace => Key::Backspace,
        keysym::XK_Tab => Key::Tab,
        keysym::XK_ISO_Left_Tab => Key::Backtab,
        keysym::XK_Return | keysym::XK_KP_Enter => Key::Return,
        keysym::XK_Escape => Key::Escape,
        keysym::XK_Delete | keysym::XK_KP_Delete => Key::Delete,
        keysym::XK_Insert | keysym::XK_KP_Insert => Key::Insert,
        keysym::XK_Home | keysym::XK_KP_Home => Key::Home,
        keysym::XK_End | keysym::XK_KP_End => Key::End,
        keysym::XK_Page_Up | keysym::XK_KP_Page_Up => Key::PageUp,
        keysym::XK_Page_Down | keysym::XK_KP_Page_Down => Key::PageDown,
        keysym::XK_Left | keysym::XK_KP_Left => Key::LeftArrow,
        keysym::XK_Right | keysym::XK_KP_Right => Key::RightArrow,
        keysym::XK_Up | keysym::XK_KP_Up => Key::UpArrow,
        keysym::XK_Down | keysym::XK_KP_Down => Key::DownArrow,
        keysym::XK_Shift_L => Key::Shift,
        keysym::XK_Shift_R => Key::ShiftR,
        keysym::XK_Control_L => Key::Control,
        keysym::XK_Control_R => Key::ControlR,
        keysym::XK_Alt_L | keysym::XK_Alt_R => Key::Alt,
        keysym::XK_ISO_Level3_Shift => Key::AltGr,
        keysym::XK_Meta_L | keysym::XK_Super_L => Key::Meta,
        keysym::XK_Meta_R | keysym::XK_Super_R => Key::MetaR,
        keysym::XK_Caps_Lock => Key::CapsLock,
        keysym::XK_F1 => Key::F1,
        keysym::XK_F2 => Key::F2,
        keysym::XK_F3 => Key::F3,
        keysym::XK_F4 => Key::F4,
        keysym::XK_F5 => Key::F5,
        keysym::XK_F6 => Key::F6,
        keysym::XK_F7 => Key::F7,
        keysym::XK_F8 => Key::F8,
        keysym::XK_F9 => Key::F9,
        keysym::XK_F10 => Key::F10,
        keysym::XK_F11 => Key::F11,
        keysym::XK_F12 => Key::F12,
        _ => {
            if length <= 0 { return None; }
            let mut text = String::with_capacity(length as usize);
            for byte in bytes.iter().take(length as usize) {
                let character = *byte as u8;
                if character >= 32 && character != 127 {
                    // XLookupString returns Latin-1, not UTF-8.
                    text.push(char::from(character));
                }
            }
            return (!text.is_empty()).then(|| text.into());
        }
    };
    Some(special.into())
}
