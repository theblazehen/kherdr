// SPDX-License-Identifier: GPL-3.0-or-later
use crate::{App, Event, LocalPane, Session, UiEventQueue, LOCAL_SESSION, client, connection::Profile, shell, status, ui};
use slint::ComponentHandle;
use std::{sync::Arc, thread};

impl LocalPane {
    pub(crate) fn state(&self) -> &'static str {
        if self.closed_confirmed { "Closed" }
        else if self.ended && self.closing { "Close not confirmed" }
        else if self.ended { "Control unavailable" }
        else if self.closing { "Closing" }
        else if self.prompt.is_some() { "Sign-in required" }
        else if self.client.as_ref().is_some_and(shell::Client::ready) { "SSH connected" }
        else if self.prepared.metadata.is_none() { "Opening local pane" }
        else { "Connecting SSH" }
    }
}

impl App {
    pub(crate) fn local_session(&self) -> &Session {
        if self.endpoints.active.id == LOCAL_SESSION { &self.endpoints.active }
        else { self.endpoints.parked.get(&LOCAL_SESSION).expect("Local endpoint is always retained") }
    }

    pub(crate) fn focused_local_pane(&self) -> Option<&LocalPane> {
        if self.endpoints.active.id != LOCAL_SESSION { return None; }
        let focus = self.endpoints.active.focus.as_ref()?;
        self.local_panes.values().find(|pane| !pane.closed_confirmed && pane.prepared.metadata.as_ref().is_some_and(|metadata|
            focus.pane_id.as_deref() == Some(metadata.pane_id.as_str())))
    }

    pub(crate) fn local_input_ready(&self) -> bool {
        if self.endpoints.active.id != LOCAL_SESSION { return true; }
        if self.discovery_pending || self.pending_local_focus.is_some()
            || self.local_panes.values().any(|pane| !pane.ended && pane.prepared.metadata.is_none()) { return false; }
        self.focused_local_pane().is_none_or(|pane| !pane.closing && !pane.ended && pane.prompt.is_none()
            && pane.client.as_ref().is_some_and(shell::Client::ready))
    }

    pub(crate) fn ensure_local(&mut self, window: &ui::AppWindow) -> Result<(), String> {
        if self.local_session().client.is_some() { return Ok(()); }
        let token = self.identities.allocate()?;
        let selected = self.endpoints.active.id == LOCAL_SESSION;
        let session = if selected { &mut self.endpoints.active }
            else { self.endpoints.parked.get_mut(&LOCAL_SESSION).ok_or("Local endpoint is missing")? };
        Self::start_session(session, token, self.auth_root.clone(), &self.local_runtime, window, selected, crate::session::StartPolicy::Explicit)?;
        Ok(())
    }

    pub(crate) fn discover_local_panes(&mut self, window: &ui::AppWindow) -> Result<(), String> {
        self.discovery_pending = true;
        let root = self.local_runtime.panes_root.clone();
        let queue = UiEventQueue::new();
        let weak = window.as_weak();
        thread::Builder::new().name("local-ssh-discovery".into()).spawn(move || {
            let result = shell::discover(&root).and_then(|panes| {
                for prepared in panes { queue.send(&weak, 0, Event::Discovered(prepared))?; }
                Ok(())
            });
            let _ = queue.send(&weak, 0, Event::DiscoveryFinished(result.err()));
        }).map_err(|error| { self.discovery_pending = false; format!("Cannot discover local SSH panes: {error}") })?;
        Ok(())
    }

    fn register_local_pane(&mut self, prepared: shell::Prepared, launched: bool, window: &ui::AppWindow) -> Result<u64, String> {
        if let Some(pane) = self.local_panes.values().find(|pane| pane.prepared.launch_id == prepared.launch_id) { return Ok(pane.id); }
        let id = self.identities.allocate()?;
        let token = self.identities.allocate()?;
        let queue = UiEventQueue::new();
        let cancel = Arc::clone(&queue);
        let weak = window.as_weak();
        let client = shell::Client::attach(&prepared, move |event| queue.send(&weak, token, Event::Shell(event)), move || cancel.cancel())?;
        self.local_panes.insert(id, LocalPane { id, token, prepared, client: Some(client), prompt: None, launched,
            closing: false, ended: false, closed_confirmed: false, detail: if launched { "Reattaching SSH in Local" } else { "Opening SSH in Local" }.into() });
        Ok(id)
    }

    pub(crate) fn open_ssh_pane(&mut self, window: &ui::AppWindow, profile: Profile, foreground: bool, reason: Option<String>) -> Result<(), String> {
        self.endpoints.admit(self.local_panes.len())?;
        self.ensure_local(window)?;
        let prepared = shell::prepare(&self.local_runtime.panes_root, profile, self.auth_root.clone(), self.local_runtime.api_socket.clone())?;
        let id = self.register_local_pane(prepared, false, window)?;
        if let Some(reason) = reason { self.local_panes.get_mut(&id).ok_or("Missing new SSH pane")?.detail = format!("{reason} Opening SSH in Local; a separate sign-in may be needed."); }
        if foreground {
            self.cancel_interaction(window);
            self.pending_local_focus = Some(id);
            self.resume_session(window, LOCAL_SESSION);
        }
        self.launch_pending_local(window);
        self.focus_pending_local(window);
        self.sync_session(window); self.connection_model(window);
        Ok(())
    }

