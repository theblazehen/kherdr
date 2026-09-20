//! Conversion at the UI boundary; persisted host data does not depend on Slint.
use crate::{
    App, LOCAL_HOST, LOCAL_SESSION, LocalPane,
    navigation_ui::update_navigation_entries,
    session::Session,
    ui::{self, AppWindow},
};
use crate::{
    connection::{AuthMethod, Config, Profile, SessionKind},
    ui::ConnectionDraft,
};
use std::collections::BTreeMap;
use slint::Model;

impl ConnectionDraft {
    pub(crate) fn new_host() -> Self {
        Self {
            port: "22".into(),
            auth_method: "key".into(),
            session_kind: "herdr".into(),
            herdr_session: "default".into(),
            herdr_binary: "herdr".into(),
            keepalive: "30".into(),
            ..Default::default()
        }
    }

    pub(crate) fn from_profile(profile: &Profile) -> Self {
        let config = &profile.config;
        Self {
            id: profile.id.clone().into(),
            name: profile.name.clone().into(),
            host: config.host.clone().into(),
            user: config.user.clone().unwrap_or_default().into(),
            identity: config.identity.clone().unwrap_or_default().into(),
            port: config.port.to_string().into(),
            command: config.remote_command.clone().into(),
            auth_method: match config.auth_method {
                AuthMethod::Key => "key",
                AuthMethod::Password => "password",
            }
            .into(),
            keepalive: config.keepalive.to_string().into(),
            compression: config.compression,
            session_kind: match profile.session_kind {
                SessionKind::Shell => "shell",
                SessionKind::Herdr => "herdr",
            }
            .into(),
            herdr_session: profile.herdr_session.clone().into(),
            herdr_binary: profile.herdr_binary.clone().into(),
        }
    }

    pub(crate) fn profile(&self) -> Result<Profile, String> {
        let mut values = BTreeMap::from([
            ("host", self.host.to_string()),
            ("port", self.port.to_string()),
            ("auth_method", self.auth_method.to_string()),
            ("keepalive", self.keepalive.to_string()),
            ("compression", self.compression.to_string()),
        ]);
        for (key, value) in [
            ("command", &self.command),
            ("user", &self.user),
            ("identity", &self.identity),
        ] {
            if !value.is_empty() {
                values.insert(key, value.to_string());
            }
        }
        let config = Config::from_values(values)?;
        let session_kind = match self.session_kind.as_str() {
            "shell" => SessionKind::Shell,
            "herdr" => SessionKind::Herdr,
            _ => return Err("Session type: choose SSH shell or Herdr".into()),
        };
        Ok(Profile {
            id: self.id.to_string(),
            name: self.name.to_string(),
            config,
            session_kind,
            herdr_session: self.herdr_session.to_string(),
            herdr_binary: self.herdr_binary.to_string(),
            remembered_herdr_sessions: Vec::new(),
        })
    }
}

