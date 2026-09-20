use crate::terminal::ImagePlacement;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

const MAX_SURFACE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_PLACEMENTS: usize = 4096;
const MAX_PIXEL_WORK: u64 = 64 * 1024 * 1024;
const SAMPLE_CHUNK: usize = 256;

pub struct Sprite {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub image: Image,
}

#[derive(Default)]
pub struct Layers {
    pub below_background: Vec<Sprite>,
    pub below_text: Vec<Sprite>,
    pub above_text: Vec<Sprite>,
}

#[derive(Default)]
pub struct GraphicsRenderer {
    images: Vec<ImagePlacement>,
    viewport: Option<(u32, u32)>,
}

impl GraphicsRenderer {
    /// None retains the previous placements. An unchanged scene returns None;
    /// an empty Layers value explicitly clears all three UI image models.
    /// Errors leave the last successfully rendered scene intact.
    pub fn update(
        &mut self,
        mut images: Option<Vec<ImagePlacement>>,
        viewport_width: u32,
        viewport_height: u32,
    ) -> Result<Option<Layers>, String> {
        if let Some(placements) = images.as_mut() {
            validate(placements)?;
            if placements.windows(2).any(|pair| order(&pair[0]) > order(&pair[1])) {
                // Equal keys retain their original order (virtual fragments can
                // share a placement ID). Usually the native snapshot is sorted.
                placements.sort_by_key(order);
            }
        }
        let placements = images.as_deref().unwrap_or(&self.images);
        let viewport = (viewport_width, viewport_height);
        if self.viewport == Some(viewport)
            && (images.is_none() || same_scene(placements, &self.images))
        {
            return Ok(None);
        }

        let mut bounds: [Option<Bounds>; 3] = [None; 3];
        let mut work = 0_u64;
        for placement in placements {
            if let Some(visible) = Bounds::visible(placement, viewport) {
                work = work.checked_add(visible.area()).ok_or("Kitty pixel work overflow")?;
                if work > MAX_PIXEL_WORK {
                    return Err(format!("Kitty images require {work} composited pixels; limit is {MAX_PIXEL_WORK}"));
                }
                let slot = &mut bounds[layer(placement.z)];
                *slot = Some(slot.map_or(visible, |old| old.union(visible)));
            }
        }
        let bytes = bounds.iter().flatten().try_fold(0_u64, |total, bound| {
            bound.area().checked_mul(4).and_then(|size| total.checked_add(size))
        }).ok_or("Kitty surface size overflow")?;
        if bytes > MAX_SURFACE_BYTES {
            return Err(format!("Kitty image surfaces require {bytes} bytes; limit is {MAX_SURFACE_BYTES}"));
        }
        // Include transparent bounding-box clearing in the work budget.
        if work + bytes / 4 > MAX_PIXEL_WORK {
            return Err(format!("Kitty compositing and surface clearing exceed {MAX_PIXEL_WORK} pixels"));
        }

        let mut layers = Layers::default();
        for (index, bound) in bounds.into_iter().enumerate() {
            let Some(bound) = bound else { continue };
            // All dimensions and the combined allocation were checked before
            // allocating any surface. No full-image grayscale copy is needed.
            let mut pixels = SharedPixelBuffer::<Rgba8Pixel>::new(bound.width(), bound.height());
            let target = pixels.make_mut_slice();
            for placement in placements.iter().filter(|placement| layer(placement.z) == index) {
                if let Some(visible) = Bounds::visible(placement, viewport) {
                    composite(target, bound, visible, placement);
                }
            }
            let sprite = Sprite {
                x: bound.left as i32,
                y: bound.top as i32,
                width: bound.width(),
                height: bound.height(),
                image: Image::from_rgba8_premultiplied(pixels),
            };
            match index {
                0 => layers.below_background.push(sprite),
                1 => layers.below_text.push(sprite),
                _ => layers.above_text.push(sprite),
            }
        }
        if let Some(images) = images {
            self.images = images;
        }
        self.viewport = Some(viewport);
        Ok(Some(layers))
    }
}