    pub(crate) fn launch_pending_local(&mut self, window: &ui::AppWindow) {
        if !self.local_session().connected { return; }
        let ids: Vec<_> = self.local_panes.values().filter(|pane| !pane.launched && !pane.ended && !pane.closing).map(|pane| pane.id).collect();
        for id in ids {
            let focus = self.endpoints.active.id == LOCAL_SESSION && self.pending_local_focus == Some(id);
            let Some(pane) = self.local_panes.get_mut(&id) else { continue; };
            // An API timeout can have an ambiguous outcome. Never repeat a PTY
            // creation: the helper's immutable Attached notice resolves success.
            pane.launched = true;
            if let Err(error) = self.local_runtime.launch_ssh(&pane.prepared, focus) {
                pane.detail = format!("Local SSH pane creation was not confirmed: {error}");
                window.set_connection_error(pane.detail.clone().into());
            }
        }
    }

    fn reattach_local_pane(&mut self, id: u64, window: &ui::AppWindow) -> Result<(), String> {
        let token = self.identities.allocate()?;
        let pane = self.local_panes.get_mut(&id).ok_or("SSH pane is no longer open")?;
        if let Some(mut client) = pane.client.take() { let _ = client.stop(); }
        pane.token = token; pane.prompt = None; pane.ended = false; pane.closed_confirmed = false;
        let queue = UiEventQueue::new();
        let cancel = Arc::clone(&queue);
        let weak = window.as_weak();
        match shell::Client::attach(&pane.prepared, move |event| queue.send(&weak, token, Event::Shell(event)), move || cancel.cancel()) {
            Ok(client) => { pane.client = Some(client); pane.detail = "Reattaching existing SSH control; no new SSH connection".into(); Ok(()) }
            Err(error) => { pane.ended = true; pane.detail = error.clone(); Err(error) }
        }
    }

    pub(crate) fn resume_local_pane(&mut self, window: &ui::AppWindow, id: u64) {
        if !self.local_panes.contains_key(&id) { return; }
        self.cancel_interaction(window);
        if self.local_panes.get(&id).is_some_and(|pane| pane.ended && !pane.closed_confirmed) {
            if let Err(error) = self.reattach_local_pane(id, window) { window.set_connection_error(error.into()); }
        }
        self.pending_local_focus = Some(id);
        self.resume_session(window, LOCAL_SESSION);
        self.focus_pending_local(window);
        self.sync_session(window); self.connection_model(window);
    }

    pub(crate) fn focus_pending_local(&mut self, window: &ui::AppWindow) {
        let Some(id) = self.pending_local_focus else { return; };
        if self.endpoints.active.id != LOCAL_SESSION { return; }
        let Some(pane) = self.local_panes.get(&id) else { self.pending_local_focus = None; return; };
        if pane.closed_confirmed { self.pending_local_focus = None; status(window, "This SSH session ended; Connect opens a new local pane."); return; }
        if pane.ended && pane.prepared.metadata.is_none() { self.pending_local_focus = None; return; }
        let Some(metadata) = pane.prepared.metadata.as_ref() else { return; };
        if !self.endpoints.active.connected { return; }
        if !self.endpoints.active.panes.iter().any(|entry| entry.pane_id == metadata.pane_id) { return; }
        if self.endpoints.active.focus.as_ref().is_some_and(|focus| focus.pane_id.as_deref() == Some(metadata.pane_id.as_str())) && self.endpoints.active.ready() {
            self.pending_local_focus = None;
        } else if !self.endpoints.active.selection_pending {
            let pane_id = metadata.pane_id.clone();
            self.pending_local_focus = None;
            self.choose(window, &pane_id);
        }
    }

    pub(crate) fn close_local_pane(&mut self, window: &ui::AppWindow, id: u64) {
        let Some(pane) = self.local_panes.get(&id) else { return; };
        let remove = !pane.launched || pane.closed_confirmed;
        let reconnect = pane.ended || pane.client.is_none();
        self.cancel_interaction(window);
        if remove {
            self.local_panes.remove(&id);
            if self.pending_local_focus == Some(id) { self.pending_local_focus = None; }
        } else {
            if let Some(pane) = self.local_panes.get_mut(&id) { pane.closing = true; }
            if reconnect {
                if let Err(error) = self.reattach_local_pane(id, window) { window.set_connection_error(error.into()); }
            } else if let Some(pane) = self.local_panes.get_mut(&id) {
                if pane.prepared.metadata.is_some() {
                    if let Err(error) = pane.client.as_ref().ok_or_else(|| "SSH controller is unavailable".to_string()).and_then(shell::Client::close) {
                        pane.detail = error.clone(); window.set_connection_error(error.into());
                    }
                }
            }
        }
        self.sync_session(window); self.connection_model(window);
    }

