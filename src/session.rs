// SPDX-License-Identifier: GPL-3.0-or-later
use crate::{auth, client, connection::Profile, terminal::Terminal};
use std::time::{Duration, Instant};

pub(crate) const MAX_OPEN_SESSIONS: usize = 16;

/// Shared by endpoints, local panes and worker incarnations; identities never
/// repeat during this process even after a view closes.
pub(crate) struct Identities(u64);
impl Identities {
    pub(crate) fn new(first: u64) -> Self { Self(first) }
    pub(crate) fn allocate(&mut self) -> Result<u64, String> {
        self.0 = self.0.checked_add(1).ok_or("Native connection identities exhausted")?;
        Ok(self.0)
    }
}

/// Owns the selected view and retained background endpoints, never their remote
/// servers. Local SSH processes remain independently owned by Local Herdr.
pub(crate) struct Endpoints {
    pub(crate) active: Session,
    pub(crate) parked: std::collections::BTreeMap<u64, Session>,
}
impl Endpoints {
    pub(crate) fn new(local: Session) -> Self { Self { active: local, parked: Default::default() } }
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Session> {
        std::iter::once(&self.active).chain(self.parked.values())
    }
    pub(crate) fn by_token(&mut self, token: u64) -> Option<&mut Session> {
        if token == 0 { return None; }
        if self.active.token == token { Some(&mut self.active) }
        else { self.parked.values_mut().find(|session| session.token == token) }
    }
    pub(crate) fn available(&self, local_panes: usize) -> usize {
        MAX_OPEN_SESSIONS.saturating_sub(1 + self.parked.len() + local_panes)
    }
    pub(crate) fn admit(&self, local_panes: usize) -> Result<(), String> {
        if self.available(local_panes) == 0 {
            Err(format!("Close an open session before opening another (maximum {MAX_OPEN_SESSIONS})."))
        } else { Ok(()) }
    }
    pub(crate) fn select(&mut self, id: u64) -> bool {
        let Some(session) = self.parked.remove(&id) else { return false; };
        if let Some(client) = &self.active.client {
            if let Err(error) = client.set_active(false) { self.active.fail(error, true); }
        }
        let previous = std::mem::replace(&mut self.active, session);
        self.parked.insert(previous.id, previous);
        if let Some(client) = &self.active.client {
            if let Err(error) = client.set_active(true) { self.active.fail(error, true); }
        }
        true
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartPolicy { Automatic, Explicit, Reconnect }

/// A native view of one independent Herdr endpoint. No profile means the local
/// server; SSH pane processes belong to that server, not to this view's lifetime.
pub struct Session {
    pub id: u64,
    pub token: u64,
    pub profile: Option<Profile>,
    pub terminal: Terminal,
    pub client: Option<client::Client>,
    pub workspaces: Vec<client::Workspace>,
    pub tabs: Vec<client::Tab>,
    pub panes: Vec<client::Pane>,
    pub agents: Vec<client::Agent>,
    pub navigation_ready: bool,
    pub focused_workspace: Option<String>,
    pub surface_panes:Vec<client::SurfacePane>, pub popup_size:Option<[u16;2]>,
    pub focus: Option<client::Focus>,
    pub prompt: Option<auth::Prompt>,
    pub connected: bool,
    pub stream_ready: bool,
    pub stream_generation: Option<u64>,
    // Ghostty consumes bounded chunks immediately, but its dirty rows must not
    // be read/acknowledged until the native presentation transaction commits.
    presentation_complete: bool,
    pub selection_pending: bool,
    /// Only an explicit initial connection may choose SSH when remote Herdr is
    /// unavailable. An established endpoint's reconnect never changes ownership.
    pub allow_fallback: bool,
    /// Only automatic host connections may fall back; an explicitly selected
    /// named Herdr session must never turn into an unrelated SSH pane.
    pub automatic_connection: bool,
    pub ever_attached: bool,
    pub auto_reconnect: bool,
    pub retry_delay: u64,
    pub retry_at: Option<Instant>,
    pub connecting: bool,
    pub detail: String,
    pub cols: u16,
    pub rows: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

impl Session {
    pub(crate) fn prepare_start(&mut self, token: u64, policy: StartPolicy) -> Result<(), String> {
        let fresh = self.token == 0;
        self.stop(); self.token = token;
        if policy != StartPolicy::Reconnect { self.auto_reconnect = true; self.retry_delay = 1; }
        self.allow_fallback = policy == StartPolicy::Automatic && !self.ever_attached;
        if !fresh { self.reset_terminal()?; }
        self.workspaces.clear(); self.tabs.clear(); self.panes.clear();
        self.agents.clear();
        self.focused_workspace = None; self.surface_panes.clear(); self.popup_size = None; self.focus = None;
        self.connecting = true; self.detail = "Connecting".into();
        Ok(())
    }

    pub fn new(id: u64, profile: Option<Profile>, cols: u16, rows: u16,
        cell_width: u16, cell_height: u16) -> Result<Self, String> {
        let mut terminal = Terminal::new(cols, rows)?;
        terminal.set_cell_size(cell_width.into(), cell_height.into())?;
        Ok(Self { id, token: 0, profile, terminal, client: None,
            workspaces: Vec::new(), tabs: Vec::new(), panes: Vec::new(), focused_workspace: None, surface_panes:Vec::new(), popup_size:None, focus: None, prompt: None,
            agents: Vec::new(), navigation_ready: false,
            connected: false, stream_ready: false, stream_generation: None,
            presentation_complete: true,
            selection_pending: false, allow_fallback: false, automatic_connection: false, ever_attached: false, auto_reconnect: false,
            retry_delay: 1, retry_at: None, connecting: false, detail: "Not connected".into(),
            cols, rows, cell_width, cell_height })
    }

    pub fn ready(&self) -> bool {
        if !self.connected || !self.stream_ready || self.selection_pending || self.prompt.is_some() { return false; }
        self.focus.as_ref().is_some_and(|focus| self.stream_generation == Some(focus.generation)
            && self.client.as_ref().is_some_and(|client| client.input_ready(focus)))
    }

    pub fn snapshot(&mut self) -> Result<Option<crate::terminal::Snapshot>, String> {
        if !self.presentation_complete { return Ok(None); }
        self.terminal.snapshot().map(Some)
    }

    pub fn reset_terminal(&mut self) -> Result<(), String> {
        self.presentation_complete = false;
        self.terminal.reset()?;
        self.responses()
    }

    pub fn stop(&mut self) {
        self.prompt = None;
        self.connected = false;
        self.connecting = false;
        self.stream_ready = false;
        self.navigation_ready = false;
        self.stream_generation = None;
        self.selection_pending = false;
        self.retry_at = None;
        if let Some(mut client) = self.client.take() {
            if let Err(error) = client.stop() { eprintln!("Herdr disconnect: {error}"); }
        }
    }

    pub fn fail(&mut self, message: String, fatal: bool) {
        self.stop();
        self.detail = message;
        if fatal { self.auto_reconnect = false; }
        if self.auto_reconnect {
            self.retry_at = Some(Instant::now() + Duration::from_secs(self.retry_delay));
            self.retry_delay = (self.retry_delay * 2).min(30);
        }
    }

    pub fn answer(&mut self, answer: auth::Answer) -> Result<(), String> {
        if self.prompt.as_ref().map(|prompt| prompt.id) != Some(answer.id) {
            return Err("Sign-in belongs to a previous connection".into());
        }
        let result = self.client.as_ref().ok_or("Connection is no longer active".to_string())?
            .answer_auth(answer);
        self.prompt = None;
        result
    }

    pub fn responses(&mut self) -> Result<(), String> {
        if self.terminal.take_responses()?.is_empty() { Ok(()) }
        else { Err("Unexpected response from a Herdr projected surface".into()) }
    }

    /// Runs for selected and background endpoints without touching UI state.
    pub fn event(&mut self, event: client::Event) -> Result<bool, String> {
        match event {
            client::Event::Authentication(prompt) => {
                self.prompt = Some(prompt); self.detail = "Sign-in required".into();
            }
            client::Event::Ready => {
                self.prompt = None;
                self.allow_fallback = false;
                self.ever_attached = true;
                self.client.as_ref().ok_or("Missing Herdr connection")?
                    .open(self.cols, self.rows, self.cell_width, self.cell_height)?;
                self.connected = true; self.connecting = false; self.detail = "Connected".into();
            }
            client::Event::Navigation { workspaces, tabs, panes, focused_workspace, agents } => {
                self.workspaces = workspaces; self.tabs = tabs; self.panes = panes; self.focused_workspace = focused_workspace;
                self.agents = agents; self.navigation_ready = true;
            }
            client::Event::Layout{panes,popup}=>{self.surface_panes=panes;self.popup_size=popup;}
            client::Event::Focus(focus) => {
                if self.stream_generation != Some(focus.generation) {
                    self.presentation_complete = false; self.stream_ready = false;
                }
                if self.client.as_ref().is_some_and(|client| client.confirms_focus(&focus)) {
                    self.focus = Some(focus); self.selection_pending = false;
                }
            }
            client::Event::Reset { generation } => {
                self.stream_ready = false; self.stream_generation = Some(generation);
                self.reset_terminal()?; return Ok(false);
            }
            client::Event::Frame { generation, bytes, complete } => {
                self.presentation_complete = false;
                if self.stream_generation != Some(generation) {
                    self.stream_ready = false;
                    return Err("Herdr frame does not match session stream generation".into());
                }
                if let Err(error) = self.terminal.feed(&bytes).and_then(|_| self.responses()) {
                    self.stream_ready = false; self.stream_generation = None;
                    return Err(error);
                }
                self.presentation_complete = complete;
                // Keep established same-pane input available during streaming;
                // a reset/new attachment becomes ready only at the commit.
                if complete { self.stream_ready = true; self.retry_delay = 1; }
                return Ok(complete);
            }
            client::Event::Unavailable(message) | client::Event::Disconnected(message) => self.fail(message, false),
            client::Event::Error { message, fatal } => {
                if fatal { self.fail(message, true); } else { self.detail = message; }
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(session: &mut Session, generation: u64, bytes: &[u8], complete: bool) {
        session.event(client::Event::Frame { generation, bytes: bytes.to_vec(), complete }).unwrap();
    }

    #[test]
    fn presentation_commit_hides_partial_rows_across_batches_and_resize() {
        let mut session = Session::new(1, None, 12, 3, 8, 16).unwrap();
        session.event(client::Event::Reset { generation: 1 }).unwrap();
        frame(&mut session, 1, b"\x1b[Hprevious", false);
        assert!(session.snapshot().unwrap().is_none());
        assert!(!session.stream_ready);
        frame(&mut session, 1, b"", true);
        let old = session.snapshot().unwrap().unwrap();
        assert!(old.changed_rows[0].cells.iter().map(|cell| cell.text.as_str()).collect::<String>().starts_with("previous"));

        // An earlier commit in the same UI batch must not authorize painting a
        // later update, even when a separate resize/invalidation requests paint.
        frame(&mut session, 1, b"\x1b[2J\x1b[Hnext", false);
        assert!(session.stream_ready);
        session.terminal.resize(13, 3).unwrap();
        session.terminal.invalidate();
        assert!(session.snapshot().unwrap().is_none());
        frame(&mut session, 1, b"\x1b[2;1Hsecond row", false);
        assert!(session.snapshot().unwrap().is_none());
        frame(&mut session, 1, b"", true);
        let committed = session.snapshot().unwrap().unwrap();
        let text = |index| committed.changed_rows.iter().find(|row| row.index == index).unwrap()
            .cells.iter().map(|cell| cell.text.as_str()).collect::<String>();
        assert!(text(0).starts_with("next"));
        assert!(text(1).starts_with("second row"));
    }

    #[test]
    fn reset_and_disconnect_never_publish_an_abandoned_update() {
        let mut session = Session::new(1, None, 12, 3, 8, 16).unwrap();
        session.event(client::Event::Reset { generation: 1 }).unwrap();
        frame(&mut session, 1, b"abandoned", false);
        session.event(client::Event::Reset { generation: 2 }).unwrap();
        assert!(session.snapshot().unwrap().is_none());
        assert!(session.event(client::Event::Frame { generation: 1, bytes: Vec::new(), complete: true }).is_err());
        assert!(session.snapshot().unwrap().is_none());
        frame(&mut session, 2, b"replacement", false);
        session.fail("Disconnected mid-update".into(), true);
        session.terminal.invalidate();
        assert!(session.snapshot().unwrap().is_none());
        assert!(!session.stream_ready);

        session.event(client::Event::Reset { generation: 3 }).unwrap();
        frame(&mut session, 3, b"retained", false);
        frame(&mut session, 3, b"", true);
        session.stop();
        session.terminal.invalidate();
        let retained = session.snapshot().unwrap().unwrap();
        assert!(retained.changed_rows[0].cells.iter().map(|cell| cell.text.as_str()).collect::<String>().starts_with("retained"));
        assert!(!session.stream_ready);
        session.reset_terminal().unwrap();
        assert!(session.snapshot().unwrap().is_none());
    }

    #[test]
    fn reconnect_policy_preserves_backoff_and_never_reenables_fallback() {
        let mut session = Session::new(1, None, 12, 3, 8, 16).unwrap();
        session.prepare_start(2, StartPolicy::Automatic).unwrap();
        assert!(session.allow_fallback);
        assert!(session.auto_reconnect);

        session.ever_attached = true;
        session.retry_delay = 8;
        session.prepare_start(3, StartPolicy::Reconnect).unwrap();
        assert!(!session.allow_fallback);
        assert_eq!(session.retry_delay, 8);

        session.prepare_start(4, StartPolicy::Explicit).unwrap();
        assert!(!session.allow_fallback);
        assert_eq!(session.retry_delay, 1);

        let mut identities = Identities(u64::MAX - 1);
        assert_eq!(identities.allocate().unwrap(), u64::MAX);
        assert!(identities.allocate().is_err());
    }
}
