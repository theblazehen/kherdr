//! Owns setup/discovery admission, cancellation, and exactly-once UI settlement.
use crate::{App, LOCAL_HOST, PromptOwner, auth, connection, setup, ui, with_app};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::sync::Arc;
use ui::AppWindow;

#[derive(Default)]
pub(crate) struct Operation {
    worker: Option<setup::Setup>,
    profile: Option<connection::Profile>,
    generation: u64,
}
impl Operation {
    fn stop(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if let Some(mut worker) = self.worker.take() {
            if let Err(error) = worker.stop() {
                eprintln!("Setup cleanup: {error}");
            }
        }
    }
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
    pub(crate) fn profile(&self) -> Option<&connection::Profile> {
        self.profile.as_ref()
    }
    pub(crate) fn failure(&self) -> Option<String> {
        self.worker.as_ref().and_then(setup::Setup::failure)
    }
    pub(crate) fn answer(&self, answer: auth::Answer) -> Result<(), String> {
        self.worker
            .as_ref()
            .ok_or("Setup operation ended".to_string())?
            .answer_auth(answer)
    }
}

impl App {
    pub(crate) fn prepare_setup(&mut self, ui: &AppWindow, id: &str, visible: bool) -> bool {
        self.cancel_setup(ui);
        let profile = match self.connections.profile(id) {
            Ok(profile) => profile.clone(),
            Err(error) => {
                ui.set_connection_error(error.into());
                return false;
            }
        };
        self.cancel_interaction(ui);
        ui.set_setup_host_id(profile.id.clone().into());
        ui.set_setup_host_name(profile.name.clone().into());
        ui.set_setup_session_name(profile.herdr_session.clone().into());
        ui.set_setup_binary(profile.herdr_binary.clone().into());
        ui.set_setup_sessions(ModelRc::default());
        ui.set_setup_inspected(false);
        ui.set_setup_available(false);
        ui.set_setup_detail(
            "Discovering running Herdr sessions without starting or stopping servers.".into(),
        );
        ui.set_setup_start_allowed(false);
        self.connection_model(ui);
        ui.set_setup_confirmation(false);
        if visible {
            ui.invoke_navigate(ui::Route::Setup);
        }
        self.setup.profile = Some(profile);
        true
    }

    pub(crate) fn show_setup(&mut self, ui: &AppWindow, id: &str) {
        if !self.prepare_setup(ui, id, true) {
            return;
        }
        self.run_setup(ui, setup::Action::Inspect);
    }

    pub(crate) fn new_herdr_session(&mut self, ui: &AppWindow, id: &str) {
        if !self.prepare_setup(ui, id, false) { return; }
        ui.set_new_session_name("".into());
        ui.set_session_name_return(ui::Route::Hosts);
        ui.invoke_navigate(ui::Route::SessionName);
    }

    pub(crate) fn inspect_host_sessions(&mut self, ui: &AppWindow, id: &str) {
        if id == LOCAL_HOST {
            self.cancel_setup(ui);
            return;
        }
        if !self.prepare_setup(ui, id, false) {
            return;
        }
        self.run_setup(ui, setup::Action::Inspect);
    }

