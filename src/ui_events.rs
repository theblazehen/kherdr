//! Bounded ordered worker delivery, independent of endpoint lifecycle policy.
use crate::{
    client::{self, Pane, Tab, Workspace},
    shell,
    ui::AppWindow,
    with_app,
};
use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

const UI_EVENT_BATCH: usize = 32;
const UI_EVENT_LIMIT: usize = 128;
const UI_EVENT_BYTES: usize = 1024 * 1024;

pub(crate) enum Event {
    Herdr(client::Event),
    Shell(shell::Event),
    Discovered(shell::Prepared),
    DiscoveryFinished(Option<String>),
}

pub(crate) struct UiEventQueue {
    state: Mutex<UiEventState>,
    space: Condvar,
}
struct UiEventState {
    pending: VecDeque<(Event, usize)>,
    bytes: usize,
    drained: u64,
    scheduled: bool,
    closed: bool,
    failure: Option<String>,
}

fn herdr_event_bytes(event: &client::Event) -> usize {
    std::mem::size_of::<client::Event>()
        + match event {
            client::Event::Frame { bytes, .. } => bytes.capacity(),
            client::Event::Navigation {
                workspaces,
                tabs,
                panes,
                agents,
                focused_workspace,
            } => {
                workspaces.capacity() * std::mem::size_of::<Workspace>()
                    + workspaces
                        .iter()
                        .map(|w| {
                            w.workspace_id.capacity()
                                + w.label.capacity()
                                + w.status.capacity()
                                + w.worktree_key.capacity()
                                + w.worktree_label.capacity()
                        })
                        .sum::<usize>()
                    + focused_workspace.as_ref().map_or(0, String::capacity)
                    + agents.capacity() * std::mem::size_of::<client::Agent>()
                    + agents.iter().map(|a| a.pane_id.capacity() + a.workspace_id.capacity() + a.tab_id.capacity()
                        + a.name.capacity() + a.agent.capacity() + a.status.capacity()).sum::<usize>()
                    + panes.capacity() * std::mem::size_of::<Pane>()
                    + panes
                        .iter()
                        .map(|a| {
                            a.pane_id.capacity()
                                + a.name.capacity()
                                + a.agent.capacity()
                                + a.status.capacity()
                                + a.workspace_id.capacity()
                                + a.workspace.capacity()
                                + a.tab_id.capacity()
                                + a.detail.capacity()
                        })
                        .sum::<usize>()
                    + tabs.capacity() * std::mem::size_of::<Tab>()
                    + tabs
                        .iter()
                        .map(|t| {
                            t.tab_id.capacity()
                                + t.workspace_id.capacity()
                                + t.workspace.capacity()
                                + t.name.capacity()
                                + t.status.capacity()
                                + t.pane_id.capacity()
                        })
                        .sum::<usize>()
            }
            client::Event::Layout { panes, .. } => {
                panes.capacity() * std::mem::size_of::<client::SurfacePane>()
                    + panes
                        .iter()
                        .map(|pane| pane.pane_id.capacity())
                        .sum::<usize>()
            }
            client::Event::Focus(focus) => focus.pane_id.as_ref().map_or(0, String::capacity),
            client::Event::Unavailable(message)
            | client::Event::Disconnected(message)
            | client::Event::Error { message, .. } => message.capacity(),
            client::Event::Authentication(prompt) => {
                prompt.title.capacity()
                    + prompt.detail.capacity()
                    + prompt.fingerprint.capacity()
                    + prompt.previous_fingerprint.capacity()
            }
            client::Event::Ready | client::Event::Reset { .. } => 0,
        }
}

fn event_bytes(event: &Event) -> usize {
    match event {
        Event::Herdr(event) => herdr_event_bytes(event),
        // Each helper document is bounded to 128 KiB. This includes the retained
        // profile/metadata copies and decoded string capacities without encoding
        // the document again merely to account for a rare lifecycle event.
        Event::Discovered(_) | Event::Shell(shell::Event::Attached(_)) => 512 * 1024,
        Event::DiscoveryFinished(error) => error.as_ref().map_or(0, String::capacity) + 128,
        Event::Shell(shell::Event::Authentication(prompt)) => {
            prompt.title.capacity()
                + prompt.detail.capacity()
                + prompt.fingerprint.capacity()
                + prompt.previous_fingerprint.capacity()
                + 128
        }
        Event::Shell(shell::Event::Closed { message, .. } | shell::Event::ControlLost(message)) => {
            message.capacity() + 128
        }
        Event::Shell(shell::Event::Ready) => 128,
    }
}

