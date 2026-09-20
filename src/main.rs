mod appearance;
mod atomic_file;
mod auth;
mod auth_ui;
mod client;
mod clipboard;
mod connection;
mod connection_ui;
mod credentials;
mod fonts;
mod graphics;
mod input;
mod input_ui;
use input_ui::physical_input;
mod keyboard;
mod endpoint;
mod local_runtime;
mod local_sessions;
mod navigation_ui;
mod platform;
mod presentation;
mod terminal;
mod ssh;
mod shell;
mod session;
mod setup;
mod setup_ui;
mod state;
mod trust;
mod ui;
mod ui_events;
use ui_events::{Event, UiEventQueue};

use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::Arc, time::Duration};
use slint::{ComponentHandle, Model, VecModel};
use appearance::TextSize;
use client::Client;
use session::{Session, StartPolicy};
use ui::AppWindow;

thread_local! { static APP: RefCell<Option<App>> = const { RefCell::new(None) }; }
const INPUT_LIMIT: usize = 512 * 1024;
const LOCAL_SESSION: u64 = 1;
const LOCAL_HOST: &str = "@local";

#[derive(Clone, Copy, PartialEq, Eq)]
enum PromptOwner { Endpoint(u64), Pane(u64), Setup(u64) }

struct LocalPane {
    id: u64,
    token: u64,
    prepared: shell::Prepared,
    client: Option<shell::Client>,
    prompt: Option<auth::Prompt>,
    launched: bool,
    closing: bool,
    ended: bool,
    closed_confirmed: bool,
    detail: String,
}

struct App {
    endpoints: session::Endpoints,
    identities: session::Identities,
    local_runtime: local_runtime::Runtime,
    local_panes: std::collections::BTreeMap<u64, LocalPane>,
    pending_local_focus: Option<u64>,
    discovery_pending: bool,
    setup: setup_ui::Operation,
    presentation: presentation::Presentation,
    clipboard: clipboard::Clipboard,
    connections: connection::Store,
    auth_root: PathBuf,
    keys: credentials::KeyStore,
    auth_prompt: Option<u64>,
    auth_owner: Option<PromptOwner>,
    key_job: u64,
    text_size: TextSize,
    text_size_path: PathBuf,
    input_ready: bool,
    epoch: u64,
    cols: u16,
    height: u16,
    keyboard: keyboard::Keyboard,
    selection: Option<(usize, usize)>,
    corpus: bool,
    bytes: u64,
    collapsed_groups:std::collections::BTreeMap<u64, std::collections::HashSet<String>>,
    expanded_machines:std::collections::HashSet<u64>,
}

fn with_app(f: impl FnOnce(&mut App)) {
    APP.with(|slot| { if let Some(app) = slot.borrow_mut().as_mut() { f(app); } });
}
fn status(ui: &AppWindow, message: impl AsRef<str>) {
    eprintln!("kherdr: {}", message.as_ref());
    ui.set_connection_status(message.as_ref().into());
}

