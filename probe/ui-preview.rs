//! Staged UI on the real Kindle backend; never loads configuration or connects to hosts.
#![allow(dead_code)]
#[path = "../src/fonts.rs"]
mod fonts;
#[path = "../src/input.rs"]
mod input;
#[path = "../src/keyboard.rs"]
mod keyboard;
#[path = "../src/platform.rs"]
mod platform;
#[path = "../tools/slint-paper/src/states.rs"]
mod states;
#[path = "../src/ui.rs"]
mod ui;

use slint::ComponentHandle;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let state = args.next().unwrap_or_else(|| "readme-hero".into());
    if !matches!(state.as_str(), "readme-hero" | "readme-machines") || args.next().is_some() {
        return Err("Usage: kherdr-ui-preview [readme-hero|readme-machines]".into());
    }
    platform::install()?;
    fonts::register()?;
    let app = ui::AppWindow::new()?;
    states::configure(&app, &state)?;
    let weak = app.as_weak();
    platform::on_focus_changed(move |active| {
        if let Some(app) = weak.upgrade() {
            app.invoke_application_activity(active);
        }
    });
    app.on_quit(|| { let _ = slint::quit_event_loop(); });
    eprintln!("Staged UI preview: {state}; no configuration or network sessions loaded");
    app.run()?;
    Ok(())
}
