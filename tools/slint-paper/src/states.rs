//! Public UI properties and callbacks only; no servers, credentials or live terminal work.
use crate::{keyboard, ui};
use slint::{Color, ModelRc, VecModel};

pub const ALL: &[&str] = &[
    "readme-hero",
    "readme-machines",
    "terminal",
    "terminal-keyboard",
    "terminal-symbols",
    "terminal-functions",
    "terminal-modifiers",
    "terminal-tabs",
    "terminal-disconnected",
    "terminal-connecting",
    "machines-single",
    "machines-multiple",
    "agents-priority",
    "pane-actions",
    "workspace-actions",
    "rename",
    "entry",
    "host-verification",
    "host-key-changed",
    "sign-in",
    "missing-herdr",
    "terminal-attention-zero",
    "terminal-attention-three",
    "terminal-attention-unknown",
    "terminal-attention-9",
    "hosts",
    "host-sessions",
    "host-empty",
    "host-error",
    "host-actions",
    "shown-sessions",
    "connection",
    "setup",
    "session-name",
    "credentials",
    "keys",
    "key-editor",
    "trusted-hosts",
    "key-files",
    "key-details",
    "key-picker",
    "menu",
    "settings",
    "help",
];
fn model<T: Clone + 'static>(rows: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(rows))
}

