#[path = "../src/graphics.rs"]
mod graphics;

use crate::terminal::{ImageData, ImagePlacement};
use graphics::{GraphicsRenderer, Layers, Sprite};
use std::rc::Rc;

fn image(id: u32, generation: u64, width: u32, height: u32, pixels: &[[u8; 4]]) -> Rc<ImageData> {
    Rc::new(ImageData {
        id, generation, width, height,
        rgba: pixels.iter().flatten().copied().collect(),
    })
}

fn placement(image: Rc<ImageData>) -> ImagePlacement {
    ImagePlacement {
        placement_id: 1,
        x: 0, y: 0,
        width: image.width, height: image.height,
        source_x: 0.0, source_y: 0.0,
        source_width: f64::from(image.width), source_height: f64::from(image.height),
        z: 0,
        image,
    }
}

fn render(placements: Vec<ImagePlacement>, width: u32, height: u32) -> Result<Layers, String> {
    GraphicsRenderer::default().update(Some(placements), width, height)?
        .ok_or_else(|| "New graphics scene did not produce layers".into())
}

fn pixels(sprite: &Sprite) -> Result<Vec<[u8; 4]>, String> {
    let buffer = sprite.image.to_rgba8_premultiplied()
        .ok_or_else(|| "Composited Slint image did not expose its actual premultiplied pixels".to_string())?;
    assert_eq!((buffer.width(), buffer.height()), (sprite.width, sprite.height));
    Ok(buffer.as_slice().iter().map(|pixel| [pixel.r, pixel.g, pixel.b, pixel.a]).collect())
}

fn single(sprites: &[Sprite]) -> &Sprite {
    assert_eq!(sprites.len(), 1, "A nonempty z layer must have exactly one surface");
    &sprites[0]
}

fn empty(layers: &Layers) -> bool {
    layers.below_background.is_empty() && layers.below_text.is_empty() && layers.above_text.is_empty()
}

fn expect_rejected(placements: Vec<ImagePlacement>, width: u32, height: u32, label: &str) {
    match GraphicsRenderer::default().update(Some(placements), width, height) {
        Err(message) => assert!(!message.trim().is_empty(), "{label}: rejection must explain the failure"),
        Ok(_) => panic!("{label}: invalid or excessive scene was accepted"),
    }
}

fn alpha_and_grayscale() -> Result<(), String> {
    let colors = image(1, 1, 6, 1, &[
        [255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255],
        [128, 128, 128, 255], [255, 255, 255, 128], [255, 0, 255, 0],
    ]);
    let layers = render(vec![placement(colors)], 6, 1)?;
    assert_eq!(pixels(single(&layers.above_text))?, vec![
        [77, 77, 77, 255], [149, 149, 149, 255], [29, 29, 29, 255],
        [128, 128, 128, 255], [128, 128, 128, 128], [0, 0, 0, 0],
    ], "Image luminance must preserve gray levels and premultiplied alpha");

    // The hidden red texel must not darken the translucent white edge.
    let mut edge = placement(image(2, 2, 2, 1, &[[255, 255, 255, 255], [255, 0, 0, 0]]));
    edge.width = 4;
    let layers = render(vec![edge], 4, 1)?;
    assert_eq!(pixels(single(&layers.above_text))?, vec![
        [255, 255, 255, 255], [191, 191, 191, 191], [64, 64, 64, 64], [0, 0, 0, 0],
    ], "Bilinear interpolation must filter premultiplied rather than straight colors");

    let mut black = placement(image(99, 3, 1, 1, &[[0, 0, 0, 128]]));
    black.z = 3;
    let mut white = placement(image(4, 4, 1, 1, &[[255, 255, 255, 128]]));
    white.z = 4;
    let layers = render(vec![white, black], 1, 1)?;
    assert_eq!(pixels(single(&layers.above_text))?, vec![[128, 128, 128, 192]],
        "Ordered source-over must preserve partial destination alpha");
    Ok(())
}