impl App {
    pub(crate) fn connection_model(&self, ui: &AppWindow) {
        let state = |session: &Session| {
            if session.prompt.is_some() {
                "Sign-in required"
            } else if session.connected {
                "Connected"
            } else if session.connecting {
                "Connecting"
            } else if session.retry_at.is_some() {
                "Reconnecting"
            } else {
                "Disconnected"
            }
        };
        let local = self.local_session();
        let mut entries = Vec::with_capacity(self.connections.profiles().len() + 1);
        entries.push(ui::HostView {
            id: LOCAL_HOST.into(),
            name: "Local".into(),
            endpoint: "Herdr on this Kindle".into(),
            state: state(local).into(),
            detail: local.detail.clone().into(),
            selected: ui.get_host_detail_id() == LOCAL_HOST,
            open: true,
            local: true,
            needs_attention: !local.connected && !local.connecting,
        });
        entries.extend(self.connections.profiles().iter().map(|profile| {
            let session = self
                .endpoints
                .iter()
                .find(|session| session.profile.as_ref().is_some_and(|p| p.id == profile.id));
            let pane = self
                .local_panes
                .values()
                .find(|pane| pane.prepared.profile.id == profile.id);
            ui::HostView {
                id: profile.id.clone().into(),
                name: profile.name.clone().into(),
                endpoint: profile.config.description().into(),
                state: session
                    .map(state)
                    .or_else(|| pane.map(LocalPane::state))
                    .unwrap_or("Saved host")
                    .into(),
                detail: session
                    .map(|s| {
                        format!(
                            "Remote Herdr · {}\n{}",
                            s.profile
                                .as_ref()
                                .map_or(profile.herdr_session.as_str(), |p| p
                                    .herdr_session
                                    .as_str()),
                            s.detail
                        )
                    })
                    .or_else(|| pane.map(|p| format!("SSH pane owned by Local\n{}", p.detail)))
                    .unwrap_or_else(|| {
                        format!("Preferred Herdr session: {}", profile.herdr_session)
                    })
                    .into(),
                selected: ui.get_host_detail_id().as_str() == profile.id,
                open: session.is_some(),
                local: false,
                needs_attention: self.endpoints.iter().any(|s| {
                    s.profile.as_ref().is_some_and(|p| p.id == profile.id)
                        && (s.prompt.is_some() || (!s.connected && !s.connecting))
                }) || self.local_panes.values().any(|p| {
                    p.prepared.profile.id == profile.id && (p.prompt.is_some() || p.ended)
                }),
            }
        }));
        ui.set_host_detail(entries.iter().find(|entry| entry.id == ui.get_host_detail_id()).cloned().unwrap_or_default());
        let mut hosts = ui.get_host_entries();
        if update_navigation_entries(&mut hosts, entries) {
            ui.set_host_entries(hosts);
        }
        let focused = self.focused_local_pane().map(|pane| pane.id);
        let mut entries: Vec<_> = self
            .endpoints
            .iter()
            .map(|session| {
                let profile = session.profile.as_ref();
                ui::OpenSessionView {
                    id: session.id.to_string().into(),
                    host_id: profile.map_or(LOCAL_HOST, |p| p.id.as_str()).into(),
                    host_name: profile.map_or("This Kindle", |p| p.name.as_str()).into(),
                    name: profile.map_or("Local", |p| p.herdr_session.as_str()).into(),
                    kind: if profile.is_some() {
                        "Remote Herdr"
                    } else {
                        "Local Herdr"
                    }
                    .into(),
                    state: state(session).into(),
                    selected: session.id == self.endpoints.active.id
                        && (session.id != LOCAL_SESSION || focused.is_none()),
                }
            })
            .collect();
        entries.extend(self.local_panes.values().map(|pane| {
            ui::OpenSessionView {
                id: pane.id.to_string().into(),
                host_id: pane.prepared.profile.id.clone().into(),
                host_name: pane.prepared.profile.name.clone().into(),
                name: format!(
                    "SSH {}",
                    pane.prepared
                        .metadata
                        .as_ref()
                        .map(|m| m.pane_id.clone())
                        .unwrap_or_else(|| pane.id.to_string())
                )
                .into(),
                kind: "SSH in Local".into(),
                state: pane.state().into(),
                selected: focused == Some(pane.id),
            }
        }));
        let mut sessions = ui.get_open_sessions();
        if update_navigation_entries(&mut sessions, entries) {
            ui.set_open_sessions(sessions);
        }
        let host = ui.get_setup_host_id();
        ui.set_current_view_id(self.endpoints.active.id.to_string().into());
        let discovered = ui.get_setup_sessions();
        let mut names: std::collections::BTreeSet<String> = discovered.iter().map(|name| name.to_string()).collect();
        names.extend(self.endpoints.iter().filter_map(|session| session.profile.as_ref()
            .filter(|p| p.id == host.as_str()).map(|p| p.herdr_session.clone())));
        let rows = names.into_iter().map(|name| {
            let session = self.endpoints.iter().find(|session| session.profile.as_ref()
                .is_some_and(|p| p.id == host.as_str() && p.herdr_session == name));
            let available = ui.get_setup_inspected() && discovered.iter().any(|entry| entry.as_str() == name);
            ui::HostSessionView {
                name: name.into(), id: session.map_or_else(String::new, |s| s.id.to_string()).into(),
                state: session.map_or(if available { "Running" } else { "Not found in last refresh" }, state).into(),
                detail: session.filter(|s| !s.connected && !s.connecting).map_or("", |s| s.detail.as_str()).into(),
                checked: session.is_some(), available,
                can_retry: session.is_some_and(|s| !s.connected && !s.connecting && s.retry_at.is_none() && s.prompt.is_none()),
            }
        }).collect();
        let mut sessions = ui.get_host_sessions();
        if update_navigation_entries(&mut sessions, rows) { ui.set_host_sessions(sessions); }
        let current = self.endpoints.active.profile.as_ref();
        ui.set_current_connection_id(current.map_or(LOCAL_HOST, |p| p.id.as_str()).into());
        ui.set_current_connection_name(current.map_or("Local", |p| p.name.as_str()).into());
        ui.set_current_session_name(current.map_or("Local", |p| p.herdr_session.as_str()).into());
        ui.set_current_session_label(current.map_or_else(|| "This Kindle · Local".to_owned(), |p| format!("{} · {}", p.name, p.herdr_session)).into());
        ui.set_current_connection_detail(
            current
                .map(|p| format!("{} · Remote Herdr · {}", p.name, p.herdr_session))
                .or_else(|| {
                    focused
                        .and_then(|id| self.local_panes.get(&id))
                        .map(|pane| {
                            format!(
                                "{} · SSH pane owned by Local on this Kindle",
                                pane.prepared.profile.name
                            )
                        })
                })
                .unwrap_or_else(|| "Local Herdr · This Kindle".into())
                .into(),
        );
        if let Some(error) = &self.connections.error {
            ui.set_connection_error(error.clone().into());
        }
    }

