use std::ffi::CStr;
use std::time::{Duration, Instant};

type MotionEvent = unsafe extern "C" fn(
    *mut x11_dl::xlib::Display, libc::c_int, libc::c_int, libc::c_int, libc::c_ulong,
) -> libc::c_int;
type ButtonEvent = unsafe extern "C" fn(
    *mut x11_dl::xlib::Display, libc::c_uint, libc::c_int, libc::c_ulong,
) -> libc::c_int;

struct Library(*mut libc::c_void);

impl Library {
    fn symbol(&self, name: &CStr) -> Result<*mut libc::c_void, String> {
        unsafe {
            libc::dlerror();
            let symbol = libc::dlsym(self.0, name.as_ptr());
            let error = libc::dlerror();
            if !error.is_null() {
                return Err(CStr::from_ptr(error).to_string_lossy().into_owned());
            }
            if symbol.is_null() {
                return Err(format!("missing symbol {}", name.to_string_lossy()));
            }
            Ok(symbol)
        }
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        unsafe { libc::dlclose(self.0); }
    }
}

struct DisplayConnection {
    // Xlib may register XTest callbacks used by XCloseDisplay. Drop this only
    // after the Drop body has closed the display, and before unloading Xlib.
    library: Option<Library>,
    xlib: x11_dl::xlib::Xlib,
    display: *mut x11_dl::xlib::Display,
}

impl Drop for DisplayConnection {
    fn drop(&mut self) {
        unsafe { (self.xlib.XCloseDisplay)(self.display); }
    }
}

struct PressedButton<'a> {
    connection: &'a DisplayConnection,
    button: ButtonEvent,
    pressed: bool,
}

impl Drop for PressedButton<'_> {
    fn drop(&mut self) {
        if self.pressed {
            unsafe {
                (self.button)(self.connection.display, 1, 0, 0);
                (self.connection.xlib.XSync)(self.connection.display, 0);
            }
        }
    }
}

fn phase(start: Instant, name: &str) {
    println!("{name} monotonic_elapsed_us={}", start.elapsed().as_micros());
}