fn layers_and_order() -> Result<(), String> {
    let mut placements = Vec::new();
    for (index, (z, gray)) in [(i32::MIN, 10), (-1_073_741_824, 20), (-1, 30), (0, 40)].into_iter().enumerate() {
        let mut item = placement(image(index as u32 + 1, index as u64 + 10, 1, 1, &[[gray, gray, gray, 255]]));
        item.z = z;
        placements.push(item);
    }
    placements.reverse();
    let layers = render(placements, 1, 1)?;
    assert_eq!(pixels(single(&layers.below_background))?, vec![[10, 10, 10, 255]]);
    assert_eq!(pixels(single(&layers.below_text))?, vec![[30, 30, 30, 255]]);
    assert_eq!(pixels(single(&layers.above_text))?, vec![[40, 40, 40, 255]]);

    let black = placement(image(1, 20, 1, 1, &[[0, 0, 0, 255]]));
    let white = placement(image(2, 21, 1, 1, &[[255, 255, 255, 128]]));
    let layers = render(vec![white, black], 1, 1)?;
    assert_eq!(pixels(single(&layers.above_text))?, vec![[128, 128, 128, 255]],
        "Equal z uses ascending image ID, independent of input order");

    let pair = image(3, 22, 2, 1, &[[0, 0, 0, 255], [255, 255, 255, 255]]);
    let mut first = placement(pair);
    first.width = 1;
    first.source_width = 1.0;
    let mut second = first.clone();
    second.placement_id = 2;
    second.source_x = 1.0;
    let layers = render(vec![second, first], 1, 1)?;
    assert_eq!(pixels(single(&layers.above_text))?, vec![[255, 255, 255, 255]],
        "Equal image ID and z use ascending placement ID");
    Ok(())
}

fn crops_and_clipping() -> Result<(), String> {
    let mut cropped = placement(image(1, 30, 4, 1, &[
        [0, 0, 0, 255], [64, 64, 64, 255], [128, 128, 128, 255], [255, 255, 255, 255],
    ]));
    cropped.source_x = 1.0;
    cropped.source_width = 2.0;
    let layers = render(vec![cropped.clone()], 4, 1)?;
    assert_eq!(pixels(single(&layers.above_text))?, vec![
        [48, 48, 48, 255], [80, 80, 80, 255], [112, 112, 112, 255], [160, 160, 160, 255],
    ], "Native crop UVs must filter neighboring texels, clamping only at the full image edge");
    cropped.x = -1;
    let layers = render(vec![cropped], 2, 1)?;
    assert_eq!(pixels(single(&layers.above_text))?, vec![[80, 80, 80, 255], [112, 112, 112, 255]],
        "Negative clipping must not rescale or restart the source mapping");

    let grid: Vec<_> = (0_u8..12).map(|index| [index * 10, index * 10, index * 10, 255]).collect();
    let mut square = placement(image(2, 31, 4, 3, &grid));
    square.x = -1; square.y = -1;
    square.width = 4; square.height = 4;
    square.source_x = 1.0; square.source_y = 1.0;
    square.source_width = 2.0; square.source_height = 2.0;
    let layers = render(vec![square], 2, 2)?;
    assert_eq!(pixels(single(&layers.above_text))?, vec![
        [63, 63, 63, 255], [68, 68, 68, 255], [83, 83, 83, 255], [88, 88, 88, 255],
    ], "Two-dimensional crop interpolation must survive top and left clipping");

    let mut first = placement(image(3, 32, 1, 1, &[[255, 255, 255, 255]]));
    first.x = 2; first.y = 1;
    let mut last = first.clone();
    last.x = 4; last.y = 3;
    let layers = render(vec![first.clone(), last], 10, 10)?;
    let surface = single(&layers.above_text);
    assert_eq!((surface.x, surface.y, surface.width, surface.height), (2, 1, 3, 3));
    let actual = pixels(surface)?;
    assert_eq!(actual[0], [255, 255, 255, 255]);
    assert_eq!(actual[8], [255, 255, 255, 255]);
    assert!(actual[1..8].iter().all(|pixel| *pixel == [0, 0, 0, 0]), "Gaps in tight surfaces must remain transparent");
    assert!(empty(&render(vec![first.clone()], 2, 1)?), "Fully offscreen placements must not allocate sprites");
    assert!(empty(&render(vec![first], 0, 0)?), "A zero-size viewport has no visible image surfaces");
    Ok(())
}