fn order(placement: &ImagePlacement) -> (i32, u32, u32) {
    (placement.z, placement.image.id, placement.placement_id)
}

fn layer(z: i32) -> usize {
    if z < -1_073_741_824 { 0 } else if z < 0 { 1 } else { 2 }
}

fn same_scene(left: &[ImagePlacement], right: &[ImagePlacement]) -> bool {
    left.len() == right.len() && left.iter().zip(right).all(|(a, b)| {
        a.image.id == b.image.id
            && a.image.generation == b.image.generation
            && a.image.width == b.image.width
            && a.image.height == b.image.height
            && a.image.rgba.len() == b.image.rgba.len()
            && a.placement_id == b.placement_id
            && a.x == b.x && a.y == b.y
            && a.width == b.width && a.height == b.height
            && a.source_x == b.source_x && a.source_y == b.source_y
            && a.source_width == b.source_width && a.source_height == b.source_height
            && a.z == b.z
    })
}

fn validate(placements: &[ImagePlacement]) -> Result<(), String> {
    if placements.len() > MAX_PLACEMENTS {
        return Err(format!("Kitty has {} placements; limit is {MAX_PLACEMENTS}", placements.len()));
    }
    for placement in placements {
        let image = &placement.image;
        let length = (image.width as usize).checked_mul(image.height as usize)
            .and_then(|pixels| pixels.checked_mul(4));
        if image.width == 0 || image.height == 0
            || length != Some(image.rgba.len()) || image.rgba.len() > MAX_IMAGE_BYTES
        {
            return Err(format!("Kitty image {} has invalid or oversized RGBA data ({}×{}, {} bytes)",
                image.id, image.width, image.height, image.rgba.len()));
        }
        if placement.width == 0 || placement.height == 0 {
            return Err(format!("Kitty placement {} has an empty destination", placement.placement_id));
        }
        if !valid_crop(placement.source_x, placement.source_width, image.width)
            || !valid_crop(placement.source_y, placement.source_height, image.height)
        {
            return Err(format!("Kitty placement {} has an invalid source crop in image {}",
                placement.placement_id, image.id));
        }
    }
    Ok(())
}

fn valid_crop(start: f64, extent: f64, maximum: u32) -> bool {
    let end = start + extent;
    let maximum = f64::from(maximum);
    let tolerance = maximum.max(1.0) * 1e-12;
    start.is_finite() && extent.is_finite() && end.is_finite() && extent > 0.0
        && start >= -tolerance && end <= maximum + tolerance
        && start.max(0.0) < end.min(maximum)
}

#[derive(Clone, Copy)]
struct Bounds {
    left: i64,
    top: i64,
    right: i64,
    bottom: i64,
}

impl Bounds {
    fn visible(placement: &ImagePlacement, viewport: (u32, u32)) -> Option<Self> {
        let bounds = Self {
            left: i64::from(placement.x).max(0),
            top: i64::from(placement.y).max(0),
            right: (i64::from(placement.x) + i64::from(placement.width)).min(i64::from(viewport.0)),
            bottom: (i64::from(placement.y) + i64::from(placement.height)).min(i64::from(viewport.1)),
        };
        (bounds.left < bounds.right && bounds.top < bounds.bottom).then_some(bounds)
    }

    fn width(self) -> u32 { (self.right - self.left) as u32 }
    fn height(self) -> u32 { (self.bottom - self.top) as u32 }
    fn area(self) -> u64 { u64::from(self.width()) * u64::from(self.height()) }