impl App {
    fn update_ready(&mut self, ui: &AppWindow) {
        let ready = self.endpoints.active.ready() && self.local_input_ready();
        let became_ready = ready && !self.input_ready;
        self.input_ready = ready;
        ui.set_input_ready(ready);
        ui.set_selection_pending(self.endpoints.active.selection_pending || self.pending_local_focus.is_some());
        if became_ready { status(ui, "Connected"); }
    }
    fn connection_key_paths(&self) -> Vec<PathBuf> {
        self.endpoints.iter().filter_map(|session| session.profile.as_ref()?.config.identity.as_ref().map(PathBuf::from))
            .chain(self.local_panes.values().filter_map(|pane| pane.prepared.profile.config.identity.as_ref().map(PathBuf::from))).collect()
    }
    fn answer_connection_auth(&mut self, ui: &AppWindow, answer: auth::Answer) {
        let owner = self.auth_owner.take();
        let result = match owner {
            Some(PromptOwner::Setup(generation)) if generation == self.setup.generation() =>
                self.setup.answer(answer),
            Some(PromptOwner::Endpoint(token)) => self.endpoints.by_token(token)
                .ok_or("Session is no longer shown on this device".to_owned()).and_then(|session| session.answer(answer)),
            Some(PromptOwner::Pane(token)) if self.focused_local_pane().is_some_and(|pane| pane.token == token)
                && self.endpoints.active.focus.as_ref().is_some_and(|focus| self.endpoints.active.client.as_ref().is_some_and(|client| client.confirms_focus(focus))) => {
                let pane = self.local_panes.values_mut().find(|pane| pane.token == token).ok_or("SSH pane is no longer open".to_string());
                pane.and_then(|pane| {
                    if pane.prompt.as_ref().map(|prompt| prompt.id) != Some(answer.id) { return Err("SSH sign-in prompt changed".into()); }
                    let result = pane.client.as_ref().ok_or("SSH control is no longer attached")?.answer_auth(answer);
                    pane.prompt = None;
                    result
                })
            }
            _ => Err("Sign-in belongs to a previous connection or terminal".into()),
        };
        if let Err(error) = result {
            if matches!(owner, Some(PromptOwner::Setup(_))) { ui.set_setup_detail(error.into()); }
            else { status(ui, error); }
        }
        self.auth_prompt = None; ui.invoke_hide_auth_prompt(); ui.set_auth_secret("".into());
        self.sync_session(ui); self.connection_model(ui);
    }
    fn sync_session(&mut self, ui: &AppWindow) {
        ui.set_connected(self.endpoints.active.connected);
        ui.set_connecting_or_retrying(self.endpoints.active.connecting || self.endpoints.active.retry_at.is_some());
        ui.set_current_endpoint_local(self.endpoints.active.id == LOCAL_SESSION);
        ui.set_capability_status(if self.endpoints.active.id != LOCAL_SESSION {
            "Remote Herdr — independent server session."
        } else if self.focused_local_pane().is_some() {
            "SSH in local Herdr — UI detachment preserves this pane; SSH loss may end remote commands."
        } else { "Local Herdr — shells belong to this Kindle's server, not the UI connection." }.into());
        if !ui.get_setup_visible() && !matches!(self.auth_owner, Some(PromptOwner::Setup(_))) {
            let browsing = ui.get_route() == ui::Route::Hosts && !ui.get_host_detail_id().is_empty();
            let host_prompt = browsing.then(|| self.endpoints.iter().find(|session| {
                session.profile.as_ref().is_some_and(|p| p.id == ui.get_host_detail_id().as_str()) && session.prompt.is_some()
            })).flatten().and_then(|session| session.prompt.clone().map(|prompt| (PromptOwner::Endpoint(session.token), prompt)));
            let prompt = host_prompt.or_else(|| (!ui.get_connections_visible()).then(|| self.endpoints.active.prompt.clone().map(|prompt| (PromptOwner::Endpoint(self.endpoints.active.token), prompt))).flatten())
                .or_else(|| self.focused_local_pane().and_then(|pane| {
                    if ui.get_connections_visible() || self.pending_local_focus.is_some_and(|id| id != pane.id) { return None; }
                    let coherent = self.endpoints.active.focus.as_ref().is_some_and(|focus|
                        self.endpoints.active.client.as_ref().is_some_and(|client| client.confirms_focus(focus)));
                    if !coherent { return None; }
                    pane.prompt.clone().map(|prompt| (PromptOwner::Pane(pane.token), prompt))
                }));
            if let Some((owner, prompt)) = prompt {
                if (!ui.get_connections_visible() || browsing) && (self.auth_prompt != Some(prompt.id) || self.auth_owner != Some(owner)) {
                    self.authentication_prompt(ui, prompt, owner);
                }
            } else {
                self.auth_prompt = None; self.auth_owner = None;
                ui.invoke_hide_auth_prompt(); ui.set_auth_secret("".into());
            }
        }
        self.update_ready(ui);
        self.navigation_model(ui);
        if let Some(pane) = self.pending_local_focus.and_then(|id| self.local_panes.get(&id)) { status(ui, &pane.detail); }
        else if let Some(pane) = self.focused_local_pane() { status(ui, &pane.detail); }
        else if self.endpoints.active.id == LOCAL_SESSION && self.discovery_pending { status(ui, "Reattaching local SSH controls"); }
        else { status(ui, &self.endpoints.active.detail); }
    }
    fn start_session(session: &mut Session, token: u64, root: PathBuf, runtime: &local_runtime::Runtime, ui: &AppWindow, selected: bool, policy: StartPolicy) -> Result<(), String> {
        session.prepare_start(token, policy)?;
        let queue = UiEventQueue::new();
        let cancel = Arc::clone(&queue); let weak = ui.as_weak();
        let client = if let Some(profile) = &session.profile {
            let mut config = profile.config.clone();
            config.remote_command = client::remote_command(&profile.herdr_binary, &profile.herdr_session)?;
            Client::start(config, profile.name.clone(), root,
                move |event| queue.send(&weak, token, Event::Herdr(event)), move || cancel.cancel())?
        } else {
            runtime.ensure_started()?;
            Client::start_local(runtime.endpoint_socket.clone(),
                move |event| queue.send(&weak, token, Event::Herdr(event)), move || cancel.cancel())?
        };
        client.set_active(selected)?;
        session.client = Some(client);
        Ok(())
    }
    fn connect(&mut self, ui: &AppWindow) {
        self.cancel_interaction(ui); self.epoch = self.epoch.wrapping_add(1);
        let policy = if self.endpoints.active.automatic_connection { StartPolicy::Automatic } else { StartPolicy::Explicit };
        let result = self.identities.allocate().and_then(|token| Self::start_session(&mut self.endpoints.active, token, self.auth_root.clone(), &self.local_runtime, ui, true, policy));
        if let Err(error) = result { self.endpoints.active.fail(error, true); }
        self.sync_session(ui); self.connection_model(ui);
        if let Err(error) = self.paint(ui) { self.connection_lost(ui, error); }
    }
    fn open_shell(&mut self, ui: &AppWindow, host: &str) {
        self.cancel_setup(ui);
        match self.connections.select(host) {
            Ok(profile) => if let Err(error) = self.open_ssh_pane(ui, profile, true, None) { ui.set_connection_error(error.into()); },
            Err(error) => ui.set_connection_error(error.into()),
        }
    }
    fn open_remote_profile(&mut self, ui: &AppWindow, profile: connection::Profile, automatic: bool, foreground: bool) -> Result<u64, String> {
        let existing = self.endpoints.iter().find(|session| session.profile.as_ref().is_some_and(|p|
            p.id == profile.id && p.herdr_session == profile.herdr_session)).map(|session| session.id);
        let id = if let Some(id) = existing { id } else {
            self.endpoints.admit(self.local_panes.len())?;
            let id = self.identities.allocate()?;
            let (cw, ch, _) = self.text_size.metrics();
            let mut session = Session::new(id, Some(profile), self.cols, self.height, cw, ch)?;
            session.automatic_connection = automatic;
            self.endpoints.parked.insert(id, session);
            id
        };
        if foreground { self.resume_session(ui, id); }
        let selected = id == self.endpoints.active.id;
        let session = if selected { &mut self.endpoints.active }
            else { self.endpoints.parked.get_mut(&id).ok_or("Remote session is no longer open")? };
        if !automatic { session.automatic_connection = false; session.allow_fallback = false; }
        let needs_start = !session.connected && !session.connecting && session.retry_at.is_none();
        if needs_start {
            if selected { self.connect(ui); }
            else {
                let token = self.identities.allocate()?;
                let session = self.endpoints.parked.get_mut(&id).ok_or("Remote session is no longer open")?;
                if let Err(error) = Self::start_session(session, token, self.auth_root.clone(), &self.local_runtime, ui, false, StartPolicy::Explicit) {
                    session.fail(error, true);
                }
                self.connection_model(ui);
            }
        }
        Ok(id)
    }
    fn set_session_visible(&mut self, ui: &AppWindow, host: &str, name: &str, visible: bool) {
        let existing = self.endpoints.iter().find(|session| session.profile.as_ref().is_some_and(|p|
            p.id == host && p.herdr_session == name)).map(|session| session.id);
        ui.set_connection_error("".into());
        if !visible {
            if let Some(id) = existing { self.close_session(ui, id); }
            return;
        }
        if let Some(id) = existing {
            let profile = self.endpoints.iter().find(|session| session.id == id).and_then(|session| session.profile.clone());
            if let Some(profile) = profile {
                if let Err(error) = self.open_remote_profile(ui, profile, false, false) { ui.set_connection_error(error.into()); }
            }
            self.connection_model(ui);
            return;
        }
        if ui.get_setup_busy() || !ui.get_setup_inspected() || ui.get_setup_host_id().as_str() != host
            || !ui.get_setup_sessions().iter().any(|session| session.as_str() == name) {
            ui.set_connection_error("Refresh this host’s running sessions before selecting one.".into());
            return;
        }
        let Some(mut profile) = self.setup.profile().filter(|p| p.id == host).cloned() else { return; };
        profile.herdr_session = name.to_owned();
        let already_remembered = self.connections.profile(host).is_ok_and(|profile| profile.remembered_herdr_sessions.iter().any(|session| session == name));
        let result = self.endpoints.admit(self.local_panes.len())
            .and_then(|()| self.connections.remember_sessions(host, &[name.to_owned()]))
            .and_then(|()| self.open_remote_profile(ui, profile, false, false));
        if let Err(error) = result {
            if !already_remembered && !self.endpoints.iter().any(|session| session.profile.as_ref().is_some_and(|p| p.id == host && p.herdr_session == name)) {
                if let Err(rollback) = self.connections.forget_session(host, name) {
                    ui.set_connection_error(format!("{error}\nCould not remove the saved selection: {rollback}").into());
                    self.connection_model(ui);
                    return;
                }
            }
            ui.set_connection_error(error.into());
        }
        self.connection_model(ui);
    }
    fn restore_remembered_sessions(&mut self, ui: &AppWindow) {
        let selected = self.connections.selected().map(|profile| (profile.id.clone(), profile.herdr_session.clone()));
        let remembered: Vec<_> = self.connections.profiles().iter().flat_map(|profile| profile.remembered_herdr_sessions.iter().map(|name| {
            let mut profile = profile.clone(); profile.herdr_session = name.clone(); profile
        })).collect();
        let mut foreground = None;
        let mut omitted = 0usize;
        for profile in remembered {
            if self.endpoints.available(self.local_panes.len()) == 0 { omitted += 1; continue; }
            let selected_session = selected.as_ref().is_some_and(|(host, name)| *host == profile.id && *name == profile.herdr_session);
            match self.open_remote_profile(ui, profile, false, false) {
                Ok(id) if selected_session => foreground = Some(id),
                Ok(_) => {},
                Err(error) => ui.set_connection_error(error.into()),
            }
        }
        if omitted != 0 { ui.set_connection_error(format!("{} remembered Herdr session(s) were not reopened because the 16-session limit was reached.", omitted).into()); }
        if let Some(id) = foreground { self.resume_session(ui, id); }
    }
    fn resume_session(&mut self, ui: &AppWindow, id: u64) {
        if self.local_panes.contains_key(&id) { self.resume_local_pane(ui, id); return; }
        self.select_endpoint(ui, id, ui::Route::Terminal);
    }
    fn select_endpoint(&mut self, ui: &AppWindow, id: u64, route: ui::Route) {
        if id != LOCAL_SESSION { self.pending_local_focus = None; }
        if id != self.endpoints.active.id {
            if !self.endpoints.parked.contains_key(&id) { return; }
            self.cancel_interaction(ui); self.epoch = self.epoch.wrapping_add(1);
            self.endpoints.select(id);
            self.auth_prompt = None; self.auth_owner = None; ui.invoke_hide_auth_prompt(); ui.set_auth_secret("".into());
            self.presentation.reset_images(ui);
            self.endpoints.active.terminal.invalidate();
        }
        if id == LOCAL_SESSION {
            if let Err(error) = self.ensure_local(ui) { self.endpoints.active.fail(error, true); }
        }
        ui.invoke_navigate(route);
        self.expanded_machines.insert(id);
        if route == ui::Route::Terminal { ui.invoke_set_keyboard(true); }
        if let Err(error) = self.resize(ui, self.text_size).and_then(|_| self.paint(ui)) { self.endpoints.active.fail(error, true); }
        self.sync_session(ui); self.connection_model(ui);
    }
    fn close_session(&mut self, ui: &AppWindow, id: u64) {
        if self.local_panes.contains_key(&id) { self.close_local_pane(ui, id); return; }
        let remote = self.endpoints.iter().find(|session| session.id == id).and_then(|session| session.profile.as_ref()
            .map(|profile| (profile.id.clone(), profile.herdr_session.clone())));
        if let Some((host, name)) = &remote {
            if let Err(error) = self.connections.forget_session(host, name) { ui.set_connection_error(error.into()); return; }
        }
        if id == LOCAL_SESSION {
            if self.endpoints.active.id == id { self.disconnect(ui); }
            else if let Some(session) = self.endpoints.parked.get_mut(&id) { session.auto_reconnect = false; session.stop(); session.detail = "UI detached — local shells and authenticated SSH panes keep running".into(); }
        } else {
            if id == self.endpoints.active.id { self.select_endpoint(ui, LOCAL_SESSION, ui.get_route()); }
            if let Some(mut session) = self.endpoints.parked.remove(&id) { session.auto_reconnect = false; session.stop(); }
        }
        self.sync_session(ui); self.connection_model(ui);
    }
    fn disconnect_host(&mut self, ui: &AppWindow, host: &str) {
        if host == LOCAL_HOST { self.close_session(ui, LOCAL_SESSION); return; }
        let ids: Vec<_> = self.endpoints.iter().filter(|session| session.profile.as_ref().is_some_and(|profile| profile.id == host)).map(|session| session.id).collect();
        for id in ids { self.close_session(ui, id); }
    }
    fn watch_connection(&mut self, ui: &AppWindow) {
        let weak = ui.as_weak();
        slint::Timer::single_shot(Duration::from_secs(1), move || {
            if let Some(ui) = weak.upgrade() { with_app(|app| {
                if let Some(error) = app.setup.failure() {
                    app.setup_event(&ui, setup::Event::Failed { message: error });
                }
                let now = std::time::Instant::now();
                let selected = app.endpoints.active.id;
                let mut changed = false;
                for session in std::iter::once(&mut app.endpoints.active).chain(app.endpoints.parked.values_mut()) {
                    if let Some(error) = session.client.as_ref().and_then(Client::failure) { session.fail(error.message, error.fatal); changed = true; }
                    if session.retry_at.is_some_and(|deadline| now >= deadline) {
                        let result = app.identities.allocate().and_then(|token|
                            Self::start_session(session, token, app.auth_root.clone(), &app.local_runtime, &ui, session.id == selected, StartPolicy::Reconnect));
                        if let Err(error) = result { session.fail(error, true); }
                        changed = true;
                    }
                }
                for pane in app.local_panes.values_mut().filter(|pane| !pane.ended) {
                    if let Some(error) = pane.client.as_ref().and_then(shell::Client::failure) {
                        pane.ended = true; pane.prompt = None; pane.detail = error; changed = true;
                    }
                }
                if changed { app.cancel_interaction(&ui); app.focus_pending_local(&ui); app.sync_session(&ui); app.connection_model(&ui); }
                app.watch_connection(&ui);
            }); }
        });
    }
    fn connection_lost(&mut self, ui: &AppWindow, message: String) {
        self.cancel_interaction(ui); self.epoch = self.epoch.wrapping_add(1);
        self.endpoints.active.fail(message, !self.endpoints.active.auto_reconnect);
        self.sync_session(ui); self.connection_model(ui);
    }
    fn disconnect(&mut self, ui: &AppWindow) {
        self.cancel_interaction(ui); self.epoch = self.epoch.wrapping_add(1);
        self.endpoints.active.auto_reconnect = false; self.endpoints.active.allow_fallback = false; self.endpoints.active.stop();
        self.endpoints.active.detail = if self.endpoints.active.id == LOCAL_SESSION { "UI detached — local shells and authenticated SSH panes keep running" }
            else { "UI detached — remote Herdr keeps running" }.into();
        self.sync_session(ui); self.connection_model(ui);
    }
    fn choose(&mut self, ui: &AppWindow, pane: &str) {
        let result = self.endpoints.active.client.as_ref().ok_or("Not connected".to_string()).and_then(|client| client.focus(pane));
        self.navigation_result(ui, result);
    }
    fn navigation_result(&mut self, ui: &AppWindow, result: Result<(), String>) {
        self.pending_local_focus = None;
        match result {
            Ok(()) => {
                self.cancel_interaction(ui);
                self.endpoints.active.selection_pending = true;
                self.update_ready(ui);
                status(ui, "Switching terminal");
            }
            Err(error) => status(ui, error),
        }
    }
    fn responses(&mut self) -> Result<(), String> { self.endpoints.active.responses() }
    fn paint(&mut self, ui: &AppWindow) -> Result<(), String> {
        // A batch can contain a commit followed by an incomplete update.
        let Some(snapshot) = self.endpoints.active.snapshot()? else { return Ok(()); };
        self.presentation.paint(ui, snapshot, self.text_size)
    }
    fn resize(&mut self, ui: &AppWindow, text_size: TextSize) -> Result<(), String> {
        let (cell_width, cell_height, font_size) = text_size.metrics();
        let cols = (ui.get_terminal_width() / f32::from(cell_width)).floor().clamp(1.0, 1000.0) as u16;
        let rows = (ui.get_terminal_height() / f32::from(cell_height)).floor().clamp(1.0, 1000.0) as u16;
        if (cols, rows, text_size) == (self.endpoints.active.cols, self.endpoints.active.rows, self.text_size)
            && (cell_width, cell_height) == (self.endpoints.active.cell_width, self.endpoints.active.cell_height) { return Ok(()); }
        self.cancel_interaction(ui);
        self.endpoints.active.terminal.set_cell_size(u32::from(cell_width), u32::from(cell_height))?;
        self.endpoints.active.terminal.resize(cols, rows)?;
        self.cols = cols; self.height = rows;
        self.endpoints.active.cols = cols; self.endpoints.active.rows = rows;
        self.endpoints.active.cell_width = cell_width; self.endpoints.active.cell_height = cell_height;
        self.text_size = text_size;
        ui.set_text_size(text_size.index());
        ui.set_cell_width(f32::from(cell_width)); ui.set_cell_height(f32::from(cell_height));
        ui.set_terminal_font_size(font_size);
        if self.endpoints.active.connected {
            if let Some(client) = self.endpoints.active.client.as_ref() {
                client.resize(cols, rows, cell_width, cell_height)?;
            }
        }
        self.responses()?;
        self.paint(ui)?;
        eprintln!("kherdr geometry: {cols}x{rows}, cell={cell_width}x{cell_height}, font={font_size}");
        Ok(())
    }





}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let first = args.next();
    if first.as_deref() == Some("--ssh-pane") {
        let descriptor = PathBuf::from(args.next().ok_or("--ssh-pane requires a private descriptor path")?);
        if args.next().is_some() { return Err("--ssh-pane accepts exactly one descriptor".into()); }
        return shell::run(&descriptor).map_err(Into::into);
    }
    if first.as_deref() == Some("--prepare-state") {
        let destination = PathBuf::from(args.next().ok_or("--prepare-state requires a destination directory")?);
        let source = args.next().map(PathBuf::from);
        if args.next().is_some() { return Err("--prepare-state accepts a destination and optional legacy directory".into()); }
        return state::prepare(&destination, source.as_deref()).map_err(Into::into);
    }
    let mut config = PathBuf::from("/mnt/us/kherdr/etc/connection.ini");
    let mut corpus = false;
    let mut args = first.into_iter().chain(args);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => config = args.next().ok_or("--config requires a path")?.into(),
            "--corpus" => corpus = true,
            "--version" => { println!("kherdr {}", env!("CARGO_PKG_VERSION")); return Ok(()); }
            _ => return Err(format!("Unknown argument {arg}; use --config PATH, --version or --corpus").into()),
        }
    }
    platform::install()?;
    fonts::register()?;
    let text_size_path = config.with_file_name("text-size.json");
    let text_size = TextSize::load(&text_size_path)?;
    let (cell_width, cell_height, font_size) = text_size.metrics();
    let ui = AppWindow::new()?;
    ui.global::<ui::PointerTiming>().on_release_duration_ms(platform::pointer_release_duration_ms);
    let rows = Rc::new(VecModel::default());
    ui.set_rows(rows.clone().into());
    ui.set_text_size(text_size.index());
    ui.set_cell_width(f32::from(cell_width)); ui.set_cell_height(f32::from(cell_height));
    ui.set_terminal_font_size(font_size); ui.set_terminal_font_family("KindleBlackboxC".into());
    let weak = ui.as_weak();
    let clipboard = clipboard::Clipboard::new(move |error| {
        if let Err(delivery) = weak.upgrade_in_event_loop(move |ui| status(&ui,error)) { eprintln!("clipboard error delivery failed: {delivery}"); }
    })?;
    let connections = connection::Store::load(&config);
    let auth_root = config.parent().unwrap_or_else(|| std::path::Path::new(".")).to_path_buf();
    let mut keys = credentials::KeyStore::load(&auth_root);
    for profile in connections.profiles() {
        if let Some(identity) = &profile.config.identity {
            if let Err(error) = keys.register_existing(std::path::Path::new(identity)) {
                ui.set_auth_error(error.into());
            }
        }
    }
    let local_runtime = local_runtime::Runtime::new(&auth_root)?;
    let mut session = Session::new(LOCAL_SESSION, None, 72, 36, cell_width, cell_height)?;
    if !session.terminal.take_responses()?.is_empty() { return Err("Unexpected terminal responses before connection".into()); }
    let app = App { endpoints: session::Endpoints::new(session), identities: session::Identities::new(LOCAL_SESSION),
        local_runtime, local_panes: std::collections::BTreeMap::new(), pending_local_focus: None, discovery_pending: false,
        presentation: presentation::Presentation::new(rows), clipboard, connections, auth_root, keys, auth_prompt: None, auth_owner: None, key_job: 0,
        setup: setup_ui::Operation::default(),
        text_size, text_size_path, input_ready: false, epoch: 0, cols: 72, height: 36,
        keyboard: keyboard::Keyboard::default(), selection: None, corpus,
        bytes: 0,collapsed_groups:std::collections::BTreeMap::new(),
        expanded_machines: std::collections::HashSet::from([LOCAL_SESSION]) };
    APP.with(|slot| *slot.borrow_mut() = Some(app));
    auth_ui::register(&ui);
    let weak = ui.as_weak();
    ui.on_management_closed(move || {
        let weak = weak.clone();
        slint::Timer::single_shot(Duration::ZERO, move || {
            if let Some(ui) = weak.upgrade() { if !ui.get_connections_visible() { with_app(|app| app.sync_session(&ui)); } }
        });
    });
    let weak = ui.as_weak();
    ui.on_setup_herdr(move |id| { if let Some(ui) = weak.upgrade() { with_app(|app| app.show_setup(&ui, &id)); } });
    let weak = ui.as_weak();
    ui.on_new_herdr_session(move |id| { if let Some(ui) = weak.upgrade() { with_app(|app| app.new_herdr_session(&ui, &id)); } });
    let weak = ui.as_weak();
    ui.on_inspect_setup(move || { if let Some(ui) = weak.upgrade() { with_app(|app| app.run_setup(&ui, setup::Action::Inspect)); } });
    let weak = ui.as_weak();
    ui.on_confirm_setup(move || { if let Some(ui) = weak.upgrade() { with_app(|app| app.run_setup(&ui, setup::Action::Start)); } });
    let weak = ui.as_weak();
    ui.on_inspect_session_name(move |name| { if let Some(ui) = weak.upgrade() { with_app(|app| app.inspect_session_name(&ui, &name)); } });
    let weak = ui.as_weak();
    ui.on_cancel_setup(move || { if let Some(ui) = weak.upgrade() { with_app(|app| app.cancel_setup(&ui)); } });
    let weak = ui.as_weak();
    ui.on_open_host(move |id| { if let Some(ui) = weak.upgrade() { with_app(|app| {
        if id.as_str() == LOCAL_HOST { app.resume_session(&ui, LOCAL_SESSION); return; }
        app.cancel_interaction(&ui); ui.set_connection_error("".into()); ui.set_host_detail_id(id.clone());
        ui.invoke_navigate(ui::Route::Hosts); app.connection_model(&ui); app.inspect_host_sessions(&ui, &id);
    }); } });
    let weak = ui.as_weak();
    ui.on_open_shell(move |id| { if let Some(ui) = weak.upgrade() { with_app(|app| app.open_shell(&ui, &id)); } });
    let weak = ui.as_weak();
    ui.on_resume_session(move |id| { if let (Some(ui), Ok(id)) = (weak.upgrade(), id.parse::<u64>()) { with_app(|app| app.resume_session(&ui, id)); } });
    let weak = ui.as_weak();
    ui.on_close_session(move |id| { if let (Some(ui), Ok(id)) = (weak.upgrade(), id.parse::<u64>()) { with_app(|app| app.close_session(&ui, id)); } });
    let weak = ui.as_weak();
    ui.on_disconnect_host(move |id| { if let Some(ui) = weak.upgrade() { with_app(|app| app.disconnect_host(&ui, &id)); } });
    let weak = ui.as_weak();
    platform::on_clipboard_copy(move |text| {
        if let Some(ui) = weak.upgrade() {
            if ui.get_local_copy_allowed() {
                with_app(|app| {
                    if let Err(error) = app.clipboard.copy(text.to_owned()) { ui.set_auth_error(error.into()); }
                });
            }
        }
    });
    let weak = ui.as_weak();
    ui.on_new_connection(move || { if let Some(ui) = weak.upgrade() { with_app(|app| app.edit_connection(&ui, "")); } });
    let weak = ui.as_weak();
    ui.on_edit_connection(move |id| { if let Some(ui) = weak.upgrade() { with_app(|app| app.edit_connection(&ui, &id)); } });
    let weak = ui.as_weak();
    ui.on_save_connection(move |draft| { if let Some(ui) = weak.upgrade() { with_app(|app| app.save_connection(&ui, &draft)); } });
    let weak = ui.as_weak();
    ui.on_set_session_visible(move |host, name, visible| { if let Some(ui) = weak.upgrade() { with_app(|app| app.set_session_visible(&ui, &host, &name, visible)); } });
    let weak = ui.as_weak();
    ui.on_open_host_session(move |host, name| { if let Some(ui) = weak.upgrade() { with_app(|app| {
        app.set_session_visible(&ui, &host, &name, true);
        let id = app.endpoints.iter().find(|session| session.profile.as_ref()
            .is_some_and(|profile| profile.id.as_str() == host.as_str() && profile.herdr_session.as_str() == name.as_str())).map(|session| session.id);
        if let Some(id) = id { app.select_endpoint(&ui, id, ui::Route::Terminal); }
    }); } });
    let weak = ui.as_weak();
    ui.on_switch_session(move |id| { if let (Some(ui), Ok(id)) = (weak.upgrade(), id.parse::<u64>()) { with_app(|app| app.select_endpoint(&ui, id, ui::Route::Machines)); } });
    let weak = ui.as_weak();
    ui.on_toggle_machine(move |id| { if let (Some(ui), Ok(id)) = (weak.upgrade(), id.parse::<u64>()) { with_app(|app| {
        if !app.expanded_machines.remove(&id) { app.expanded_machines.insert(id); }
        app.navigation_model(&ui);
    }); } });
    let weak = ui.as_weak();
    ui.on_set_agent_priority(move |priority| { if let Some(ui) = weak.upgrade() { ui.set_agents_priority(priority); with_app(|app| app.navigation_model(&ui)); } });
    let weak = ui.as_weak();
    ui.on_navigate_terminal(move |endpoint,workspace,pane| { if let (Some(ui), Ok(id)) = (weak.upgrade(), endpoint.parse::<u64>()) { with_app(|app| {
        let valid = app.endpoints.iter().any(|session| session.id == id && session.connected && session.navigation_ready
            && if pane.is_empty() { session.workspaces.iter().any(|w| w.workspace_id == workspace.as_str()) }
               else { session.panes.iter().any(|p| p.pane_id == pane.as_str() && p.workspace_id == workspace.as_str()) });
        if !valid { status(&ui, "That terminal is no longer available"); return; }
        app.select_endpoint(&ui, id, ui::Route::Terminal);
        if pane.is_empty() {
            let result = app.endpoints.active.client.as_ref().ok_or("Not connected".to_string()).and_then(|client| client.focus_workspace(&workspace));
            app.navigation_result(&ui, result);
        } else { app.choose(&ui, &pane); }
    }); } });
    let weak = ui.as_weak();
    ui.on_select_pane(move |id| { if let Some(ui) = weak.upgrade() { with_app(|app| app.choose(&ui, &id)); } });
    let weak = ui.as_weak();
    ui.on_remove_connection(move |id| { if let Some(ui) = weak.upgrade() { with_app(|app| app.remove_connection(&ui, &id)); } });
    let weak = ui.as_weak();
    platform::on_focus_changed(move |active| {
        if let Some(ui) = weak.upgrade() {
            ui.invoke_application_activity(active);
            if !active { with_app(|app| app.cancel_interaction(&ui)); }
        }
    });
    let weak = ui.as_weak();
    ui.on_geometry(move |_,_| { let weak = weak.clone(); slint::Timer::single_shot(Duration::ZERO, move || { if let Some(ui) = weak.upgrade() { with_app(|app| {
        if let Err(error) = app.resize(&ui, app.text_size) { app.endpoints.active.auto_reconnect = false; app.connection_lost(&ui, error); }
    }); } }); });
    let weak = ui.as_weak();
    ui.on_set_text_size(move |index| { if let Some(ui) = weak.upgrade() { with_app(|app| {
        let Some(size) = TextSize::from_index(index) else { status(&ui, "Unknown text size"); return; };
        if let Err(error) = app.resize(&ui, size) { app.endpoints.active.auto_reconnect = false; app.connection_lost(&ui, error); return; }
        if let Err(error) = size.save(&app.text_size_path) { status(&ui, error); }
    }); } });
    let weak = ui.as_weak();
    ui.on_select_tab(move |tab| { if let Some(ui) = weak.upgrade() { with_app(|app| {
        let result = app.endpoints.active.client.as_ref().ok_or("Not connected".to_string()).and_then(|client| client.focus_tab(&tab));
        app.navigation_result(&ui, result);
    }); } });
    let weak=ui.as_weak();
    ui.on_stock_action(move |action,target,workspace,value|{if let Some(ui)=weak.upgrade(){with_app(|app|app.stock_action(&ui,&action,&target,&workspace,&value));}});

    let weak = ui.as_weak();
    ui.on_new_tab(move || { if let Some(ui) = weak.upgrade() { with_app(|app| {
        let result = app.endpoints.active.client.as_ref().ok_or("Not connected".to_string()).and_then(Client::new_tab);
        app.navigation_result(&ui, result);
    }); } });
    let weak = ui.as_weak();
    ui.on_new_workspace(move || { if let Some(ui) = weak.upgrade() { with_app(|app| {
        let result = app.endpoints.active.client.as_ref().ok_or("Not connected".to_string()).and_then(Client::new_workspace);
        app.navigation_result(&ui, result);
    }); } });
    let weak = ui.as_weak();
    ui.on_key(move |id| { if let Some(ui) = weak.upgrade() { with_app(|app| app.key(&ui,&id)); } });
    let weak = ui.as_weak();
    ui.on_keyboard_text(move |value| { if let Some(ui) = weak.upgrade() { with_app(|app| app.keyboard_input(&ui,&value,true)); } });
    let weak = ui.as_weak();
    ui.on_terminal_key(move |text,ctrl,alt,shift| {
        if let Some(ui) = weak.upgrade() { with_app(|app| {
            if let Some(mut input) = physical_input(&text) {
                input.ctrl = ctrl; input.alt = alt; input.shift = shift;
                input.consumed_shift = shift && !input.text.is_empty();
                app.send(&ui,&input);
            }
        }); }
    });
    let weak = ui.as_weak();
    ui.on_paste(move || { if let Some(ui) = weak.upgrade() { with_app(|app| app.paste(&ui)); } });
    let weak = ui.as_weak();
    ui.on_copy(move || { if let Some(ui) = weak.upgrade() { with_app(|app| app.copy(&ui)); } });
    let weak = ui.as_weak();
    ui.on_select_start(move |col,row| { if let Some(ui) = weak.upgrade() { with_app(|app| app.select(&ui,col,row,true)); } });
    let weak = ui.as_weak();
    ui.on_select_move(move |col,row| { if let Some(ui) = weak.upgrade() { with_app(|app| app.select(&ui,col,row,false)); } });
    let weak = ui.as_weak();
    ui.on_reconnect(move || { if let Some(ui) = weak.upgrade() { with_app(|app| app.connect(&ui)); } });
    let weak = ui.as_weak();
    ui.on_disconnect(move || { if let Some(ui) = weak.upgrade() { with_app(|app| app.disconnect(&ui)); } });
    let weak = ui.as_weak();
    ui.on_quit(move || { if let Some(ui) = weak.upgrade() { with_app(|app| { app.disconnect(&ui); for session in app.endpoints.parked.values_mut() { session.stop(); } }); } if let Err(error) = slint::quit_event_loop() { eprintln!("cannot quit: {error}"); } });
    let weak = ui.as_weak();
    ui.on_scroll(move |up,ticks| { if let Some(ui) = weak.upgrade() { with_app(|app| {
        app.scroll(&ui, up, ticks);
    }); } });
    let weak = ui.as_weak();
    ui.on_terminal_tap(move |column,row| { if let Some(ui) = weak.upgrade() { with_app(|app| app.terminal_click(&ui,column,row)); } });
    with_app(|app| {
        app.connection_model(&ui);
        app.key_model(&ui);
        app.keyboard.render(&ui);
        if app.corpus {
            ui.invoke_navigate(ui::Route::Terminal);
            status(&ui,"Local Ghostty rendering corpus — no remote session");
            let corpus = "\x1b[2J\x1b[HOn-device Ghostty / Rust / Slint\r\n\r\nUnicode: café é 日本語 Ελληνικά\r\nBox: ┌─────────┐\r\n     │ terminal│\r\n     └─────────┘\r\n\x1b[1mBold\x1b[0m  \x1b[3mItalic\x1b[0m  \x1b[4mUnderline\x1b[0m  \x1b[9mStrike\x1b[0m\r\n\x1b[7mInverse\x1b[0m  \x1b[31mRed\x1b[32m Green\x1b[34m Blue\x1b[0m\r\n\x1b]8;;https://example.org\x1b\\Hyperlink text — no OSC wrappers\x1b]8;;\x1b\\\r\n\r\nKeyboard toggle changes viewport, not terminal engine.\r\n";
            for byte in corpus.as_bytes() { if let Err(error) = app.endpoints.active.terminal.feed(&[*byte]) { status(&ui,error); break; } }
            if let Err(error) = app.paint(&ui) { status(&ui,error); }
        } else {
            ui.invoke_navigate(ui::Route::Entry);
            if let Err(error) = app.ensure_local(&ui) { app.endpoints.active.fail(error, true); }
            if let Err(error) = app.discover_local_panes(&ui) { ui.set_connection_error(error.into()); }
            app.restore_remembered_sessions(&ui);
            app.sync_session(&ui); app.connection_model(&ui);
        }
        app.watch_connection(&ui);
    });
    install_system_tray(&ui);
    eprintln!("kherdr application ready");
    let result = ui.run();
    with_app(|app| { app.disconnect(&ui); for session in app.endpoints.parked.values_mut() { session.stop(); } app.clipboard.stop(); eprintln!("kherdr counters: bytes={} paints={} rows={}",app.bytes,app.presentation.paints,app.presentation.rows_painted); });
    APP.with(|slot| { slot.borrow_mut().take(); });
    result?;
    Ok(())
}

