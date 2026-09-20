//! Single-thread-owned adapter for the pinned libghostty-vt C API.
//! Snapshot rows replace complete backing rows; an empty cell still clears its
//! background. The cursor is a separate overlay, not baked into the row pixels.

use std::{cell::UnsafeCell, collections::HashMap, ffi::c_void, marker::PhantomData,
    mem, panic::{AssertUnwindSafe, catch_unwind}, ptr, rc::Rc, sync::LazyLock};

#[allow(non_camel_case_types, non_snake_case, non_upper_case_globals, dead_code)]
mod ffi {
    include!(concat!(env!("OUT_DIR"), "/ghostty_vt.rs"));
}
use ffi::*;

pub type Result<T, E = String> = std::result::Result<T, E>;
const RESPONSE_LIMIT: usize = 32 * 1024;
const IMAGE_LIMIT: usize = 32 * 1024 * 1024;

// The native allocator owns this allocation on success only. Keeping the guard
// inside catch_unwind also frees it if a decoder unexpectedly panics.
struct NativePixels {
    allocator: *const GhosttyAllocator,
    data: *mut u8,
    len: usize,
}

impl Drop for NativePixels {
    fn drop(&mut self) {
        unsafe { ghostty_free(self.allocator, self.data, self.len); }
    }
}

fn rgba_size(width: u32, height: u32) -> Option<usize> {
    if width == 0 || height == 0 { return None; }
    usize::try_from(width).ok()?.checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4).filter(|&len| len <= IMAGE_LIMIT)
}

unsafe extern "C" fn decode_png(
    _userdata: *mut c_void, allocator: *const GhosttyAllocator,
    data: *const u8, data_len: usize, output: *mut GhosttySysImage,
) -> bool {
    if data.is_null() || output.is_null() || data_len == 0 || data_len > IMAGE_LIMIT {
        return false;
    }
    catch_unwind(AssertUnwindSafe(|| {
        // Native input is borrowed only for this synchronous callback.
        let bytes = unsafe { std::slice::from_raw_parts(data, data_len) };
        let mut decoder = png::Decoder::new_with_limits(std::io::Cursor::new(bytes),
            png::Limits { bytes: IMAGE_LIMIT });
        decoder.set_ignore_text_chunk(true);
        decoder.set_ignore_iccp_chunk(true);
        decoder.set_transformations(png::Transformations::normalize_to_color8());
        let header = decoder.read_header_info().ok()?;
        let (width, height) = (header.width, header.height);
        let len = rgba_size(width, height)?;
        // The png limit excludes the caller's output buffer. Budget it here
        // before read_info allocates row/decompression buffers or reads metadata.
        decoder.set_limits(png::Limits { bytes: IMAGE_LIMIT.checked_sub(len)? });
        let mut reader = decoder.read_info().ok()?;
        if reader.output_buffer_size()? > len { return None; }
        let pixels = NativePixels { allocator, data: unsafe { ghostty_alloc(allocator, len) }, len };
        if pixels.data.is_null() { return None; }
        // Interlaced decoding can read/modify partial destination bytes, so the
        // entire allocation must be initialized before making a Rust slice.
        unsafe { ptr::write_bytes(pixels.data, 0, len); }
        let rgba = unsafe { std::slice::from_raw_parts_mut(pixels.data, len) };
        let decoded = reader.next_frame(rgba).ok()?;
        if decoded.width != width || decoded.height != height
            || decoded.bit_depth != png::BitDepth::Eight { return None; }
        let channels = match decoded.color_type {
            png::ColorType::Rgba => 4,
            png::ColorType::Rgb => 3,
            png::ColorType::GrayscaleAlpha => 2,
            png::ColorType::Grayscale => 1,
            png::ColorType::Indexed => return None,
        };
        if decoded.buffer_size() != (len / 4).checked_mul(channels)? { return None; }
        // Expand backward in the final native buffer, never overwriting a
        // source pixel before reading it and never allocating a second image.
        if channels != 4 {
            for index in (0..len / 4).rev() {
                let source = index * channels;
                let value = match channels {
                    3 => [rgba[source], rgba[source + 1], rgba[source + 2], 255],
                    2 => [rgba[source], rgba[source], rgba[source], rgba[source + 1]],
                    _ => [rgba[source], rgba[source], rgba[source], 255],
                };
                rgba[index * 4..index * 4 + 4].copy_from_slice(&value);
            }
        }
        reader.finish().ok()?;
        unsafe { output.write(GhosttySysImage { width, height, data: pixels.data, data_len: len }); }
        mem::forget(pixels); // Ownership transfers to Ghostty only here.
        Some(())
    })).ok().flatten().is_some()
}

fn configure_image_system() -> Result<()> {
    static CONFIGURED: LazyLock<Result<()>> = LazyLock::new(|| {
        let mut enabled = false;
        check(unsafe { ghostty_build_info(GHOSTTY_BUILD_INFO_KITTY_GRAPHICS, out(&mut enabled)) },
            "querying native Kitty graphics support")?;
        if !enabled { return Err("libghostty-vt was built without Kitty graphics support".into()); }
        check(unsafe { ghostty_sys_set(GHOSTTY_SYS_OPT_DECODE_PNG,
            decode_png as *const () as *const c_void) }, "installing native PNG decoder")
    });
    CONFIGURED.clone()
}

struct PlacementIterator(GhosttyKittyGraphicsPlacementIterator);

impl Drop for PlacementIterator {
    fn drop(&mut self) {
        unsafe { ghostty_kitty_graphics_placement_iterator_free(self.0); }
    }
}

struct Responses {
    // Preallocated once: the C callback never allocates, formats, or panics.
    bytes: Vec<u8>,
    used: usize,
    failed: bool,
}