    pub(crate) fn owns_events(&self, token: u64) -> bool {
        (token == 0 && self.discovery_pending) || (token != 0 && (self.endpoints.iter().any(|session| session.token == token)
            || self.local_panes.values().any(|pane| pane.token == token)))
    }

    pub(crate) fn connection_event(&mut self, window: &ui::AppWindow, token: u64, event: Event) -> Result<Option<bool>, String> {
        match event {
            Event::Discovered(prepared) if token == 0 && self.discovery_pending => {
                if let Err(error) = self.register_local_pane(prepared, true, window) { window.set_connection_error(error.into()); }
                Ok(Some(false))
            }
            Event::DiscoveryFinished(error) if token == 0 && self.discovery_pending => {
                self.discovery_pending = false;
                if let Some(error) = error { window.set_connection_error(format!("Local SSH discovery: {error}").into()); }
                Ok(Some(false))
            }
            Event::Herdr(client::Event::Unavailable(reason)) if self.endpoints.iter().any(|session| session.token == token && session.allow_fallback && session.profile.is_some()) => {
                let (id, profile) = {
                    let session = self.endpoints.by_token(token).ok_or("Connection is no longer current")?;
                    session.allow_fallback = false; session.auto_reconnect = false; session.stop();
                    (session.id, session.profile.clone().ok_or("Missing remote profile")?)
                };
                let foreground = self.endpoints.active.id == id;
                if foreground { self.resume_session(window, LOCAL_SESSION); }
                self.endpoints.parked.remove(&id);
                if let Err(error) = self.open_ssh_pane(window, profile, foreground, Some(reason.clone())) {
                    window.set_connection_error(format!("{reason} Cannot open SSH in Local: {error}").into());
                    window.invoke_navigate(ui::Route::Hosts);
                }
                Ok(Some(false))
            }
            Event::Herdr(event) => {
                let active = self.endpoints.active.token == token;
                if active && matches!(&event, client::Event::Focus(_) | client::Event::Reset { .. } | client::Event::Authentication(_)) { self.cancel_interaction(window); }
                let starts = matches!(&event, client::Event::Ready);
                if let client::Event::Frame { bytes, .. } = &event { self.bytes += bytes.len() as u64; }
                let Some(session) = self.endpoints.by_token(token) else { return Ok(None); };
                let local = session.id == LOCAL_SESSION;
                let changed = session.event(event)?;
                if local {
                    if starts { self.launch_pending_local(window); }
                    if self.pending_local_focus.is_some() { self.focus_pending_local(window); }
                }
                Ok(Some(active && changed))
            }
            Event::Shell(event) => {
                let Some(id) = self.local_panes.values().find(|pane| pane.token == token).map(|pane| pane.id) else { return Ok(None); };
                if self.focused_local_pane().is_some_and(|pane| pane.id == id) { self.cancel_interaction(window); }
                let pane = self.local_panes.get_mut(&id).ok_or("SSH pane disappeared")?;
                let mut remove = false;
                match event {
                    shell::Event::Attached(metadata) => {
                        if metadata.launch_id != pane.prepared.launch_id { return Err("SSH launch identity changed".into()); }
                        pane.prepared.metadata = Some(metadata);
                        if pane.closing { pane.client.as_ref().ok_or("SSH controller disappeared")?.close()?; }
                    }
                    shell::Event::Authentication(prompt) => { pane.prompt = Some(prompt); pane.detail = "SSH in Local requires sign-in".into(); }
                    shell::Event::Ready => { pane.prompt = None; pane.ended = false; pane.detail = "SSH in Local — detaching the UI keeps this pane running".into(); }
                    shell::Event::Closed { message, .. } => {
                        pane.prompt = None; pane.ended = true; pane.detail = message;
                        pane.closed_confirmed = true;
                        remove = pane.closing;
                    }
                    shell::Event::ControlLost(message) => {
                        pane.prompt = None; pane.ended = true; pane.closed_confirmed = false;
                        pane.detail = format!("SSH control unavailable; connection closure is not confirmed: {message}");
                    }
                }
                if remove { self.local_panes.remove(&id); if self.pending_local_focus == Some(id) { self.pending_local_focus = None; } }
                self.focus_pending_local(window);
                Ok(Some(false))
            }
            _ => Ok(None),
        }
    }

    pub(crate) fn event_delivery_failed(&mut self, window: &ui::AppWindow, token: u64, error: String) {
        if token == 0 {
            self.discovery_pending = false;
            window.set_connection_error(format!("Local SSH discovery stopped: {error}").into());
        } else if let Some(session) = self.endpoints.by_token(token) { session.fail(error, true); }
        else if let Some(pane) = self.local_panes.values_mut().find(|pane| pane.token == token) {
            pane.ended = true; pane.prompt = None; pane.detail = error;
            if let Some(mut client) = pane.client.take() { let _ = client.stop(); }
        }
        self.cancel_interaction(window);
        self.sync_session(window); self.connection_model(window);
    }
}