pub fn configure(app: &ui::AppWindow, state: &str) -> Result<(), Box<dyn std::error::Error>> {
    app.invoke_application_activity(true);
    app.set_connected(true);
    app.set_input_ready(true);
    app.set_cell_width(15.);
    app.set_cell_height(31.);
    app.set_columns(82);
    app.set_terminal_font_family("KindleBlackboxC".into());
    app.set_terminal_font_size(25.);
    app.set_current_session_label("Build host · desktop".into());
    app.set_current_session_name("desktop".into());
    app.set_attention_count(1);
    app.set_attention_unknown(false);
    app.set_selected_name("Review terminal navigation".into());
    app.set_selected_workspace_label("kherdr".into());
    app.set_current_connection_id("host-demo".into());
    app.set_current_view_id("view-desktop".into());
    app.set_current_connection_name("Build host".into());
    app.set_current_connection_detail("developer@example.invalid:22".into());
    app.set_connection_status("Connected".into());
    app.set_focused_pane_id("p1".into());
    app.set_focused_workspace_id("w1".into());
    let pane = ui::SidebarRow {
        endpoint_id: "1".into(), available: true,
        pane_id: "p1".into(),
        workspace_id: "w1".into(),
        label: "Review terminal navigation".into(),
        resource_name: "Review terminal navigation".into(),
        status: "working".into(),
        selected: true,
        can_zoom: true,
        can_swap: true,
        ..Default::default()
    };
    app.set_focused_pane(pane.clone());
    app.set_pane_rows(model(vec![
        pane.clone(),
        ui::SidebarRow {
            pane_id: "p2".into(),
            workspace_id: "w1".into(),
            label: "Local shell".into(),
            resource_name: "Shell".into(),
            status: "idle".into(),
            ..Default::default()
        },
    ]));
    let mut machines = vec![ui::MachineRow {
        endpoint_id: "1".into(), machine: true, expanded: true, enabled: true,
        entry: ui::SidebarRow { label: "Build host · desktop".into(), ..Default::default() },
        ..Default::default()
    }];
    for (workspace, label, status) in [("w1", "kherdr", "working"), ("w2", "docs", "blocked"), ("w3", "network-tools", "done")] {
        machines.push(ui::MachineRow {
            endpoint_id: "1".into(), enabled: true,
            entry: ui::SidebarRow {
                endpoint_id: "1".into(), available: true, workspace_id: workspace.into(),
                label: label.into(), resource_name: label.into(), status: status.into(), selected: workspace == "w1",
                ..Default::default()
            }, ..Default::default()
        });
    }
    if state == "machines-multiple" || state == "agents-priority" {
        machines.push(ui::MachineRow {
            endpoint_id: "2".into(), machine: true, expanded: true, enabled: true,
            entry: ui::SidebarRow { label: "Spare host · default".into(), ..Default::default() }, ..Default::default()
        });
        machines.push(ui::MachineRow {
            endpoint_id: "2".into(), enabled: true,
            entry: ui::SidebarRow { endpoint_id: "2".into(), available: true, workspace_id: "w1".into(), label: "research".into(), status: "idle".into(), ..Default::default() },
            ..Default::default()
        });
    }
    app.set_machine_rows(model(machines));
    let mut agents = vec![
        ui::SidebarRow { label: "OMP".into(), detail: "Build host / kherdr / Review terminal navigation".into(), ..pane.clone() },
        ui::SidebarRow { endpoint_id: "1".into(), available: true, pane_id: "p2".into(), workspace_id: "w2".into(),
            label: "Codex".into(), detail: "Build host / docs / Check documentation".into(), status: "blocked".into(), ..Default::default() },
        ui::SidebarRow { endpoint_id: "1".into(), available: true, pane_id: "p3".into(), workspace_id: "w3".into(),
            label: "OMP".into(), detail: "Build host / network-tools / Reconnect behavior".into(), status: "done".into(), ..Default::default() },
    ];
    if state == "agents-priority" { agents.swap(0, 1); app.set_agents_priority(true); }
    app.set_agent_rows(model(agents));
    app.set_tabs(model(vec![ui::TabView {
        tab_id: "t1".into(),
        name: "Review terminal navigation".into(),
        status: "working".into(),
        selected: true,
    }]));
    let local = ui::HostView {
        id: "local".into(),
        name: "This Kindle".into(),
        endpoint: "Local Herdr".into(),
        state: "Connected".into(),
        local: true,
        open: true,
        ..Default::default()
    };
    let host = ui::HostView {
        id: "host-demo".into(),
        name: "Build host".into(),
        endpoint: "developer@example.invalid:22".into(),
        state: "Connected".into(),
        selected: true,
        open: true,
        ..Default::default()
    };
    app.set_host_entries(model(vec![local, host.clone()]));
    app.set_host_detail(host);
    app.set_host_sessions(model(vec![
        ui::HostSessionView {
            name: "desktop".into(),
            id: "view-desktop".into(),
            state: "Connected".into(),
            checked: true,
            available: true,
            ..Default::default()
        },
        ui::HostSessionView {
            name: "default".into(),
            state: "Running".into(),
            available: true,
            ..Default::default()
        },
    ]));
    app.set_open_sessions(model(vec![
        ui::OpenSessionView {
            id: "view-local".into(),
            host_id: "local".into(),
            host_name: "This Kindle".into(),
            name: "Local".into(),
            kind: "Local Herdr".into(),
            state: "Connected".into(),
            ..Default::default()
        },
        ui::OpenSessionView {
            id: "view-desktop".into(),
            host_id: "host-demo".into(),
            host_name: "Build host".into(),
            name: "desktop".into(),
            kind: "Remote Herdr".into(),
            state: "Connected".into(),
            selected: true,
        },
    ]));
    app.set_connection_draft(ui::ConnectionDraft {
        name: "Build host".into(),
        host: "example.invalid".into(),
        user: "developer".into(),
        port: "22".into(),
        auth_method: "key".into(),
        keepalive: "30".into(),
        herdr_session: "desktop".into(),
        herdr_binary: "herdr".into(),
        session_kind: "herdr".into(),
        ..Default::default()
    });
    app.set_key_entries(model(vec![ui::KeyView {
        id: "demo-key".into(),
        name: "Development key".into(),
        detail: "Ed25519 · public-key authentication".into(),
        path: "/example/id_ed25519".into(),
    }]));
    app.set_selected_key_name("Development key".into());
    app.set_key_name("Development key".into());
    app.set_key_path("/example/id_ed25519".into());
    app.set_key_directory("/example".into());
    app.set_key_fingerprint("SHA256:EXAMPLE-NOT-A-REAL-FINGERPRINT".into());
    app.set_key_public_key("Public key preview — synthetic design data".into());
    app.set_key_files(model(vec![
        ui::FileView {
            path: "/example/keys".into(),
            name: "keys".into(),
            directory: true,
        },
        ui::FileView {
            path: "/example/id_ed25519".into(),
            name: "id_ed25519".into(),
            directory: false,
        },
    ]));
    app.set_trusted_hosts(model(vec![ui::TrustedHostView {
        id: "demo-trust".into(),
        host: "example.invalid".into(),
        algorithm: "ssh-ed25519".into(),
        fingerprint: "SHA256:EXAMPLE-NOT-A-REAL-FINGERPRINT".into(),
    }]));
    app.set_setup_host_id("host-demo".into());
    app.set_setup_host_name("Build host".into());
    app.set_setup_session_name("desktop".into());
    app.set_setup_binary("herdr".into());
    app.set_setup_detail("Herdr 0.9 is available. Existing sessions remain untouched.".into());
    app.set_setup_inspected(true);
    app.set_setup_available(true);
    app.set_setup_sessions(model(vec!["desktop".into(), "default".into()]));
    // The README scene is entirely synthetic; never load live session content.
    let lines: &[&str] = if state.starts_with("readme-") { &[
        "",
        "  dev@workbench  ~/hello",
        "  $ tree",
        "  .",
        "  ├── Cargo.toml",
        "  └── src",
        "      └── main.rs",
        "",
        "  $ cat src/main.rs",
        "  fn main() {",
        "      let places = [\"the desk\", \"the sofa\", \"anywhere\"];",
        "",
        "      println!(\"hello, e-ink.\\n\");",
        "      for place in places {",
        "          println!(\"A real terminal. From {place}.\");",
        "      }",
        "  }",
        "",
        "  $ cargo run --quiet",
        "  hello, e-ink.",
        "",
        "  A real terminal. From the desk.",
        "  A real terminal. From the sofa.",
        "  A real terminal. From anywhere.",
        "",
        "  $ git status --short --branch",
        "  ## main",
        "",
        "  dev@workbench  ~/hello",
        "  $ ",
    ] } else { &["$ pwd", "/workspace/project", "", "$ "] };
    app.set_rows(model(
        lines
            .iter()
            .enumerate()
            .map(|(index, text)| {
                let cells: Vec<_> = text
                    .chars()
                    .enumerate()
                    .map(|(column, value)| ui::CellView {
                        column: column as i32,
                        span: 1,
                        text: value.to_string().into(),
                        foreground: Color::from_rgb_u8(0, 0, 0),
                        background: Color::from_rgb_u8(255, 255, 255),
                        background_is_default: true,
                        ..Default::default()
                    })
                    .collect();
                ui::RowView {
                    index: index as i32,
                    cells: model(cells.clone()),
                    glyphs: model(cells),
                    ..Default::default()
                }
            })
            .collect(),
    ));
    let mut keyboard = keyboard::Keyboard::default();
    if state == "terminal-symbols" || state == "terminal-functions" {
        keyboard.toggle("symbols");
    }
    if state == "terminal-functions" {
        keyboard.toggle("functions");
    }
    if state == "terminal-modifiers" {
        keyboard.toggle("ctrl");
        keyboard.toggle("shift");
    }
    keyboard.render(app);
    app.invoke_navigate(ui::Route::Terminal);
    app.invoke_set_keyboard(matches!(
        state,
        "terminal-keyboard"
            | "readme-hero"
            | "readme-machines"
            | "terminal-symbols"
            | "terminal-functions"
            | "terminal-modifiers"
            | "rename"
            | "connection"
            | "key-editor"
            | "session-name"
    ));
    match state {
        "readme-hero" | "readme-machines" => {
            app.set_attention_count(0);
            app.set_system_time("10:24".into());
            app.set_system_battery("96%".into());
            app.set_current_connection_name("Workbench".into());
            app.set_current_session_name("dev".into());
            app.set_current_session_label("Workbench · dev".into());
            app.set_selected_workspace_label("hello".into());
            app.set_machine_rows(model(vec![
                ui::MachineRow { endpoint_id: "local".into(), machine: true, local: true, expanded: true, enabled: true,
                    entry: ui::SidebarRow { label: "This Kindle · Local".into(), ..Default::default() }, ..Default::default() },
                ui::MachineRow { endpoint_id: "local".into(), enabled: true,
                    entry: ui::SidebarRow { endpoint_id: "local".into(), available: true, workspace_id: "local-notes".into(), label: "notes".into(), status: "idle".into(), ..Default::default() }, ..Default::default() },
                ui::MachineRow { endpoint_id: "1".into(), machine: true, expanded: true, enabled: true,
                    entry: ui::SidebarRow { label: "Workbench · dev".into(), ..Default::default() }, ..Default::default() },
                ui::MachineRow { endpoint_id: "1".into(), enabled: true,
                    entry: ui::SidebarRow { endpoint_id: "1".into(), available: true, workspace_id: "w1".into(), label: "hello".into(), status: "working".into(), selected: true, ..Default::default() }, ..Default::default() },
                ui::MachineRow { endpoint_id: "1".into(), enabled: true,
                    entry: ui::SidebarRow { endpoint_id: "1".into(), available: true, workspace_id: "w2".into(), label: "garden".into(), status: "done".into(), ..Default::default() }, ..Default::default() },
            ]));
            app.set_agent_rows(model(vec![
                ui::SidebarRow { endpoint_id: "1".into(), available: true, pane_id: "p1".into(), workspace_id: "w1".into(),
                    label: "OMP".into(), detail: "Workbench / hello / Explore the code".into(), status: "working".into(), ..Default::default() },
                ui::SidebarRow { endpoint_id: "1".into(), available: true, pane_id: "p2".into(), workspace_id: "w2".into(),
                    label: "Codex".into(), detail: "Workbench / garden / Review a small change".into(), status: "done".into(), ..Default::default() },
            ]));
            app.set_tabs(model(vec![
                ui::TabView { tab_id: "t1".into(), name: "Code".into(), status: "idle".into(), selected: false },
                ui::TabView { tab_id: "t2".into(), name: "Shell".into(), status: "idle".into(), selected: true },
                ui::TabView { tab_id: "t3".into(), name: "Notes".into(), status: "idle".into(), selected: false },
            ]));
            if state == "readme-machines" { app.invoke_navigate(ui::Route::Machines); }
        }
        "terminal" | "terminal-keyboard" | "terminal-symbols" | "terminal-functions"
        | "terminal-modifiers" => {}
        "terminal-tabs" => app.set_tabs(model(vec![
            ui::TabView {
                tab_id: "t1".into(),
                name: "Navigation".into(),
                status: "working".into(),
                selected: true,
            },
            ui::TabView {
                tab_id: "t2".into(),
                name: "Shell".into(),
                status: "idle".into(),
                selected: false,
            },
        ])),
        "terminal-disconnected" => {
            app.set_connected(false);
            app.set_input_ready(false);
            app.set_connection_status("Connection lost. Remote work may still be running.".into());
        }
        "terminal-connecting" => {
            app.set_connected(false);
            app.set_input_ready(false);
            app.set_connecting_or_retrying(true);
            app.set_connection_status("Connecting to Build host…".into());
        }
        "pane-actions" | "rename" => {}
        "machines-single" | "machines-multiple" | "agents-priority" | "workspace-actions" => app.invoke_navigate(ui::Route::Machines),
        "entry" => app.invoke_navigate(ui::Route::Entry),
        "host-verification" | "host-key-changed" | "sign-in" => {
            app.set_auth_prompt_title("developer@example.invalid".into());
            app.set_auth_prompt_detail("Build host · SSH port 22".into());
            app.set_auth_fingerprint("SHA256:RGrDwUnCZcdmYZzmiuxYmMYkvwgfFiLjLT4WtlBCXZk".into());
            app.set_auth_previous_fingerprint("SHA256:lAu8PUKlzcCHSZlsOx90CDglifSQcpJcopEPjqTwPV4".into());
            app.set_auth_password_saving_allowed(true);
            app.invoke_present_auth_prompt(if state == "sign-in" { 2 } else if state == "host-key-changed" { 1 } else { 0 });
        }
        "missing-herdr" => {
            app.set_setup_available(false);
            app.set_setup_detail("Herdr was not found on Build host. Use an SSH terminal, or check again after it is available.".into());
            app.set_setup_sessions(model(vec![]));
            app.invoke_navigate(ui::Route::Setup);
        }
        "terminal-attention-zero" => app.set_attention_count(0),
        "terminal-attention-three" => app.set_attention_count(3),
        "terminal-attention-unknown" => app.set_attention_unknown(true),
        "terminal-attention-9" => app.set_attention_count(12),
        "hosts" => app.invoke_navigate(ui::Route::Hosts),
        "host-sessions" | "host-empty" | "host-error" | "host-actions" => {
            app.set_host_detail_id("host-demo".into());
            if state == "host-empty" {
                app.set_host_sessions(model(vec![]));
            }
            if state == "host-error" {
                let mut host = app.get_host_detail();
                host.state = "Error".into();
                host.detail = "SSH: Connection refused (os error 111)".into();
                app.set_host_detail(host);
                app.set_host_sessions(model(vec![]));
            }
            app.invoke_navigate(if state == "host-actions" {
                ui::Route::HostActions
            } else {
                ui::Route::Hosts
            });
        }
        "shown-sessions" => app.invoke_navigate(ui::Route::Sessions),
        "connection" => app.invoke_navigate(ui::Route::Connection),
        "setup" => app.invoke_navigate(ui::Route::Setup),
        "session-name" => app.invoke_navigate(ui::Route::SessionName),
        "credentials" => app.invoke_navigate(ui::Route::Credentials),
        "keys" => app.invoke_navigate(ui::Route::Keys),
        "key-editor" => app.invoke_navigate(ui::Route::KeyEditor),
        "trusted-hosts" => app.invoke_navigate(ui::Route::TrustedHosts),
        "key-files" => app.invoke_navigate(ui::Route::KeyFiles),
        "key-details" => app.invoke_navigate(ui::Route::KeyDetails),
        "key-picker" => app.invoke_navigate(ui::Route::KeyPicker),
        "menu" => app.invoke_navigate(ui::Route::Menu),
        "settings" => app.invoke_navigate(ui::Route::Settings),
        "help" => app.invoke_navigate(ui::Route::Help),
        _ => return Err(format!("Unknown state {state}").into()),
    }
    Ok(())
}

pub fn actions(state: &str) -> &[&str] {
    match state {
        "pane-actions" => &["Menu: Connected", "Pane actions", "Pane actions for Review terminal navigation"],
        "workspace-actions" => &["Workspace actions for kherdr"],
        "rename" => &["Menu: Connected", "Pane actions", "Pane actions for Review terminal navigation", "Rename pane"],
        _ => &[],
    }
}