fn fractional_fragments() -> Result<(), String> {
    let mut whole = placement(image(1, 40, 2, 2, &[
        [255, 255, 255, 255], [255, 0, 0, 0], [0, 0, 0, 64], [128, 128, 128, 255],
    ]));
    whole.x = -1; whole.y = -1;
    whole.width = 8; whole.height = 4;
    whole.z = -1;
    let expected = render(vec![whole.clone()], 6, 2)?;
    let mut fragments = Vec::new();
    for row in 0..4 {
        for col in 0..8 {
            let mut fragment = whole.clone();
            fragment.x += col;
            fragment.y += row;
            fragment.width = 1; fragment.height = 1;
            fragment.source_x = f64::from(col) / 4.0;
            fragment.source_y = f64::from(row) / 2.0;
            fragment.source_width = 0.25; fragment.source_height = 0.5;
            fragments.push(fragment);
        }
    }
    fragments.reverse();
    let actual = render(fragments, 6, 2)?;
    let expected = single(&expected.below_text);
    let actual = single(&actual.below_text);
    assert_eq!((actual.x, actual.y, actual.width, actual.height),
        (expected.x, expected.y, expected.width, expected.height));
    assert_eq!(pixels(actual)?, pixels(expected)?,
        "Fractional virtual-cell fragments must exactly match one continuous image, without seams or disappearing subpixel crops");
    Ok(())
}

fn integer_source_fragments() -> Result<(), String> {
    // Stock Herdr can turn a tiny virtual image into adjacent regular Kitty
    // placements whose source crops meet on integer texel boundaries. These
    // crops still sample one texture, not independently edge-clamped tiles.
    let mut whole = placement(image(2, 41, 2, 2, &[
        [0, 0, 0, 255], [85, 85, 85, 255],
        [170, 170, 170, 255], [255, 255, 255, 255],
    ]));
    whole.width = 8; whole.height = 8;
    let expected = render(vec![whole.clone()], 8, 8)?;
    let mut fragments = Vec::new();
    for row in 0..2 {
        for col in 0..2 {
            let mut fragment = whole.clone();
            fragment.placement_id = (row * 2 + col + 1) as u32;
            fragment.x = col * 4;
            fragment.y = row * 4;
            fragment.width = 4; fragment.height = 4;
            fragment.source_x = f64::from(col);
            fragment.source_y = f64::from(row);
            fragment.source_width = 1.0; fragment.source_height = 1.0;
            fragments.push(fragment);
        }
    }
    fragments.reverse();
    let actual = render(fragments, 8, 8)?;
    let actual = pixels(single(&actual.above_text))?;
    assert_eq!(actual, pixels(single(&expected.above_text))?,
        "Adjacent integer-source regular placements must reconstruct the continuous tiny image across both axes");
    assert_eq!((actual[3], actual[4]), ([32, 32, 32, 255], [53, 53, 53, 255]),
        "The midpoint must interpolate across the placement seam instead of jumping from 0 to 85");
    Ok(())
}