unsafe extern "C" fn write_pty(
    _terminal: GhosttyTerminal, userdata: *mut c_void, data: *const u8, len: usize,
) {
    if userdata.is_null() || len == 0 { return; }
    // USERDATA points at the stable UnsafeCell allocation, never at Terminal.
    // Ghostty calls synchronously; no Rust access to the collector overlaps it.
    let responses = unsafe { &mut *userdata.cast::<Responses>() };
    if responses.failed { return; }
    if data.is_null() || len > responses.bytes.len().saturating_sub(responses.used) {
        responses.failed = true;
        return;
    }
    // Bounds were checked above; no slices, allocation, or fallible operations
    // occur across this C ABI boundary. The native bytes are callback-borrowed.
    unsafe { ptr::copy_nonoverlapping(data, responses.bytes.as_mut_ptr().add(responses.used), len); }
    responses.used += len;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorStyle { Block, Bar, Underline }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub column: u16,
    pub row: u16,
    pub style: CursorStyle,
}

#[derive(Debug)]
pub struct Cell {
    pub column: u16,
    /// Wide leads cover two columns, clipped to the viewport. Tails are omitted.
    pub width: u8,
    pub text: String,
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub background_is_default: bool,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
}

#[derive(Debug)]
pub struct Row { pub index: u16, pub cells: Vec<Cell> }

#[derive(Debug)]
pub struct ImageData {
    pub id: u32,
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct ImagePlacement {
    pub image: Rc<ImageData>,
    pub placement_id: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub source_x: f64,
    pub source_y: f64,
    pub source_width: f64,
    pub source_height: f64,
    pub z: i32,
}

struct VirtualPlacement {
    image_id: u32,
    placement_id: u32,
    columns: u32,
    rows: u32,
}

// One row-local run, following graphics_unicode.zig's IncompletePlacement.
// Omitted diacritics continue only a compatible immediately preceding run.
struct PlaceholderRun {
    x: u16,
    y: u16,
    image_low: u32,
    image_high: Option<u8>,
    placement_id: u32,
    row: Option<u32>,
    col: Option<u32>,
    width: u32,
}

impl PlaceholderRun {
    fn append(&mut self, next: &Self) -> bool {
        if self.image_low != next.image_low || self.placement_id != next.placement_id
            || next.row.is_some() && next.row != self.row
            || next.col.is_some() && next.col != self.col.and_then(|col| col.checked_add(self.width))
            || next.image_high.is_some() && next.image_high != self.image_high { return false; }
        self.width += 1;
        true
    }

    fn start(mut self) -> Self {
        self.row = Some(self.row.unwrap_or(0));
        self.col = Some(self.col.unwrap_or(0));
        self
    }
}

// Protocol diacritic order from the pinned graphics_unicode.zig; indices are
// the encoded row, column, or high image-ID byte, not Unicode numeric values.
fn placeholder_diacritic(character: char) -> Option<u32> {
    const DIACRITICS: &[u32] = &[
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A, 0x034B, 0x034C,
    0x0350, 0x0351, 0x0352, 0x0357, 0x035B, 0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369,
    0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F, 0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592,
    0x0593, 0x0594, 0x0595, 0x0597, 0x0598, 0x0599, 0x059C, 0x059D, 0x059E, 0x059F, 0x05A0, 0x05A1,
    0x05A8, 0x05A9, 0x05AB, 0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065A, 0x065B, 0x065D, 0x065E, 0x06D6, 0x06D7, 0x06D8,
    0x06D9, 0x06DA, 0x06DB, 0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2, 0x06E4, 0x06E7, 0x06E8, 0x06EB,
    0x06EC, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736, 0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743,
    0x0745, 0x0747, 0x0749, 0x074A, 0x07EB, 0x07EC, 0x07ED, 0x07EE, 0x07EF, 0x07F0, 0x07F1, 0x07F3,
    0x0816, 0x0817, 0x0818, 0x0819, 0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A, 0x082B, 0x082C, 0x082D, 0x0951, 0x0953, 0x0954,
    0x0F82, 0x0F83, 0x0F86, 0x0F87, 0x135D, 0x135E, 0x135F, 0x17DD, 0x193A, 0x1A17, 0x1A75, 0x1A76,
    0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C, 0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F, 0x1B70, 0x1B71,
    0x1B72, 0x1B73, 0x1CD0, 0x1CD1, 0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1, 0x1DC3, 0x1DC4,
    0x1DC5, 0x1DC6, 0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5,
    0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF, 0x1DE0, 0x1DE1,
    0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1, 0x20D4, 0x20D5, 0x20D6, 0x20D7,
    0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0, 0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2,
    0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6, 0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB, 0x2DEC, 0x2DED, 0x2DEE,
    0x2DEF, 0x2DF0, 0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8, 0x2DF9, 0x2DFA,
    0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1, 0xA8E0, 0xA8E1,
    0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9, 0xA8EA, 0xA8EB, 0xA8EC, 0xA8ED,
    0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2, 0xAAB3, 0xAAB7, 0xAAB8, 0xAABE, 0xAABF, 0xAAC1,
    0xFE20, 0xFE21, 0xFE22, 0xFE23, 0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38, 0x1D185, 0x1D186, 0x1D187,
    0x1D188, 0x1D189, 0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242, 0x1D243, 0x1D244,
    ];
    DIACRITICS.binary_search(&(character as u32)).ok().map(|index| index as u32)
}

fn placeholder_color_id(color: GhosttyStyleColor) -> Result<u32> {
    match color.tag {
        GHOSTTY_STYLE_COLOR_NONE => Ok(0),
        GHOSTTY_STYLE_COLOR_PALETTE => Ok(u32::from(unsafe { color.value.palette })),
        GHOSTTY_STYLE_COLOR_RGB => {
            let color = unsafe { color.value.rgb };
            Ok(u32::from(color.r) << 16 | u32::from(color.g) << 8 | u32::from(color.b))
        }
        _ => Err("unknown native placeholder color tag".into()),
    }
}

#[derive(Debug)]
pub struct Snapshot {
    pub cols: u16,
    pub rows: u16,
    pub full: bool,
    pub changed_rows: Vec<Row>,
    /// None retains the previous model; Some(empty) clears it.
    pub images: Option<Vec<ImagePlacement>>,
    pub cursor: Option<Cursor>,
    pub default_foreground: [u8; 3],
    pub default_background: [u8; 3],
}

pub struct Terminal {
    terminal: GhosttyTerminal,
    render: GhosttyRenderState,
    iterator: GhosttyRenderStateRowIterator,
    cells: GhosttyRenderStateRowCells,
    image_cache: HashMap<u64, Rc<ImageData>>,
    responses: Box<UnsafeCell<Responses>>,
    cols: u16,
    rows: u16,
    cell_size: Option<(u32, u32)>,
    pending: bool,
    full: bool,
    cursor: Option<Cursor>,
    foreground: [u8; 3],
    background: [u8; 3],
    // A C failure can occur after mutating state. Fail closed until a fresh
    // reset succeeds instead of pretending the previous dimensions/data live on.
    failed: bool,
    // Explicitly !Send + !Sync, even if a future bindings generator changes the
    // opaque handle representation. All native access stays on the owner thread.
    _owner: PhantomData<Rc<()>>,
}

fn check(result: GhosttyResult, operation: &str) -> Result<()> {
    if result == GHOSTTY_SUCCESS { Ok(()) }
    else { Err(format!("{operation}: libghostty-vt error {result}")) }
}

fn out<T>(value: &mut T) -> *mut c_void { ptr::from_mut(value).cast() }
fn input<T>(value: &T) -> *const c_void { ptr::from_ref(value).cast() }
fn rgb(value: GhosttyColorRgb) -> [u8; 3] { [value.r, value.g, value.b] }

// The concrete output types at each invocation are part of the pinned ABI.
macro_rules! render_get {
    ($self:expr, $kind:ident, $ty:ty) => {{
        let mut value: $ty = unsafe { mem::zeroed() };
        check(unsafe { ghostty_render_state_get($self.render, $kind, out(&mut value)) }, stringify!($kind))?;
        value
    }};
}

impl Terminal {
    pub fn new(cols: u16, rows: u16) -> Result<Self> {
        if cols == 0 || rows == 0 { return Err("terminal dimensions must be nonzero".into()); }
        configure_image_system()?;
        // Every partially-created handle is covered by Drop, including a handle
        // returned alongside an error. The C destructors explicitly accept NULL.
        let mut this = Self {
            terminal: ptr::null_mut(), render: ptr::null_mut(), iterator: ptr::null_mut(),
            cells: ptr::null_mut(),
            image_cache: HashMap::new(),
            responses: Box::new(UnsafeCell::new(Responses {
                bytes: byte_buffer(RESPONSE_LIMIT)?, used: 0, failed: false,
            })),
            cols, rows, cell_size: None, pending: true, full: true, cursor: None,
            foreground: [0; 3], background: [255; 3], failed: false, _owner: PhantomData,
        };
        unsafe {
            check(ghostty_terminal_new(ptr::null(), &mut this.terminal,
                GhosttyTerminalOptions { cols, rows, max_scrollback: 0 }), "creating terminal")?;
            check(ghostty_render_state_new(ptr::null(), &mut this.render), "creating render state")?;
            check(ghostty_render_state_row_iterator_new(ptr::null(), &mut this.iterator), "creating row iterator")?;
            check(ghostty_render_state_row_cells_new(ptr::null(), &mut this.cells), "creating cell iterator")?;
        }
        if this.terminal.is_null() || this.render.is_null() || this.iterator.is_null()
            || this.cells.is_null() {
            return Err("libghostty-vt returned a NULL handle on success".into());
        }
        this.configure()?;
        Ok(this)
    }