    fn union(self, other: Self) -> Self {
        Self {
            left: self.left.min(other.left),
            top: self.top.min(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Sample {
    first: usize,
    second: usize,
    weight: u32,
}

struct Axis {
    origin: f64,
    scale: f64,
    last: usize,
}

impl Axis {
    fn new(start: f64, extent: f64, source_size: u32, destination_size: u32) -> Self {
        Self {
            origin: start - 0.5,
            scale: extent / f64::from(destination_size),
            // Native Ghostty uses linear filtering with full-texture
            // clamp-to-edge. A source crop changes UVs, not the sampler bounds:
            // adjacent cropped placements must interpolate across their seam.
            last: source_size as usize - 1,
        }
    }

    fn sample(&self, position: u32) -> Sample {
        let coordinate = (self.origin + (f64::from(position) + 0.5) * self.scale)
            .clamp(0.0, self.last as f64);
        let first = coordinate.floor() as usize;
        Sample {
            first,
            second: (first + 1).min(self.last),
            weight: ((coordinate - first as f64) * 65536.0) as u32,
        }
    }
}

#[derive(Clone, Copy)]
struct GrayAlpha {
    gray: u32,
    alpha: u32,
}

fn texel(data: &[u8], offset: usize) -> GrayAlpha {
    let alpha = u32::from(data[offset + 3]);
    // Match the e-ink platform's luminance conversion, without the terminal
    // text contrast threshold. Premultiply BEFORE filtering transparent edges.
    let luminance = u32::from(data[offset]) * 77
        + u32::from(data[offset + 1]) * 150 + u32::from(data[offset + 2]) * 29;
    GrayAlpha { gray: (luminance * alpha + 32640) / 65280, alpha }
}

fn interpolate(a: GrayAlpha, b: GrayAlpha, weight: u32) -> GrayAlpha {
    let inverse = 65536 - weight;
    GrayAlpha {
        gray: (a.gray * inverse + b.gray * weight + 32768) >> 16,
        alpha: (a.alpha * inverse + b.alpha * weight + 32768) >> 16,
    }
}

fn sample_row(data: &[u8], row: usize, x: Sample) -> GrayAlpha {
    let first = texel(data, row + x.first * 4);
    if x.weight == 0 || x.first == x.second {
        first
    } else {
        interpolate(first, texel(data, row + x.second * 4), x.weight)
    }
}

fn composite(target: &mut [Rgba8Pixel], surface: Bounds, visible: Bounds, placement: &ImagePlacement) {
    let image = &placement.image;
    let x_axis = Axis::new(placement.source_x, placement.source_width, image.width, placement.width);
    let y_axis = Axis::new(placement.source_y, placement.source_height, image.height, placement.height);
    let source_stride = image.width as usize * 4;
    let target_stride = surface.width() as usize;
    let x_offset = (visible.left - i64::from(placement.x)) as u32;
    let y_offset = (visible.top - i64::from(placement.y)) as u32;
    let target_x = (visible.left - surface.left) as usize;
    let target_y = (visible.top - surface.top) as usize;
    // Fixed-size stack scratch avoids a scanline allocation proportional to an
    // untrusted viewport width. Each X mapping is reused for the entire height.
    let mut samples = [Sample::default(); SAMPLE_CHUNK];
    for chunk in (0..visible.width() as usize).step_by(SAMPLE_CHUNK) {
        let count = (visible.width() as usize - chunk).min(SAMPLE_CHUNK);
        for (index, sample) in samples[..count].iter_mut().enumerate() {
            *sample = x_axis.sample(x_offset + (chunk + index) as u32);
        }
        for row in 0..visible.height() as usize {
            let y = y_axis.sample(y_offset + row as u32);
            let first_row = y.first * source_stride;
            let second_row = y.second * source_stride;
            let start = (target_y + row) * target_stride + target_x + chunk;
            for (pixel, x) in target[start..start + count].iter_mut().zip(&samples[..count]) {
                let mut source = sample_row(&image.rgba, first_row, *x);
                if y.weight != 0 && y.first != y.second {
                    source = interpolate(source, sample_row(&image.rgba, second_row, *x), y.weight);
                }
                if source.alpha == 0 { continue }
                let inverse = 255 - source.alpha;
                let gray = (source.gray + (u32::from(pixel.r) * inverse + 127) / 255) as u8;
                pixel.r = gray;
                pixel.g = gray;
                pixel.b = gray;
                pixel.a = (source.alpha + (u32::from(pixel.a) * inverse + 127) / 255) as u8;
            }
        }
    }
}