    pub(crate) fn inspect_session_name(&mut self, ui: &AppWindow, name: &str) {
        if ui.get_route() != ui::Route::SessionName || ui.get_setup_busy() { return; }
        if name.is_empty() || name.len() > 64 || matches!(name, "." | "..")
            || !name.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')) {
            ui.set_connection_error("Use 1–64 letters, digits, dots, underscores or hyphens for the session name.".into());
            return;
        }
        let Some(profile) = &mut self.setup.profile else { return; };
        profile.herdr_session = name.to_owned();
        ui.set_setup_session_name(name.into());
        ui.set_connection_error("".into());
        ui.invoke_navigate(ui::Route::Setup);
        self.run_setup(ui, setup::Action::Inspect);
    }

    pub(crate) fn cancel_setup(&mut self, ui: &AppWindow) {
        let was_busy = ui.get_setup_busy();
        self.setup.stop();
        if matches!(self.auth_owner, Some(PromptOwner::Setup(_))) {
            self.auth_owner = None;
            self.auth_prompt = None;
            ui.invoke_hide_auth_prompt();
            ui.set_auth_secret("".into());
        }
        ui.set_setup_busy(false);
        if was_busy { ui.set_setup_inspected(false); ui.set_setup_detail("Discovery cancelled. Refresh to try again.".into()); }
        if ui.get_setup_visible() {
            ui.invoke_navigate(ui::Route::Hosts);
        }
        ui.set_setup_confirmation(false);
    }

    pub(crate) fn run_setup(&mut self, ui: &AppWindow, action: setup::Action) {
        if ui.get_setup_busy()
            || self.setup.profile.is_none()
            || (action != setup::Action::Inspect && !ui.get_setup_visible())
        {
            return;
        }
        if action == setup::Action::Start && !ui.get_setup_start_allowed() {
            return;
        }
        let Some(profile) = self.setup.profile.clone() else {
            return;
        };
        self.setup.stop();
        self.cancel_interaction(ui);
        self.auth_owner = None;
        let generation = self.setup.generation;
        ui.set_setup_busy(true);
        ui.set_setup_inspected(false);
        ui.set_setup_start_allowed(false);
        ui.set_setup_confirmation(false);
        let weak = ui.as_weak();
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancellation = cancelled.clone();
        let pending = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        match setup::Setup::start(
            profile,
            self.auth_root.clone(),
            action,
            move |event| {
                use std::sync::atomic::Ordering;
                if cancelled.load(Ordering::Acquire) {
                    return Ok(());
                }
                if pending.fetch_add(1, Ordering::AcqRel) >= 8 {
                    pending.fetch_sub(1, Ordering::AcqRel);
                    return Err("Setup UI stopped consuming events".into());
                }
                let delivered = pending.clone();
                let result = weak.upgrade_in_event_loop(move |ui| {
                    delivered.fetch_sub(1, Ordering::AcqRel);
                    with_app(|app| {
                        if app.setup.generation == generation {
                            app.setup_event(&ui, event);
                        }
                    });
                });
                if result.is_err() {
                    pending.fetch_sub(1, Ordering::AcqRel);
                }
                result.map_err(|error| format!("Cannot deliver setup result: {error}"))
            },
            move || cancellation.store(true, std::sync::atomic::Ordering::Release),
        ) {
            Ok(setup) => self.setup.worker = Some(setup),
            Err(error) => {
                ui.set_setup_busy(false);
                ui.set_setup_detail(error.into());
            }
        }
        self.connection_model(ui);
    }

    pub(crate) fn setup_event(&mut self, ui: &AppWindow, event: setup::Event) {
        if !matches!(&event, setup::Event::Authentication(_)) {
            // Settle once through callback or watchdog. Fence queued events
            // before joining so failed delivery cannot strand the busy UI.
            self.setup.stop();
            ui.set_setup_busy(false);
            if matches!(self.auth_owner, Some(PromptOwner::Setup(_))) {
                self.auth_owner = None;
                self.auth_prompt = None;
                ui.invoke_hide_auth_prompt();
            }
        }
        match event {
            setup::Event::Authentication(prompt) => {
                self.auth_prompt = None;
                self.authentication_prompt(ui, prompt, PromptOwner::Setup(self.setup.generation));
            }
            setup::Event::Inspected {
                version,
                binary,
                detail,
                start_allowed,
                sessions,
                ..
            } => {
                ui.set_setup_detail(detail.into());
                ui.set_setup_inspected(true);
                ui.set_setup_available(!version.is_empty());
                ui.set_setup_sessions(ModelRc::new(VecModel::from(
                    sessions
                        .into_iter()
                        .map(Into::into)
                        .collect::<Vec<slint::SharedString>>(),
                )));
                ui.set_setup_start_allowed(start_allowed);
                if !binary.is_empty() {
                    ui.set_setup_binary(binary.clone().into());
                    if let Some(profile) = &mut self.setup.profile {
                        profile.herdr_binary = binary;
                    }
                }
                self.connection_model(ui);
                if version.is_empty() { ui.invoke_navigate(ui::Route::Setup); }
            }
            setup::Event::Completed { binary, detail } => {
                ui.set_setup_detail(detail.into());
                if let Some(profile) = &mut self.setup.profile {
                    profile.herdr_binary = binary.clone();
                    ui.set_setup_binary(binary.into());
                }
                ui.set_setup_start_allowed(false);
                self.connection_model(ui);
                self.run_setup(ui, setup::Action::Inspect);
            }
            setup::Event::Failed { message } => {
                ui.set_setup_detail(message.into());
                ui.set_setup_start_allowed(false);
                ui.set_setup_inspected(false);
                self.connection_model(ui);
            }
        }
    }
}