impl UiEventQueue {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(UiEventState {
                pending: VecDeque::new(),
                bytes: 0,
                drained: 0,
                scheduled: false,
                closed: false,
                failure: None,
            }),
            space: Condvar::new(),
        })
    }

    pub(crate) fn cancel(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.closed = true;
        state.pending.clear();
        state.bytes = 0;
        self.space.notify_all();
    }

    pub(crate) fn send(
        self: &Arc<Self>,
        weak: &slint::Weak<AppWindow>,
        epoch: u64,
        event: Event,
    ) -> Result<(), String> {
        let bytes = event_bytes(&event);
        let mut state = self
            .state
            .lock()
            .map_err(|_| "UI event queue poisoned".to_string())?;
        if state.closed {
            return Ok(());
        }
        if bytes > UI_EVENT_BYTES {
            return Err("UI event exceeds bounded delivery capacity".into());
        }
        // One ordered producer. Large events can need several drains; only actual
        // consumption extends the deadline, not spurious wakes. Stop wakes before join.
        let mut progress = state.drained;
        let mut deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !state.closed
            && state.failure.is_none()
            && (state.pending.len() >= UI_EVENT_LIMIT || bytes > UI_EVENT_BYTES - state.bytes)
        {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                let error = "UI stopped consuming Herdr output for 10 seconds".to_string();
                state.failure = Some(error.clone());
                return Err(error);
            }
            let (next, _) = self
                .space
                .wait_timeout(state, remaining)
                .map_err(|_| "UI event queue poisoned".to_string())?;
            state = next;
            if state.drained != progress {
                progress = state.drained;
                deadline = std::time::Instant::now() + Duration::from_secs(10);
            }
        }
        if state.closed {
            return Ok(());
        }
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        state.pending.push_back((event, bytes));
        state.bytes += bytes;
        if !state.scheduled {
            state.scheduled = true;
            if let Err(error) = schedule_ui_events(weak, self, epoch) {
                state.failure = Some(error.clone());
                return Err(error);
            }
        }
        Ok(())
    }
}

fn schedule_ui_events(
    weak: &slint::Weak<AppWindow>,
    queue: &Arc<UiEventQueue>,
    token: u64,
) -> Result<(), String> {
    let queue = Arc::clone(queue);
    let next = weak.clone();
    weak.upgrade_in_event_loop(move |ui| {
        with_app(|app| {
            if !app.owns_events(token) {
                queue.cancel();
                return;
            }
            let result = (|| -> Result<(), String> {
                let mut batch: [Option<Event>; UI_EVENT_BATCH] = std::array::from_fn(|_| None);
                {
                    let mut state = queue
                        .state
                        .lock()
                        .map_err(|_| "UI event queue poisoned".to_string())?;
                    if let Some(error) = &state.failure {
                        return Err(error.clone());
                    }
                    let limit = state.pending.len().min(UI_EVENT_BATCH);
                    // Preserve an available commit as this callback's last
                    // terminal mutation. Otherwise a sustained queue can make
                    // every batch end mid-update and starve visible commits.
                    let count = state
                        .pending
                        .iter()
                        .take(limit)
                        .enumerate()
                        .filter_map(|(index, (event, _))| {
                            matches!(
                                event,
                                Event::Herdr(client::Event::Frame { complete: true, .. })
                            )
                            .then_some(index + 1)
                        })
                        .last()
                        .unwrap_or(limit);
                    for slot in batch.iter_mut().take(count) {
                        let (event, bytes) = state
                            .pending
                            .pop_front()
                            .ok_or("UI queue accounting failed")?;
                        *slot = Some(event);
                        state.bytes -= bytes;
                    }
                    if count != 0 {
                        state.drained = state.drained.wrapping_add(1);
                        queue.space.notify_one();
                    }
                }
                let mut changed = false;
                let mut state_changed = false;
                for event in batch.into_iter().flatten() {
                    if !app.owns_events(token) {
                        queue.cancel();
                        break;
                    }
                    state_changed |= !matches!(&event, Event::Herdr(client::Event::Frame { .. }));
                    if let Some(active_changed) = app.connection_event(&ui, token, event)? {
                        changed |= active_changed;
                    }
                }
                if state_changed {
                    app.sync_session(&ui);
                    app.connection_model(&ui);
                } else if app.endpoints.active.token == token {
                    app.update_ready(&ui);
                }
                if changed {
                    app.paint(&ui)?;
                }
                let mut state = queue
                    .state
                    .lock()
                    .map_err(|_| "UI event queue poisoned".to_string())?;
                if state.closed {
                    return Ok(());
                }
                if let Some(error) = &state.failure {
                    return Err(error.clone());
                }
                if state.pending.is_empty() {
                    state.scheduled = false;
                } else {
                    schedule_ui_events(&next, &queue, token)?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                queue.cancel();
                app.event_delivery_failed(&ui, token, error);
            }
        });
    })
    .map_err(|error| format!("Cannot wake UI: {error}"))
}
