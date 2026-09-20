//! Resolve the actual application UI with Slint before exporting editable design geometry.
#![allow(dead_code)]
#[path = "../../../src/fonts.rs"]
mod fonts;
#[path = "../../../src/input.rs"]
mod input;
#[path = "../../../src/keyboard.rs"]
mod keyboard;
mod scene;
mod states;
#[path = "../../../src/ui.rs"]
mod ui;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, Rgb8Pixel};
use std::{
    path::Path,
    rc::Rc,
    time::{Duration, Instant},
};

struct Backend {
    window: Rc<MinimalSoftwareWindow>,
    start: Instant,
}
impl Platform for Backend {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
    fn duration_since_start(&self) -> Duration {
        self.start.elapsed()
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let output = args
        .next()
        .ok_or("Usage: kherdr-slint-paper OUTPUT [STATE]")?;
    if output == "--help" || output == "--list" {
        if output == "--help" {
            println!(
                "Usage: kherdr-slint-paper OUTPUT [STATE]\nRequires KHERDR_FONT_DIR with the original Kindle fonts.\nStates:"
            );
        }
        println!("{}", states::ALL.join("\n"));
        return Ok(());
    }
    let requested = args.next();
    if args.next().is_some() {
        return Err("Unexpected argument".into());
    }
    if let Some(name) = &requested {
        if !states::ALL.contains(&name.as_str()) {
            return Err(format!("Unknown state: {name}").into());
        }
    }
    let output = Path::new(&output);
    std::fs::create_dir_all(output)?;
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Backend {
        window: window.clone(),
        start: Instant::now(),
    }))?;
    fonts::register()?;
    let mut manifest = Vec::new();
    for state in states::ALL {
        if requested.as_ref().is_some_and(|name| name != state) {
            continue;
        }
        let app = ui::AppWindow::new()?;
        let height = if state.starts_with("readme-") { 1648 } else { 1547 };
        window.set_size(slint::PhysicalSize::new(1236, height));
        states::configure(&app, state)?;
        app.show()?;
        let mut pixels = vec![
            Rgb8Pixel {
                r: 255,
                g: 255,
                b: 255
            };
            1236 * height as usize
        ];
        for action in std::iter::once(None).chain(states::actions(state).iter().map(Some)) {
            if let Some(label) = action {
                scene::activate(app.window(), label)?;
            }
            for _ in 0..4 {
                slint::platform::update_timers_and_animations();
                window.draw_if_needed(|renderer| {
                    renderer.render(&mut pixels, 1236);
                });
            }
        }
        let scene = scene::capture(app.window(), state)?;
        std::fs::write(
            output.join(format!("{state}.json")),
            serde_json::to_vec_pretty(&scene)?,
        )?;
        let file = std::fs::File::create(output.join(format!("{state}.png")))?;
        let mut encoder = png::Encoder::new(file, 1236, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        encoder.write_header()?.write_image_data(&bytes)?;
        manifest.push(serde_json::json!({"state": state, "route":format!("{:?}",app.get_route()), "width":1236, "height":height, "nodes":scene.nodes.len(), "unsupported":scene.unsupported}));
        app.hide()?;
    }
    if manifest.is_empty() {
        return Err(format!("Unknown state: {}", requested.unwrap_or_default()).into());
    }
    std::fs::write(
        output.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&manifest)?);
    Ok(())
}