fn install_system_tray(ui: &AppWindow) {
    use std::process::Command;
    use std::sync::mpsc::{self, RecvTimeoutError};
    let (send, receive) = mpsc::sync_channel(1);
    let weak = ui.as_weak();
    ui.on_system_settings(move || {
        if let Some(ui) = weak.upgrade() {
            with_app(|app| app.cancel_interaction(&ui));
            let _ = send.try_send(());
        }
    });
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let mut refresh = true;
        loop {
            if refresh {
                let time = Command::new("date").arg("+%H:%M").output().ok()
                    .filter(|output| output.status.success())
                    .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
                    .unwrap_or_else(|| "—".into());
                let battery = Command::new("lipc-get-prop").args(["com.lab126.powerd", "battLevel"]).output().ok()
                    .filter(|output| output.status.success())
                    .and_then(|output| String::from_utf8_lossy(&output.stdout).trim().parse::<u8>().ok())
                    .filter(|level| *level <= 100).map(|level| format!("{level}%"))
                    .unwrap_or_else(|| "—".into());
                if weak.upgrade_in_event_loop(move |ui| {
                    if ui.get_system_time().as_str() != time { ui.set_system_time(time.into()); }
                    if ui.get_system_battery().as_str() != battery { ui.set_system_battery(battery.into()); }
                }).is_err() { break; }
            }
            match receive.recv_timeout(Duration::from_secs(30)) {
                Ok(()) => {
                    refresh = false;
                    let result = Command::new("lipc-set-prop").args([
                        "com.lab126.kppchromebar", "configurePillState", "openedByTap",
                    ]).output();
                    if !result.is_ok_and(|output| output.status.success()) {
                        let _ = weak.upgrade_in_event_loop(|ui| status(&ui, "Amazon Quick Settings could not be opened"));
                    }
                }
                Err(RecvTimeoutError::Timeout) => refresh = true,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    });
}