    fn configure(&mut self) -> Result<()> {
        let foreground = GhosttyColorRgb { r: 0, g: 0, b: 0 };
        let background = GhosttyColorRgb { r: 255, g: 255, b: 255 };
        let disabled = false;
        let image_limit = IMAGE_LIMIT as u64;
        unsafe {
            // Pointer-valued options take the pointer itself, NOT its address.
            check(ghostty_terminal_set(self.terminal, GHOSTTY_TERMINAL_OPT_USERDATA,
                self.responses.get().cast()), "setting response userdata")?;
            check(ghostty_terminal_set(self.terminal, GHOSTTY_TERMINAL_OPT_WRITE_PTY,
                write_pty as *const () as *const c_void), "setting response callback")?;
            check(ghostty_terminal_set(self.terminal, GHOSTTY_TERMINAL_OPT_COLOR_FOREGROUND, input(&foreground)), "setting default foreground")?;
            check(ghostty_terminal_set(self.terminal, GHOSTTY_TERMINAL_OPT_COLOR_BACKGROUND, input(&background)), "setting default background")?;
            check(ghostty_terminal_set(self.terminal, GHOSTTY_TERMINAL_OPT_DEFAULT_CURSOR_BLINK, input(&disabled)), "disabling default cursor blink")?;
            check(ghostty_terminal_set(self.terminal, GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_STORAGE_LIMIT, input(&image_limit)), "enabling bounded Kitty image storage")?;
            for option in [GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_FILE,
                GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_TEMP_FILE,
                GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_SHARED_MEM] {
                check(ghostty_terminal_set(self.terminal, option, input(&disabled)), "disabling external image sources")?;
            }
        }
        Ok(())
    }

    fn ready(&self) -> Result<()> {
        if unsafe { (*self.responses.get()).failed } {
            Err("terminal response collection failed or exceeded 32 KiB; reset required".into())
        } else if self.failed { Err("terminal state failed; reset required before further use".into()) }
        else { Ok(()) }
    }

    pub fn reset(&mut self) -> Result<()> {
        // Fresh native objects also discard partial parser sequences and render
        // references. Failed allocation never re-enables the previous attachment.
        self.failed = true;
        let mut replacement = Self::new(self.cols, self.rows)?;
        if let Some((width, height)) = self.cell_size { replacement.set_cell_size(width, height)?; }
        *self = replacement;
        Ok(())
    }

    /// Supply rendered cell dimensions for graphics and genuine PTY size reports.
    pub fn set_cell_size(&mut self, width: u32, height: u32) -> Result<()> {
        self.ready()?;
        if width == 0 || height == 0 { return Err("terminal cell dimensions must be nonzero".into()); }
        self.check_pixel_size(self.cols, self.rows, width, height)?;
        if self.cell_size == Some((width, height)) { return Ok(()); }
        self.failed = true;
        check(unsafe { ghostty_terminal_resize(self.terminal, self.cols, self.rows, width, height) }, "setting terminal pixel geometry")?;
        self.cell_size = Some((width, height));
        self.pending = true;
        self.full = true;
        self.failed = false;
        self.ready()
    }