fn drag(args: &[String]) -> Result<(), String> {
    if args.len() != 7 || args[0] != "drag" {
        return Err("usage: x11-device-probe [drag X1 Y1 X2 Y2 HOLD_MS MOVE_MS]".into());
    }
    let coordinate = |index: usize| -> Result<i32, String> {
        args[index].parse().map_err(|_| format!("invalid coordinate: {}", args[index]))
    };
    let duration = |index: usize| -> Result<u64, String> {
        let value = args[index].parse::<u64>()
            .map_err(|_| format!("invalid duration: {}", args[index]))?;
        if value > 5000 {
            return Err("HOLD_MS and MOVE_MS must each be at most 5000".into());
        }
        Ok(value)
    };
    let (x1, y1, x2, y2) = (coordinate(1)?, coordinate(2)?, coordinate(3)?, coordinate(4)?);
    let (hold_ms, move_ms) = (duration(5)?, duration(6)?);
    let xlib = x11_dl::xlib::Xlib::open().map_err(|error| format!("Xlib: {error}"))?;
    let display = unsafe { (xlib.XOpenDisplay)(std::ptr::null()) };
    if display.is_null() {
        return Err("cannot open DISPLAY".into());
    }
    let mut connection = DisplayConnection { library: None, xlib, display };
    let screen = unsafe { (connection.xlib.XDefaultScreen)(display) };
    let width = unsafe { (connection.xlib.XDisplayWidth)(display, screen) };
    let height = unsafe { (connection.xlib.XDisplayHeight)(display, screen) };
    for (x, y) in [(x1, y1), (x2, y2)] {
        if x < 0 || y < 0 || x >= width || y >= height {
            return Err(format!("coordinate ({x}, {y}) outside screen {screen}: {width}x{height}"));
        }
    }
    let handle = unsafe { libc::dlopen(c"libXtst.so.6".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if handle.is_null() {
        let error = unsafe { libc::dlerror() };
        let detail = if error.is_null() {
            "unknown loader error".into()
        } else {
            unsafe { CStr::from_ptr(error) }.to_string_lossy().into_owned()
        };
        return Err(format!("libXtst.so.6: {detail}"));
    }
    let library = connection.library.insert(Library(handle));
    let motion = unsafe {
        std::mem::transmute::<*mut libc::c_void, MotionEvent>(library.symbol(c"XTestFakeMotionEvent")?)
    };
    let button = unsafe {
        std::mem::transmute::<*mut libc::c_void, ButtonEvent>(library.symbol(c"XTestFakeButtonEvent")?)
    };
    let start = Instant::now();
    if unsafe { motion(display, screen, x1, y1, 0) } == 0 {
        return Err("XTestFakeMotionEvent failed positioning pointer".into());
    }
    unsafe { (connection.xlib.XSync)(display, 0); }
    phase(start, "READY");
    // Arm before sending the press: even a failed press attempt takes the release path.
    let mut guard = PressedButton { connection: &connection, button, pressed: true };
    if unsafe { button(display, 1, 1, 0) } == 0 {
        return Err("XTestFakeButtonEvent failed pressing button".into());
    }
    unsafe { (connection.xlib.XSync)(display, 0); }
    phase(start, "PRESSED");
    std::thread::sleep(Duration::from_millis(hold_ms));
    let movement_start = Instant::now();
    // At most 250 motions, with absolute deadlines so processing time cannot accumulate.
    let steps = move_ms.div_ceil(20).max(1);
    for step in 1..=steps {
        let deadline = movement_start + Duration::from_micros(move_ms * 1000 * step / steps);
        std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        let interpolate = |from: i32, to: i32| -> i32 {
            (i64::from(from) + (i64::from(to) - i64::from(from)) * step as i64 / steps as i64) as i32
        };
        if unsafe { motion(display, screen, interpolate(x1, x2), interpolate(y1, y2), 0) } == 0 {
            return Err("XTestFakeMotionEvent failed moving pointer".into());
        }
        unsafe { (connection.xlib.XFlush)(display); }
    }
    unsafe { (connection.xlib.XSync)(display, 0); }
    phase(start, "MOVED");
    if unsafe { button(display, 1, 0, 0) } == 0 {
        return Err("XTestFakeButtonEvent failed releasing button".into());
    }
    unsafe { (connection.xlib.XSync)(display, 0); }
    guard.pressed = false;
    phase(start, "RELEASED");
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        diagnostics();
    } else if let Err(error) = drag(&args) {
        eprintln!("x11-device-probe: {error}");
        std::process::exit(2);
    }
}

fn diagnostics() {
    macro_rules! check {
        ($label:literal, $load:expr) => {
            match $load {
                Ok(_) => println!("{}: available", $label),
                Err(error) => println!("{}: {error:?}", $label),
            }
        };
    }
    check!("Xlib", x11_dl::xlib::Xlib::open());
    check!("Xcursor", x11_dl::xcursor::Xcursor::open());
    check!("XInput2", x11_dl::xinput2::XInput2::open());
    check!("Xrandr", x11_dl::xrandr::Xrandr::open());
    check!("Xrender", x11_dl::xrender::Xrender::open());
    check!("Xlib-xcb", x11_dl::xlib_xcb::Xlib_xcb::open());
    if let Ok(xlib) = x11_dl::xlib::Xlib::open() {
        unsafe {
            let display = (xlib.XOpenDisplay)(std::ptr::null());
            if !display.is_null() {
                let screen = (xlib.XDefaultScreen)(display);
                println!("screen={} width={} height={} depth={}", screen,
                    (xlib.XDisplayWidth)(display, screen),
                    (xlib.XDisplayHeight)(display, screen),
                    (xlib.XDefaultDepth)(display, screen));
                (xlib.XCloseDisplay)(display);
            } else {
                println!("cannot open DISPLAY");
            }
        }
    }
}