    pub(crate) fn edit_connection(&mut self, ui: &AppWindow, id: &str) {
        self.cancel_setup(ui);
        let draft = if id.is_empty() {
            ui::ConnectionDraft::new_host()
        } else {
            match self.connections.profile(id) {
                Ok(profile) => ui::ConnectionDraft::from_profile(profile),
                Err(error) => {
                    ui.set_connection_error(error.into());
                    return;
                }
            }
        };
        if let Some(error) = &self.connections.error {
            ui.set_connection_error(error.clone().into());
            return;
        }
        for session in
            std::iter::once(&mut self.endpoints.active).chain(self.endpoints.parked.values_mut())
        {
            if !session.connected
                && session
                    .profile
                    .as_ref()
                    .is_some_and(|profile| profile.id == id)
            {
                session.auto_reconnect = false;
                session.stop();
                session.detail = "Paused while editing host".into();
            }
        }
        self.cancel_interaction(ui);
        ui.invoke_cancel_input();
        ui.set_host_detail_id(id.into());
        ui.set_connection_draft(draft);
        self.key_model(ui);
        ui.set_connection_error("".into());
        ui.invoke_navigate(ui::Route::Connection);
    }

    pub(crate) fn save_connection(&mut self, ui: &AppWindow, draft: &ui::ConnectionDraft) {
        match draft
            .profile()
            .and_then(|profile| self.connections.save(profile))
        {
            Ok(id) => {
                self.cancel_interaction(ui);
                ui.invoke_cancel_input();
                if let Ok(saved) = self.connections.profile(&id) {
                    for session in std::iter::once(&mut self.endpoints.active).chain(self.endpoints.parked.values_mut()) {
                        if let Some(profile) = &mut session.profile {
                            if profile.id == saved.id {
                                let name = std::mem::take(&mut profile.herdr_session);
                                *profile = saved.clone();
                                profile.herdr_session = name;
                            }
                        }
                    }
                }
                ui.set_host_detail_id(id.clone().into());
                ui.invoke_navigate(ui::Route::Hosts);
                self.inspect_host_sessions(ui, &id);
                ui.set_connection_error("".into());
                self.connection_model(ui);
            }
            Err(error) => ui.set_connection_error(error.into()),
        }
    }


    pub(crate) fn remove_connection(&mut self, ui: &AppWindow, id: &str) {
        if id == LOCAL_HOST {
            ui.set_connection_error("Local is the Kindle's runtime, not a saved host.".into());
            return;
        }
        if self.discovery_pending {
            ui.set_connection_error(
                "Wait for local SSH controls to finish reattaching before removing a host.".into(),
            );
            return;
        }
        if self.endpoints.iter().any(|session| {
            session
                .profile
                .as_ref()
                .is_some_and(|profile| profile.id == id)
        }) || self
            .local_panes
            .values()
            .any(|pane| pane.prepared.profile.id == id)
        {
            ui.set_connection_error("Hide this host’s Herdr sessions from this device and close its SSH panes before forgetting the saved host.".into());
            return;
        }
        match self.connections.remove(id) {
            Ok(()) => {
                ui.set_host_detail_id("".into());
                ui.set_connection_error("".into());
                self.connection_model(ui);
            }
            Err(error) => ui.set_connection_error(error.into()),
        }
    }
}
