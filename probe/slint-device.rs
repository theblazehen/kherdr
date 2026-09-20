use slint::ComponentHandle;
#[path = "../src/platform.rs"]
mod platform;
#[path = "../src/fonts.rs"]
mod fonts;

slint::slint! {
    export component DeviceProbe inherits Window {
        title: "L:A_N:application_ID:net.fabiszewski.kherdr_PC:N_O:URL";
        width: 1236px;
        height: 1648px;
        no-frame: true;
        background: white;
        default-font-size: 32px;
        in-out property <int> taps: 0;
        in-out property <bool> keyboard: false;
        callback touched();
        callback close-requested();
        VerticalLayout {
            padding: 20px;
            spacing: 16px;
            Text { text: "kherdr — Rust + Slint"; color: black; font-size: 42px; }
            Text { text: "CPU software renderer / X11"; color: black; }
            Rectangle {
                border-width: 2px;
                border-color: black;
                min-height: 112px;
                Text {
                    text: "Touch count: " + root.taps + " — toggle keyboard";
                    color: black;
                    horizontal-alignment: center;
                    vertical-alignment: center;
                }
                TouchArea { clicked => { root.touched(); } }
            }
            Rectangle {
                border-width: 2px;
                border-color: black;
                vertical-stretch: 1;
                Text {
                    x: 16px; y: 16px;
                    width: parent.width - 32px;
                    text: "Native text rendering\n\nASCII: [] {} () <> / \\ | ~ `\nUnicode: café é Ελληνικά 日本語\nBox drawing: ┌────┐ │ └────┘\n\nThis is a toolkit probe, not a remote session.\nNothing animates or polls while idle.";
                    color: black;
                    font-family: "monospace";
                    wrap: word-wrap;
                }
            }
            if root.keyboard: Rectangle {
                height: 460px;
                border-width: 2px;
                border-color: black;
                Text {
                    text: "Keyboard-sized touch panel\nViewport resizes above";
                    color: black;
                    horizontal-alignment: center;
                    vertical-alignment: center;
                }
            }
            Rectangle {
                height: 96px;
                border-width: 2px;
                border-color: black;
                Text {
                    text: "Exit probe"; color: black;
                    horizontal-alignment: center;
                    vertical-alignment: center;
                }
                TouchArea { clicked => { root.close-requested(); } }
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    platform::install()?;
    fonts::register()?;
    let app = DeviceProbe::new()?;
    let weak = app.as_weak();
    app.on_touched(move || {
        if let Some(app) = weak.upgrade() {
            app.set_taps(app.get_taps() + 1);
            app.set_keyboard(!app.get_keyboard());
            eprintln!("touch={} panel={}", app.get_taps(), app.get_keyboard());
        }
    });
    app.on_close_requested(|| {
        if let Err(error) = slint::quit_event_loop() {
            eprintln!("cannot exit event loop: {error}");
        }
    });
    eprintln!("kherdr Slint device probe: software renderer selected");
    app.run()?;
    eprintln!("kherdr Slint device probe: clean exit");
    Ok(())
}