    fn check_pixel_size(&self, cols: u16, rows: u16, width: u32, height: u32) -> Result<()> {
        // Mouse positions are f32 and native SGR-pixel conversion uses i32.
        // Stay in the exactly representable integer range before native casts.
        if u64::from(cols) * u64::from(width) > (1 << 24)
            || u64::from(rows) * u64::from(height) > (1 << 24) {
            return Err("terminal pixel dimensions exceed mouse coordinate range".into());
        }
        Ok(())
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.ready()?;
        if cols == 0 || rows == 0 { return Err("terminal dimensions must be nonzero".into()); }
        if (cols, rows) == (self.cols, self.rows) { return Ok(()); }
        let (width, height) = self.cell_size.unwrap_or((0, 0));
        self.check_pixel_size(cols, rows, width, height)?;
        self.failed = true;
        check(unsafe { ghostty_terminal_resize(self.terminal, cols, rows, width, height) }, "resizing terminal")?;
        self.cols = cols;
        self.rows = rows;
        self.pending = true;
        self.full = true;
        self.failed = false;
        self.ready()
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Result<()> {
        self.ready()?;
        if !bytes.is_empty() {
            // This pinned function is synchronous and infallible by its public
            // contract. Parser errors are handled inside Ghostty, not reported
            // through an invented success/error return value. No bytes coalesce.
            unsafe { ghostty_terminal_vt_write(self.terminal, bytes.as_ptr(), bytes.len()); }
            self.pending = true;
        }
        self.ready()
    }

    /// Drain complete native responses after each feed or geometry update.
    /// Overflow fails the terminal closed: never return a truncated reply batch.
    pub fn take_responses(&mut self) -> Result<Vec<u8>> {
        self.ready()?;
        let responses = self.responses.get_mut();
        let mut bytes = byte_buffer(responses.used)?;
        bytes.copy_from_slice(&responses.bytes[..responses.used]);
        responses.used = 0;
        Ok(bytes)
    }

    /// Repaint a retained session after another session occupied the display.
    pub fn invalidate(&mut self) {
        self.pending = true;
        self.full = true;
    }

    pub fn snapshot(&mut self) -> Result<Snapshot> {
        self.ready()?;
        if !self.pending {
            return Ok(Snapshot { cols: self.cols, rows: self.rows, full: false,
                changed_rows: Vec::new(), images: None, cursor: self.cursor,
                default_foreground: self.foreground, default_background: self.background });
        }
        let result = self.read_snapshot();
        if result.is_err() { self.failed = true; }
        result
    }

    fn read_snapshot(&mut self) -> Result<Snapshot> {
        check(unsafe { ghostty_render_state_update(self.render, self.terminal) }, "updating render state")?;
        let cols = render_get!(self, GHOSTTY_RENDER_STATE_DATA_COLS, u16);
        let rows = render_get!(self, GHOSTTY_RENDER_STATE_DATA_ROWS, u16);
        if (cols, rows) != (self.cols, self.rows) { return Err("render viewport geometry mismatch".into()); }
        let dirty = render_get!(self, GHOSTTY_RENDER_STATE_DATA_DIRTY, GhosttyRenderStateDirty);
        if dirty != GHOSTTY_RENDER_STATE_DIRTY_FALSE && dirty != GHOSTTY_RENDER_STATE_DIRTY_PARTIAL
            && dirty != GHOSTTY_RENDER_STATE_DIRTY_FULL { return Err("unknown render dirty state".into()); }
        // The pinned RenderState::Colors already applies DEC mode 5 to default
        // colors. Explicit cell colors stay explicit; do NOT invert DEC5 twice.
        let foreground = rgb(render_get!(self, GHOSTTY_RENDER_STATE_DATA_COLOR_FOREGROUND, GhosttyColorRgb));
        let background = rgb(render_get!(self, GHOSTTY_RENDER_STATE_DATA_COLOR_BACKGROUND, GhosttyColorRgb));
        let full = self.full || dirty == GHOSTTY_RENDER_STATE_DIRTY_FULL
            || foreground != self.foreground || background != self.background;
        let visible = render_get!(self, GHOSTTY_RENDER_STATE_DATA_CURSOR_VISIBLE, bool);
        let positioned = render_get!(self, GHOSTTY_RENDER_STATE_DATA_CURSOR_VIEWPORT_HAS_VALUE, bool);
        let cursor = if visible && positioned {
            let mut column = render_get!(self, GHOSTTY_RENDER_STATE_DATA_CURSOR_VIEWPORT_X, u16);
            let row = render_get!(self, GHOSTTY_RENDER_STATE_DATA_CURSOR_VIEWPORT_Y, u16);
            let tail = render_get!(self, GHOSTTY_RENDER_STATE_DATA_CURSOR_VIEWPORT_WIDE_TAIL, bool);
            if tail { column = column.saturating_sub(1); }
            let style = match render_get!(self, GHOSTTY_RENDER_STATE_DATA_CURSOR_VISUAL_STYLE, GhosttyRenderStateCursorVisualStyle) {
                GHOSTTY_RENDER_STATE_CURSOR_VISUAL_STYLE_BAR => CursorStyle::Bar,
                GHOSTTY_RENDER_STATE_CURSOR_VISUAL_STYLE_UNDERLINE => CursorStyle::Underline,
                GHOSTTY_RENDER_STATE_CURSOR_VISUAL_STYLE_BLOCK | GHOSTTY_RENDER_STATE_CURSOR_VISUAL_STYLE_BLOCK_HOLLOW => CursorStyle::Block,
                _ => return Err("unknown cursor visual style".into()),
            };
            if column >= cols || row >= rows { return Err("cursor outside render viewport".into()); }
            Some(Cursor { column, row, style })
        } else { None };
        let mut changed_rows = Vec::new();
        // These APIs populate PREALLOCATED handles: pass &handle, not handle.
        check(unsafe { ghostty_render_state_get(self.render, GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR, out(&mut self.iterator)) }, "populating row iterator")?;
        let mut index = 0usize;
        while unsafe { ghostty_render_state_row_iterator_next(self.iterator) } {
            if index >= usize::from(rows) { return Err("too many render rows".into()); }
            let mut row_dirty = false;
            check(unsafe { ghostty_render_state_row_get(self.iterator, GHOSTTY_RENDER_STATE_ROW_DATA_DIRTY, out(&mut row_dirty)) }, "reading row dirty state")?;
            if full || row_dirty {
                changed_rows.try_reserve(1).map_err(|e| format!("allocating changed rows: {e}"))?;
                changed_rows.push(self.read_row(index as u16, foreground, background)?);
            }
            index += 1;
        }
        if index != usize::from(rows) { return Err("missing render rows".into()); }
        let images = Some(self.read_images()?);
        // Build the entire owned snapshot before acknowledging ANY row. A failed
        // cell read must never erase dirty information for undelivered rows.
        check(unsafe { ghostty_render_state_get(self.render, GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR, out(&mut self.iterator)) }, "rewinding row iterator for acknowledgement")?;
        let clean_row = false;
        while unsafe { ghostty_render_state_row_iterator_next(self.iterator) } {
            check(unsafe { ghostty_render_state_row_set(self.iterator, GHOSTTY_RENDER_STATE_ROW_OPTION_DIRTY, input(&clean_row)) }, "acknowledging row")?;
        }
        let clean: GhosttyRenderStateDirty = GHOSTTY_RENDER_STATE_DIRTY_FALSE;
        check(unsafe { ghostty_render_state_set(self.render, GHOSTTY_RENDER_STATE_OPTION_DIRTY, input(&clean)) }, "acknowledging frame")?;
        self.pending = false;
        self.full = false;
        self.cursor = cursor;
        self.foreground = foreground;
        self.background = background;
        Ok(Snapshot { cols, rows, full, changed_rows, images, cursor,
            default_foreground: foreground, default_background: background })
    }

    fn read_images(&mut self) -> Result<Vec<ImagePlacement>> {
        // No storage/image/placement borrow survives this function. Even an
        // unchanged storage generation requires fresh geometry after scrolling.
        let mut graphics: GhosttyKittyGraphics = ptr::null_mut();
        check(unsafe { ghostty_terminal_get(self.terminal, GHOSTTY_TERMINAL_DATA_KITTY_GRAPHICS,
            out(&mut graphics)) }, "reading active Kitty graphics storage")?;
        if graphics.is_null() { return Err("native Kitty graphics storage is NULL".into()); }
        let mut iterator = PlacementIterator(ptr::null_mut());
        check(unsafe { ghostty_kitty_graphics_placement_iterator_new(ptr::null(), &mut iterator.0) },
            "creating image placement iterator")?;
        if iterator.0.is_null() { return Err("native image placement iterator is NULL".into()); }
        check(unsafe { ghostty_kitty_graphics_get(graphics, GHOSTTY_KITTY_GRAPHICS_DATA_PLACEMENT_ITERATOR,
            out(&mut iterator.0)) }, "populating image placement iterator")?;
        let mut placements = Vec::new();
        let mut virtuals = Vec::new();
        let mut cache: HashMap<u64, Rc<ImageData>> = HashMap::new();
        let mut cached_bytes = 0usize;
        while unsafe { ghostty_kitty_graphics_placement_next(iterator.0) } {
            let mut image_id = 0u32;
            let mut placement_id = 0u32;
            let mut virtual_placement = false;
            let mut offset_x = 0u32;
            let mut offset_y = 0u32;
            let mut z = 0i32;
            let keys = [GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_IMAGE_ID,
                GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_PLACEMENT_ID,
                GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_IS_VIRTUAL,
                GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_X_OFFSET,
                GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_Y_OFFSET,
                GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_Z];
            let mut values = [out(&mut image_id), out(&mut placement_id), out(&mut virtual_placement),
                out(&mut offset_x), out(&mut offset_y), out(&mut z)];
            check(unsafe { ghostty_kitty_graphics_placement_get_multi(iterator.0, keys.len(),
                keys.as_ptr(), values.as_mut_ptr(), ptr::null_mut()) }, "reading image placement")?;
            if virtual_placement {
                let mut columns = 0u32;
                let mut rows = 0u32;
                check(unsafe { ghostty_kitty_graphics_placement_get(iterator.0,
                    GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_COLUMNS, out(&mut columns)) }, "reading virtual placement columns")?;
                check(unsafe { ghostty_kitty_graphics_placement_get(iterator.0,
                    GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_ROWS, out(&mut rows)) }, "reading virtual placement rows")?;
                virtuals.try_reserve(1).map_err(|e| format!("allocating virtual placements: {e}"))?;
                virtuals.push(VirtualPlacement { image_id, placement_id, columns, rows });
                continue;
            }
            let handle = unsafe { ghostty_kitty_graphics_image(graphics, image_id) };
            if handle.is_null() { return Err("native image placement references a missing image".into()); }
            let (cell_width, cell_height) = self.cell_size
                .ok_or_else(|| "rendered cell dimensions required for Kitty graphics".to_owned())?;
            let mut geometry: GhosttyKittyGraphicsPlacementRenderInfo = unsafe { mem::zeroed() };
            geometry.size = mem::size_of::<GhosttyKittyGraphicsPlacementRenderInfo>();
            check(unsafe { ghostty_kitty_graphics_placement_render_info(iterator.0, handle,
                self.terminal, &mut geometry) }, "resolving image placement geometry")?;
            if !geometry.viewport_visible || geometry.pixel_width == 0 || geometry.pixel_height == 0
                || geometry.source_width == 0 || geometry.source_height == 0 { continue; }
            let x = i64::from(geometry.viewport_col) * i64::from(cell_width) + i64::from(offset_x);
            let y = i64::from(geometry.viewport_row) * i64::from(cell_height) + i64::from(offset_y);
            if x >= i64::from(self.cols) * i64::from(cell_width)
                || y >= i64::from(self.rows) * i64::from(cell_height)
                || x + i64::from(geometry.pixel_width) <= 0
                || y + i64::from(geometry.pixel_height) <= 0 { continue; }
            let x = i32::try_from(x).map_err(|_| "image x coordinate exceeds render range")?;
            let y = i32::try_from(y).map_err(|_| "image y coordinate exceeds render range")?;
            let image = self.cache_image(handle, image_id, &mut cache, &mut cached_bytes)?;
            placements.try_reserve(1).map_err(|e| format!("allocating image placements: {e}"))?;
            placements.push(ImagePlacement { image, placement_id, x, y,
                width: geometry.pixel_width, height: geometry.pixel_height,
                source_x: f64::from(geometry.source_x), source_y: f64::from(geometry.source_y),
                source_width: f64::from(geometry.source_width), source_height: f64::from(geometry.source_height), z });
        }
        if !virtuals.is_empty() {
            self.read_virtual_images(graphics, &virtuals, &mut placements, &mut cache, &mut cached_bytes)?;
        }
        placements.sort_by_key(|placement| (placement.z, placement.image.id, placement.placement_id));
        // Only visible images remain owned. Empty/deleted/alternate-screen
        // storage clears both the renderer's model and this decoded cache.
        self.image_cache = cache;
        Ok(placements)
    }

    fn cache_image(&mut self, handle: GhosttyKittyGraphicsImage, image_id: u32,
        cache: &mut HashMap<u64, Rc<ImageData>>, cached_bytes: &mut usize) -> Result<Rc<ImageData>> {
        let mut generation = 0u64;
        check(unsafe { ghostty_kitty_graphics_image_get(handle, GHOSTTY_KITTY_IMAGE_DATA_GENERATION,
            out(&mut generation)) }, "reading image generation")?;
        if generation == 0 { return Err("stored native image has no generation".into()); }
        if let Some(image) = cache.get(&generation) { return Ok(Rc::clone(image)); }
        let image = if let Some(image) = self.image_cache.get(&generation) {
            Rc::clone(image)
        } else {
            // UI snapshots may still own the previous generation, but this
            // cache need not retain it while allocating its replacement.
            self.image_cache.retain(|_, image| image.id != image_id);
            Rc::new(Self::read_image(handle, image_id, generation, IMAGE_LIMIT - *cached_bytes)?)
        };
        *cached_bytes = cached_bytes.checked_add(image.rgba.len())
            .filter(|&bytes| bytes <= IMAGE_LIMIT)
            .ok_or_else(|| "visible RGBA image cache exceeds 32 MiB".to_owned())?;
        cache.try_reserve(1).map_err(|e| format!("allocating image cache: {e}"))?;
        cache.insert(generation, Rc::clone(&image));
        Ok(image)
    }

    fn read_virtual_images(&mut self, graphics: GhosttyKittyGraphics, virtuals: &[VirtualPlacement],
        placements: &mut Vec<ImagePlacement>, cache: &mut HashMap<u64, Rc<ImageData>>,
        cached_bytes: &mut usize) -> Result<()> {
        // Scan native render cells, not input escape sequences. This also
        // resolves placeholders moved by terminal scrolling/editing operations.
        check(unsafe { ghostty_render_state_get(self.render, GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR,
            out(&mut self.iterator)) }, "rewinding rows for virtual graphics")?;
        let mut row = 0u16;
        while unsafe { ghostty_render_state_row_iterator_next(self.iterator) } {
            let mut run: Option<PlaceholderRun> = None;
            check(unsafe { ghostty_render_state_row_get(self.iterator, GHOSTTY_RENDER_STATE_ROW_DATA_CELLS,
                out(&mut self.cells)) }, "reading virtual graphics row")?;
            for column in 0..self.cols {
                check(unsafe { ghostty_render_state_row_cells_select(self.cells, column) }, "selecting placeholder cell")?;
                let mut raw: GhosttyCell = unsafe { mem::zeroed() };
                let mut codepoint = 0u32;
                check(unsafe { ghostty_render_state_row_cells_get(self.cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW,
                    out(&mut raw)) }, "reading placeholder cell")?;
                check(unsafe { ghostty_cell_get(raw, GHOSTTY_CELL_DATA_CODEPOINT, out(&mut codepoint)) },
                    "reading placeholder codepoint")?;
                if codepoint != 0x10eeee {
                    if let Some(previous) = run.take() {
                        self.place_virtual_run(graphics, virtuals, previous, placements, cache, cached_bytes)?;
                    }
                    continue;
                }
                let mut style: GhosttyStyle = unsafe { mem::zeroed() };
                style.size = mem::size_of::<GhosttyStyle>();
                check(unsafe { ghostty_render_state_row_cells_get(self.cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE,
                    out(&mut style)) }, "reading placeholder IDs")?;
                let text = self.cell_text()?;
                let mut diacritics = text.chars().skip(1);
                let next = PlaceholderRun {
                    x: column, y: row, width: 1,
                    image_low: placeholder_color_id(style.fg_color)?,
                    placement_id: placeholder_color_id(style.underline_color)?,
                    row: diacritics.next().and_then(placeholder_diacritic),
                    col: diacritics.next().and_then(placeholder_diacritic),
                    image_high: diacritics.next().and_then(placeholder_diacritic)
                        .and_then(|value| u8::try_from(value).ok()),
                };
                if run.as_mut().is_some_and(|previous| previous.append(&next)) { continue; }
                if let Some(previous) = run.take() {
                    self.place_virtual_run(graphics, virtuals, previous, placements, cache, cached_bytes)?;
                }
                run = Some(next.start());
            }
            if let Some(previous) = run {
                self.place_virtual_run(graphics, virtuals, previous, placements, cache, cached_bytes)?;
            }
            row = row.checked_add(1).ok_or("too many placeholder rows")?;
        }
        Ok(())
    }

    fn place_virtual_run(&mut self, graphics: GhosttyKittyGraphics, virtuals: &[VirtualPlacement],
        run: PlaceholderRun, placements: &mut Vec<ImagePlacement>,
        cache: &mut HashMap<u64, Rc<ImageData>>, cached_bytes: &mut usize) -> Result<()> {
        let image_id = run.image_low | u32::from(run.image_high.unwrap_or(0)) << 24;
        // ID zero chooses the first native virtual placement for this image,
        // preserving storage iterator order as graphics_unicode.zig does.
        let Some(placement) = virtuals.iter().find(|placement| placement.image_id == image_id
            && (run.placement_id == 0 || run.placement_id == placement.placement_id)) else { return Ok(()); };
        let handle = unsafe { ghostty_kitty_graphics_image(graphics, image_id) };
        // Stale/deleted placeholder text is blank, not a terminal failure.
        if handle.is_null() { return Ok(()); }
        let (cell_width, cell_height) = self.cell_size
            .ok_or_else(|| "rendered cell dimensions required for Kitty graphics".to_owned())?;
        let mut width = 0u32;
        let mut height = 0u32;
        check(unsafe { ghostty_kitty_graphics_image_get(handle, GHOSTTY_KITTY_IMAGE_DATA_WIDTH,
            out(&mut width)) }, "reading virtual image width")?;
        check(unsafe { ghostty_kitty_graphics_image_get(handle, GHOSTTY_KITTY_IMAGE_DATA_HEIGHT,
            out(&mut height)) }, "reading virtual image height")?;
        if width == 0 || height == 0 { return Err("native virtual image has zero dimensions".into()); }
        let cols = if placement.columns == 0 { width.div_ceil(cell_width) } else { placement.columns };
        let rows = if placement.rows == 0 { height.div_ceil(cell_height) } else { placement.rows };
        // Native graphics_unicode grid casts to CellCountInt (u16); invalid
        // placement grids are skipped by the native renderer as well.
        if cols == 0 || rows == 0 || cols > u32::from(u16::MAX) || rows > u32::from(u16::MAX) { return Ok(()); }
        let grid_width = f64::from(cols) * f64::from(cell_width);
        let grid_height = f64::from(rows) * f64::from(cell_height);
        let scale = (grid_width / f64::from(width)).min(grid_height / f64::from(height));
        let padding_x = (grid_width - f64::from(width) * scale) / 2.0;
        let padding_y = (grid_height - f64::from(height) * scale) / 2.0;
        let run_x = f64::from(run.col.unwrap_or(0)) * f64::from(cell_width);
        let run_y = f64::from(run.row.unwrap_or(0)) * f64::from(cell_height);
        // Equivalent to native renderPlacement's scaled-source/letterbox
        // clipping, expressed as an intersection in destination pixel space.
        let left = run_x.max(padding_x);
        let top = run_y.max(padding_y);
        let right = (run_x + f64::from(run.width) * f64::from(cell_width))
            .min(padding_x + f64::from(width) * scale);
        let bottom = (run_y + f64::from(cell_height)).min(padding_y + f64::from(height) * scale);
        if right <= left || bottom <= top { return Ok(()); }
        // Preserve fractional texel coordinates: rounding here would erase
        // visible fragments when a small image spans many placeholder cells.
        let source_x = ((left - padding_x) / scale).clamp(0.0, f64::from(width));
        let source_y = ((top - padding_y) / scale).clamp(0.0, f64::from(height));
        let source_width = ((right - left) / scale).min(f64::from(width) - source_x);
        let source_height = ((bottom - top) / scale).min(f64::from(height) - source_y);
        let dest_width = (right - left).round() as u32;
        let dest_height = (bottom - top).round() as u32;
        if source_width <= 0.0 || source_height <= 0.0 || dest_width == 0 || dest_height == 0 { return Ok(()); }
        let x = u64::from(run.x) * u64::from(cell_width) + (left - run_x).round() as u64;
        let y = u64::from(run.y) * u64::from(cell_height) + (top - run_y).round() as u64;
        let x = i32::try_from(x).map_err(|_| "virtual image x exceeds render range")?;
        let y = i32::try_from(y).map_err(|_| "virtual image y exceeds render range")?;
        let image = self.cache_image(handle, image_id, cache, cached_bytes)?;
        placements.try_reserve(1).map_err(|e| format!("allocating virtual image fragments: {e}"))?;
        placements.push(ImagePlacement { image, placement_id: placement.placement_id, x, y,
            width: dest_width, height: dest_height, source_x, source_y, source_width, source_height,
            // Pinned renderer/image.zig assigns Unicode placeholders z=-1.
            z: -1 });
        Ok(())
    }

    fn read_image(handle: GhosttyKittyGraphicsImage, id: u32, generation: u64,
        available: usize) -> Result<ImageData> {
        let mut width = 0u32;
        let mut height = 0u32;
        let mut format: GhosttyKittyImageFormat = GHOSTTY_KITTY_IMAGE_FORMAT_RGBA;
        let mut compression: GhosttyKittyImageCompression = GHOSTTY_KITTY_IMAGE_COMPRESSION_NONE;
        let mut data: *const u8 = ptr::null();
        let mut data_len = 0usize;
        let keys = [GHOSTTY_KITTY_IMAGE_DATA_WIDTH, GHOSTTY_KITTY_IMAGE_DATA_HEIGHT,
            GHOSTTY_KITTY_IMAGE_DATA_FORMAT, GHOSTTY_KITTY_IMAGE_DATA_COMPRESSION,
            GHOSTTY_KITTY_IMAGE_DATA_DATA_PTR, GHOSTTY_KITTY_IMAGE_DATA_DATA_LEN];
        let mut values = [out(&mut width), out(&mut height), out(&mut format),
            out(&mut compression), out(&mut data), out(&mut data_len)];
        check(unsafe { ghostty_kitty_graphics_image_get_multi(handle, keys.len(), keys.as_ptr(),
            values.as_mut_ptr(), ptr::null_mut()) }, "reading native image pixels")?;
        let len = rgba_size(width, height).filter(|&len| len <= available)
            .ok_or_else(|| "visible RGBA image cache exceeds 32 MiB or has invalid dimensions".to_owned())?;
        let channels = match format {
            GHOSTTY_KITTY_IMAGE_FORMAT_RGBA => 4,
            GHOSTTY_KITTY_IMAGE_FORMAT_RGB => 3,
            GHOSTTY_KITTY_IMAGE_FORMAT_GRAY_ALPHA => 2,
            GHOSTTY_KITTY_IMAGE_FORMAT_GRAY => 1,
            _ => return Err("native image has an unsupported decoded pixel format".into()),
        };
        if compression != GHOSTTY_KITTY_IMAGE_COMPRESSION_NONE || data.is_null()
            || data_len != (len / 4) * channels {
            return Err("native image pixel buffer violates its decoded format contract".into());
        }
        let source = unsafe { std::slice::from_raw_parts(data, data_len) };
        let mut rgba = Vec::new();
        rgba.try_reserve_exact(len).map_err(|e| format!("allocating owned image pixels: {e}"))?;
        if channels == 4 {
            rgba.extend_from_slice(source);
        } else {
            for pixel in source.chunks_exact(channels) {
                let value = match channels {
                    3 => [pixel[0], pixel[1], pixel[2], 255],
                    2 => [pixel[0], pixel[0], pixel[0], pixel[1]],
                    _ => [pixel[0], pixel[0], pixel[0], 255],
                };
                rgba.extend_from_slice(&value);
            }
        }
        Ok(ImageData { id, generation, width, height, rgba })
    }

    fn read_row(&mut self, index: u16, default_fg: [u8; 3], default_bg: [u8; 3]) -> Result<Row> {
        check(unsafe { ghostty_render_state_row_get(self.iterator, GHOSTTY_RENDER_STATE_ROW_DATA_CELLS, out(&mut self.cells)) }, "populating cell iterator")?;
        let mut cells = Vec::new();
        cells.try_reserve_exact(usize::from(self.cols)).map_err(|e| format!("allocating row cells: {e}"))?;
        for column in 0..self.cols {
            check(unsafe { ghostty_render_state_row_cells_select(self.cells, column) }, "selecting render cell")?;
            let mut raw: GhosttyCell = unsafe { mem::zeroed() };
            let mut wide: GhosttyCellWide = GHOSTTY_CELL_WIDE_NARROW;
            check(unsafe { ghostty_render_state_row_cells_get(self.cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW, out(&mut raw)) }, "reading raw cell")?;
            check(unsafe { ghostty_cell_get(raw, GHOSTTY_CELL_DATA_WIDE, out(&mut wide)) }, "reading cell width")?;
            if wide == GHOSTTY_CELL_WIDE_SPACER_TAIL { continue; }
            if wide != GHOSTTY_CELL_WIDE_NARROW && wide != GHOSTTY_CELL_WIDE_WIDE
                && wide != GHOSTTY_CELL_WIDE_SPACER_HEAD { return Err("unknown cell width".into()); }
            let width = if wide == GHOSTTY_CELL_WIDE_WIDE && column < self.cols - 1 { 2 } else { 1 };
            let mut style: GhosttyStyle = unsafe { mem::zeroed() };
            style.size = mem::size_of::<GhosttyStyle>();
            check(unsafe { ghostty_render_state_row_cells_get(self.cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE, out(&mut style)) }, "reading cell style")?;
            let (mut foreground, _) = self.cell_color(GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_FG_COLOR, default_fg)?;
            let (mut background, default_background) = self.cell_color(GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_BG_COLOR, default_bg)?;
            if style.inverse { mem::swap(&mut foreground, &mut background); }
            if style.faint {
                for channel in 0..3 {
                    foreground[channel] = ((u16::from(foreground[channel]) * 7 + u16::from(background[channel]) * 3) / 10) as u8;
                }
            }
            let mut text = if style.invisible || wide == GHOSTTY_CELL_WIDE_SPACER_HEAD { String::new() }
                else { self.cell_text()? };
            let placeholder = text.starts_with('\u{10eeee}');
            if placeholder { text.clear(); }
            cells.push(Cell { column, width, text, foreground, background,
                background_is_default: default_background && !style.inverse,
                bold: style.bold, italic: style.italic,
                underline: !style.invisible && !placeholder && style.underline != GHOSTTY_SGR_UNDERLINE_NONE as i32,
                strikethrough: !style.invisible && !placeholder && style.strikethrough });
        }
        Ok(Row { index, cells })
    }

    fn cell_color(&self, kind: GhosttyRenderStateRowCellsData, fallback: [u8; 3]) -> Result<([u8; 3], bool)> {
        let mut color = GhosttyColorRgb { r: 0, g: 0, b: 0 };
        let result = unsafe { ghostty_render_state_row_cells_get(self.cells, kind, out(&mut color)) };
        if result == GHOSTTY_INVALID_VALUE { return Ok((fallback, true)); }
        check(result, "resolving cell color")?;
        Ok((rgb(color), false))
    }

    fn cell_text(&self) -> Result<String> {
        let mut small = [0u8; 64];
        let mut buffer = GhosttyBuffer { ptr: small.as_mut_ptr(), cap: small.len(), len: 0 };
        let result = unsafe { ghostty_render_state_row_cells_get(self.cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_UTF8, out(&mut buffer)) };
        if result == GHOSTTY_OUT_OF_SPACE {
            let mut bytes = byte_buffer(buffer.len)?;
            buffer.ptr = bytes.as_mut_ptr();
            buffer.cap = bytes.len();
            buffer.len = 0;
            check(unsafe { ghostty_render_state_row_cells_get(self.cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_UTF8, out(&mut buffer)) }, "reading full grapheme")?;
            if buffer.len > bytes.len() { return Err("grapheme length exceeds destination".into()); }
            bytes.truncate(buffer.len);
            return String::from_utf8(bytes).map_err(|e| format!("invalid rendered grapheme UTF-8: {e}"));
        }
        check(result, "reading grapheme")?;
        if buffer.len > small.len() { return Err("grapheme length exceeds destination".into()); }
        let text = std::str::from_utf8(&small[..buffer.len]).map_err(|e| format!("invalid rendered grapheme UTF-8: {e}"))?;
        let mut owned = String::new();
        owned.try_reserve_exact(text.len()).map_err(|e| format!("allocating grapheme: {e}"))?;
        owned.push_str(text);
        Ok(owned)
    }

}

impl Drop for Terminal {
    fn drop(&mut self) {
        unsafe {
            ghostty_render_state_row_cells_free(self.cells);
            ghostty_render_state_row_iterator_free(self.iterator);
            ghostty_render_state_free(self.render);
            // The response userdata allocation stays alive throughout native
            // destruction, then Rust drops it with the rest of the fields.
            ghostty_terminal_free(self.terminal);
        }
    }
}

fn byte_buffer(size: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(size).map_err(|e| format!("allocating terminal buffer: {e}"))?;
    bytes.resize(size, 0);
    Ok(bytes)
}