fn cache_and_clear() -> Result<(), String> {
    let mut renderer = GraphicsRenderer::default();
    let original = placement(image(1, 50, 1, 1, &[[64, 64, 64, 255]]));
    assert!(renderer.update(Some(vec![original.clone()]), 2, 1)?.is_some());
    assert!(renderer.update(None, 2, 1)?.is_none(), "Text-only frames must retain graphics without rerendering");
    assert!(renderer.update(Some(vec![original.clone()]), 2, 1)?.is_none(), "Identical placement snapshots must reuse graphics");
    let replacement = placement(image(1, 51, 1, 1, &[[128, 128, 128, 255]]));
    let layers = renderer.update(Some(vec![replacement.clone()]), 2, 1)?.expect("Retransmitted generation must repaint");
    assert_eq!(pixels(single(&layers.above_text))?, vec![[128, 128, 128, 255]]);
    let mut moved = replacement.clone();
    moved.x = 1;
    let layers = renderer.update(Some(vec![moved]), 2, 1)?.expect("Placement movement must repaint");
    assert_eq!(single(&layers.above_text).x, 1);
    let layers = renderer.update(None, 1, 1)?.expect("Viewport clipping changes must repaint retained placements");
    assert!(empty(&layers));
    let layers = renderer.update(None, 2, 1)?.expect("Viewport expansion must restore retained offscreen placements");
    assert_eq!(single(&layers.above_text).x, 1);
    let layers = renderer.update(Some(Vec::new()), 2, 1)?.expect("Explicit empty scene must clear graphics");
    assert!(empty(&layers));
    assert!(renderer.update(None, 2, 1)?.is_none());
    let layers = renderer.update(None, 3, 1)?.expect("Empty viewport resize must remain empty");
    assert!(empty(&layers), "Deleted placements must not reappear on resize");

    let mut invalid = replacement.clone();
    invalid.source_width = f64::NAN;
    assert!(renderer.update(Some(vec![replacement]), 2, 1)?.is_some());
    assert!(renderer.update(Some(vec![invalid]), 2, 1).is_err());
    assert!(renderer.update(None, 2, 1)?.is_none(), "Rejected scene must not poison the successful cache");
    let layers = renderer.update(None, 3, 1)?.expect("Valid retained scene survives rejection");
    assert_eq!(pixels(single(&layers.above_text))?, vec![[128, 128, 128, 255]]);

    let mut crop = placement(image(2, 52, 4, 1, &[
        [0, 0, 0, 255], [64, 64, 64, 255], [128, 128, 128, 255], [255, 255, 255, 255],
    ]));
    crop.source_x = 1.0; crop.source_width = 2.0;
    let layers = renderer.update(Some(vec![crop.clone()]), 4, 1)?.expect("New crop must repaint");
    assert_eq!(pixels(single(&layers.above_text))?[0], [48, 48, 48, 255]);
    crop.width = 2;
    let layers = renderer.update(Some(vec![crop.clone()]), 4, 1)?.expect("Destination size must repaint");
    assert_eq!(pixels(single(&layers.above_text))?, vec![[64, 64, 64, 255], [128, 128, 128, 255]]);
    crop.source_x = 2.0; crop.source_width = 1.0;
    let layers = renderer.update(Some(vec![crop.clone()]), 4, 1)?.expect("Source rectangle must repaint");
    assert_eq!(pixels(single(&layers.above_text))?, vec![[112, 112, 112, 255], [160, 160, 160, 255]]);
    crop.z = -1;
    let layers = renderer.update(Some(vec![crop]), 4, 1)?.expect("Changed z must move the image between layers");
    assert!(layers.above_text.is_empty());
    assert_eq!(single(&layers.below_text).width, 2);
    Ok(())
}

fn rejection_bounds() {
    let base = placement(image(1, 60, 1, 1, &[[255, 255, 255, 255]]));
    expect_rejected(vec![base.clone(); 4097], 1, 1, "placement count");
    let mut huge = base.clone();
    huge.width = 2048; huge.height = 2048;
    expect_rejected(vec![huge.clone(); 17], 2048, 2048, "overlap pixel work");
    let mut below = huge.clone(); below.z = i32::MIN;
    let mut middle = huge; middle.z = -1;
    expect_rejected(vec![below, middle, {
        let mut above = base.clone(); above.width = 2048; above.height = 2048; above
    }], 2048, 2048, "combined surface memory across z layers");
    let mut far = base.clone(); far.x = 4095; far.y = 4095;
    expect_rejected(vec![base.clone(), far], 4096, 4096, "sparse bounding-box surface memory");

    for (start, extent, label) in [
        (f64::NAN, 1.0, "NaN crop origin"), (0.0, f64::INFINITY, "infinite crop size"),
        (0.0, 0.0, "empty source crop"), (0.0, -1.0, "negative source crop"),
        (-1.0, 1.0, "negative crop origin"), (0.0, 2.0, "crop outside image"),
    ] {
        let mut bad = base.clone(); bad.source_x = start; bad.source_width = extent;
        expect_rejected(vec![bad], 1, 1, label);
    }
    let mut bad = base.clone(); bad.width = 0;
    expect_rejected(vec![bad], 1, 1, "empty destination");
    expect_rejected(vec![placement(image(2, 61, 1, 1, &[]))], 1, 1, "short RGBA data");
    expect_rejected(vec![placement(image(3, 62, 1, 1, &[[0, 0, 0, 0]; 2]))], 1, 1, "excess RGBA data");
    expect_rejected(vec![placement(image(4, 63, u32::MAX, u32::MAX, &[]))], 1, 1, "overflowing image dimensions");
}

pub fn run() -> Result<(), String> {
    alpha_and_grayscale()?;
    layers_and_order()?;
    crops_and_clipping()?;
    fractional_fragments()?;
    integer_source_fragments()?;
    cache_and_clear()?;
    rejection_bounds();
    println!("Graphics compositor corpus passed: actual Slint pixels, grayscale, premultiplied alpha, z boundaries/order, full-texture crop filtering/clipping, fractional virtual and integer regular fragment continuity, cache/retransmission/resize/deletion, and rejection bounds");
    Ok(())
}
