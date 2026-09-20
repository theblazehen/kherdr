// SPDX-License-Identifier: GPL-3.0-or-later
use crate::{connection::Config, endpoint};
use std::{collections::VecDeque, panic::{catch_unwind, AssertUnwindSafe}, path::PathBuf, sync::{Arc, Mutex, MutexGuard}, thread::{self, JoinHandle}, time::{Duration, Instant}};
use tokio::sync::{mpsc, Notify};
const INPUT_LIMIT: usize = 512 * 1024;
const QUEUE_LIMIT: usize = 1024 * 1024;
const MAX_GENERATION: u64 = u64::MAX - 1;

/// Invoke only the stock Herdr stdio relay; no custom remote code is uploaded.
pub fn remote_command(herdr_binary: &str, session: &str) -> Result<String, String> {
    if herdr_binary.is_empty() || session.is_empty() || herdr_binary.contains('\0') || session.contains('\0') {
        return Err("Herdr executable and session must be nonempty and contain no NUL".into());
    }
    fn quote(value: &str) -> String { format!("'{}'", value.replace('\'', "'\\''")) }
    let binary = quote(herdr_binary); let session = quote(session);
    if herdr_binary == "herdr" {
        Ok(format!("if command -v herdr >/dev/null 2>&1; then exec herdr --session {session} remote-client-bridge; else exec \"$HOME/.local/bin/herdr\" --session {session} remote-client-bridge; fi"))
    } else { Ok(format!("exec {binary} --session {session} remote-client-bridge")) }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Focus { pub generation: u64, pub pane_id: Option<String> }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workspace {
    pub workspace_id: String, pub label: String, pub status: String,
    pub focused: bool, pub custom_label: bool,
    pub worktree_key: String, pub worktree_label:String, pub worktree_linked: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pane {
    pub pane_id: String, pub name: String, pub agent: String,
    pub status: String, pub workspace_id: String, pub workspace: String, pub tab_id: String, pub detail: String,
    pub custom_label: bool, pub right_click_passthrough: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    pub tab_id: String, pub workspace_id: String, pub workspace: String, pub name: String,
    pub status: String, pub pane_id: String, pub focused: bool, pub custom_label: bool, pub zoomed: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    pub pane_id: String, pub workspace_id: String, pub tab_id: String,
    pub name: String, pub agent: String, pub status: String,
}
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct SurfacePane { pub pane_id:String, pub rect:[u16;4], pub inner:[u16;4] }
#[derive(Debug)]
pub enum Event {
    Authentication(crate::auth::Prompt),
    Unavailable(String),
    Ready,
    Navigation { workspaces: Vec<Workspace>, tabs: Vec<Tab>, panes: Vec<Pane>, focused_workspace: Option<String>, agents: Vec<Agent> },
    Layout { panes:Vec<SurfacePane>, popup:Option<[u16;2]> },
    Focus(Focus),
    Reset { generation: u64 },
    Frame { generation: u64, bytes: Vec<u8>, complete: bool },
    Disconnected(String),
    Error { message: String, fatal: bool },
}


#[derive(Debug)]
pub(crate) enum Input { Key(String), Text(String), Paste(String), Scroll { up: bool, lines: u16 }, Click { pane:String, column:u16, row:u16, right:bool } }
#[derive(Debug)]
pub(crate) enum Navigation {
    Focus { pane_id:String }, FocusWorkspace(String), FocusTab(String),
    ClickFocus { focus:Focus, pane_id:String, column:u16, row:u16, right:bool },
    NewTab(Option<String>), NewWorkspace,
    RenameWorkspace { id:String, label:String }, CloseWorkspace(String),
    NewWorktree { id:String, branch:String }, OpenWorktree { id:String, path:String }, RemoveWorktree(String),
    RenameTab { id:String, label:String }, CloseTab(String),
    RenamePane { id:String, label:Option<String> }, SplitPane { id:String, workspace:String, right:bool },
    SwapPanes { source:String, target:String }, ToggleRightClick { id:String, to_pane:bool }, ZoomPane(String), ClosePane(String), ReloadConfig,
}
#[derive(Debug)]
pub(crate) enum Command {
    Start { dimensions:[u16;4], active:bool }, Resize([u16;4]), Active(bool),
    Navigate { id:u64, action:Navigation }, Input { focus:Focus, event:Input },
}
#[derive(Debug)]
pub(crate) enum EndpointEvent { Ui(Event), Ack(u64), NavigationFailed { id:u64, message:String } }
impl Command {
    fn size(&self) -> usize {
        std::mem::size_of::<Self>() + match self {
            Self::Navigate { action, .. } => match action {
                Navigation::Focus { pane_id } | Navigation::FocusWorkspace(pane_id) | Navigation::FocusTab(pane_id)
                    | Navigation::CloseWorkspace(pane_id) | Navigation::CloseTab(pane_id) | Navigation::ZoomPane(pane_id)
                    | Navigation::ClosePane(pane_id) | Navigation::RemoveWorktree(pane_id) => pane_id.len(),
                Navigation::ClickFocus { focus,pane_id,.. }=>focus.pane_id.as_ref().map_or(0,String::len)+pane_id.len(),
                Navigation::NewTab(id)=>id.as_ref().map_or(0,String::len),
                Navigation::RenameWorkspace{id,label}|Navigation::RenameTab{id,label}=>id.len()+label.len(),
                Navigation::NewWorktree{id,branch}=>id.len()+branch.len(),
                Navigation::OpenWorktree{id,path}=>id.len()+path.len(),
                Navigation::RenamePane{id,label}=>id.len()+label.as_ref().map_or(0,String::len),
                Navigation::SplitPane{id,workspace,..}=>id.len()+workspace.len(),
                Navigation::SwapPanes{source,target}=>source.len()+target.len(),
                Navigation::ToggleRightClick{id,..}=>id.len(),
                Navigation::NewWorkspace|Navigation::ReloadConfig=>0,
            },
            Self::Input { focus,event } => focus.pane_id.as_ref().map_or(0,String::len)+match event { Input::Key(s)|Input::Text(s)|Input::Paste(s)=>s.len(), Input::Click{pane,..}=>pane.len(), Input::Scroll {..}=>0 },
            _=>0,
        }
    }
}
struct PendingFocus { id:u64, target:Option<String>, requested:Instant }
struct State {
    running:bool, hello:bool, opened:bool, stream_ready:bool, active:bool, activation_generation:u64,
    focus:Option<Focus>, pending_focus:Option<PendingFocus>, next_id:u64, render_generation:Option<u64>,
    workspaces:Vec<Workspace>, tabs:Vec<Tab>, panes:Vec<Pane>, focused_workspace:Option<String>, outgoing:VecDeque<Command>, queued:usize,
    failure:Option<String>, terminal_failure:bool, wake:Arc<Notify>, stopping:Option<Instant>,
}
impl State {
    fn ready(&self)->Result<(),String> { if self.running && self.hello && self.stopping.is_none() { Ok(()) } else { Err("Herdr endpoint is not ready".into()) } }
    fn input_focus(&self)->Result<&Focus,String> {
        self.ready()?;
        if !self.active || !self.stream_ready || self.pending_focus.is_some() { return Err("Terminal is not ready for input".into()); }
        self.focus.as_ref().filter(|f| f.pane_id.is_some() && f.generation>self.activation_generation).ok_or_else(|| "No confirmed Herdr focus".into())
    }
    fn admit(&mut self, command:Command)->Result<(),String> {
        let size=command.size();
        if size>QUEUE_LIMIT-self.queued { return Err("Herdr output queue is full; operation was not sent".into()); }
        self.queued+=size; self.outgoing.push_back(command); self.wake.notify_one(); Ok(())
    }
    fn cancel_unsent(&mut self) {
        self.outgoing.retain(|c| !matches!(c,Command::Input {..}));
        self.queued=self.outgoing.iter().map(Command::size).sum();
    }
}
struct DeliveryCancellation(Option<Box<dyn FnOnce()+Send>>);
impl DeliveryCancellation {
    fn cancel(&mut self)->Result<(),String> { if let Some(cancel)=self.0.take() { catch_unwind(AssertUnwindSafe(cancel)).map_err(|_| "Herdr delivery cancellation panicked".to_string())?; } Ok(()) }
}
impl Drop for DeliveryCancellation { fn drop(&mut self) { let _=self.cancel(); } }
pub struct Client {
    state:Arc<Mutex<State>>, worker:Option<JoinHandle<Result<(),String>>>, cancellation:DeliveryCancellation,
    authentication:Option<crate::ssh::Control>, control:endpoint::Control,
}
impl Client {
    pub fn start(config:Config, connection_label:String, auth_root:PathBuf,
        notify:impl Fn(Event)->Result<(),String>+Send+Sync+'static, cancel_notify:impl FnOnce()+Send+'static)->Result<Self,String> {
        let cancellation=DeliveryCancellation(Some(Box::new(cancel_notify)));
        let (ssh,pipes)=crate::ssh::Transport::start(config,connection_label,auth_root)?;
        let authentication=ssh.control.clone();
        Self::start_transport(endpoint::Connection::Remote { pipes,control:authentication.clone() },Some(ssh),Some(authentication),notify,cancellation)
    }
    pub fn start_local(endpoint_socket:PathBuf,
        notify:impl Fn(Event)->Result<(),String>+Send+Sync+'static, cancel_notify:impl FnOnce()+Send+'static)->Result<Self,String> {
        Self::start_transport(endpoint::Connection::Local { endpoint_socket },None,None,notify,DeliveryCancellation(Some(Box::new(cancel_notify))))
    }
    fn start_transport(connection:endpoint::Connection, mut ssh:Option<crate::ssh::Transport>, authentication:Option<crate::ssh::Control>,
        notify:impl Fn(Event)->Result<(),String>+Send+Sync+'static, cancellation:DeliveryCancellation)->Result<Self,String> {
        let (mut transport,sender,mut receiver)=endpoint::Transport::start(connection)?;
        let control=transport.control.clone();
        let state=Arc::new(Mutex::new(State { running:true,hello:false,opened:false,stream_ready:false,active:true,activation_generation:0,
            focus:None,pending_focus:None,next_id:1,render_generation:None,workspaces:Vec::new(),tabs:Vec::new(),panes:Vec::new(),focused_workspace:None,outgoing:VecDeque::new(),queued:0,
            failure:None,terminal_failure:false,wake:Arc::new(Notify::new()),stopping:None }));
        let worker_state=Arc::clone(&state); let auth=authentication.clone();
        let worker=thread::Builder::new().name("herdr-events".into()).spawn(move || {
            let mut pump=None;
            let result=catch_unwind(AssertUnwindSafe(|| -> Result<(),String> {
            let runtime=tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e|e.to_string())?;
            let command_state=Arc::clone(&worker_state);
            pump=Some(thread::Builder::new().name("herdr-commands".into()).spawn(move || {
                let result=catch_unwind(AssertUnwindSafe(|| {
                    let runtime=tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e|e.to_string())?;
                    runtime.block_on(commands(&command_state,sender))
                })).unwrap_or_else(|_|Err("Herdr command worker panicked".into()));
                // Channel closure is often a consequence of SSH failure. The
                // event owner publishes its authoritative disposition below.
                if result.is_err() { if let Ok(mut s)=command_state.lock() { s.running=false; } }
                result
            }).map_err(|e|e.to_string())?);
                loop {
                    if let Some(prompt)=auth.as_ref().and_then(|a|a.prompt()) { deliver(&notify,Event::Authentication(prompt))?; }
                    { let s=worker_state.lock().map_err(|_|"Herdr state lock poisoned")?;
                        if !s.running { break; }
                        if s.pending_focus.as_ref().is_some_and(|p|p.requested.elapsed()>Duration::from_secs(20)) { return Err("Timed out waiting for authoritative Herdr focus".into()); }
                    }
                    match runtime.block_on(async { tokio::time::timeout(Duration::from_millis(50),receiver.recv()).await }) {
                        Ok(Some((event,permit)))=>{
                            let events={ let mut s=worker_state.lock().map_err(|_|"Herdr state lock poisoned")?; receive(&mut s,event)? };
                            let start=Instant::now();
                            for event in events.into_iter().flatten() { deliver(&notify,event)?; }
                            if let Ok(mut s)=worker_state.lock() { if let Some(p)=&mut s.pending_focus { p.requested+=start.elapsed(); } }
                            drop(permit);
                        },
                        Ok(None)=>break,
                        Err(_)=>{},
                    }
                } Ok(())
            })).unwrap_or_else(|_|Err("Herdr event callback panicked".into()));
            if let Ok(mut s)=worker_state.lock() { s.running=false; s.wake.notify_waiters(); }
            let cleanup=transport.stop();
            let failure=match auth.as_ref().and_then(|a|a.failure()) {
                Some(error) if error.fatal=>Some(error),
                other=>transport.control.failure().or(other),
            };
            let ssh_cleanup=ssh.as_mut().map_or(Ok(()),|s|s.stop());
            let pump_result=pump.map_or(Ok(()),|p|p.join().map_err(|_|"Herdr command worker panicked".to_string()).and_then(|r|r));
            let result=result.and(cleanup).and(ssh_cleanup);
            let mut s=worker_state.lock().map_err(|_|"Herdr state lock poisoned")?;
            let result=if s.stopping.is_some() { Ok(()) } else {
                result.and_then(|()|failure.as_ref().map_or(pump_result,|f|Err(f.message.clone())))
            };
            if let Err(e)=&result { s.failure=Some(e.clone()); }
            s.terminal_failure|=failure.as_ref().is_some_and(|f|f.fatal);
            s.hello=false; s.focus=None; s.pending_focus=None; s.outgoing.clear(); s.queued=0;
            let stopped=s.stopping.is_some(); let fatal=s.terminal_failure; let message=s.failure.clone().unwrap_or_else(||"Disconnected".into()); drop(s);
            if !stopped {
                let delivered=if fatal { deliver(&notify,Event::Error { message:message.clone(),fatal:true }) } else { Ok(()) }
                    .and_then(|()|deliver(&notify,Event::Disconnected(message)));
                if let Err(e)=delivered { if let Ok(mut s)=worker_state.lock() { s.failure=Some(e.clone()); } return Err(e); }
            }
            result
        }).map_err(|e|format!("Cannot start Herdr event thread: {e}"))?;
        Ok(Self { state,worker:Some(worker),cancellation,authentication,control })
    }
    pub fn answer_auth(&self,answer:crate::auth::Answer)->Result<(),String> { self.authentication.as_ref().ok_or("Local endpoint has no SSH authentication prompt")?.answer(answer) }
    pub fn open(&self, cols: u16, rows: u16, cell_width: u16, cell_height: u16) -> Result<(), String> {
        dimensions(cols, rows, cell_width, cell_height)?;
        let mut s = self.lock()?; s.ready()?;
        if s.opened { return Err("Herdr client is already started".into()); }
        let active = s.active;
        s.admit(Command::Start { dimensions: [cols,rows,cell_width,cell_height], active })?;
        s.opened = true;
        Ok(())
    }

    pub fn focus(&self, pane: &str) -> Result<(), String> {
        valid_id(pane)?;
        let mut s = self.lock()?; s.ready()?;
        if s.pending_focus.is_some() { return Err("A Herdr focus request is still pending".into()); }
        if !s.panes.iter().any(|a| a.pane_id == pane) {
            return Err("Herdr pane is no longer in navigation".into());
        }
        Self::navigate(&mut s, Navigation::Focus { pane_id:pane.into() }, Some(pane.into()))
    }

    pub fn focus_tab(&self, tab: &str) -> Result<(), String> { self.tab_command(false, tab) }
    pub fn close_tab(&self, tab: &str) -> Result<(), String> { self.tab_command(true, tab) }
    pub fn new_tab(&self) -> Result<(), String> {
        let mut s = self.lock()?; s.ready()?;
        let workspace=s.focused_workspace.clone(); Self::navigate(&mut s, Navigation::NewTab(workspace), None)
    }
    pub fn new_tab_in(&self,workspace:&str)->Result<(),String> {
        valid_id(workspace)?;let mut s=self.lock()?;s.ready()?;
        if !s.workspaces.iter().any(|w|w.workspace_id==workspace) { return Err("Herdr workspace is no longer in navigation".into()); }
        Self::navigate(&mut s,Navigation::NewTab(Some(workspace.into())),None)
    }
    pub fn new_workspace(&self) -> Result<(), String> {
        let mut s = self.lock()?; s.ready()?;
        Self::navigate(&mut s, Navigation::NewWorkspace, None)
    }
    fn tab_command(&self, close: bool, tab: &str) -> Result<(), String> {
        valid_id(tab)?;
        let mut s = self.lock()?; s.ready()?;
        if !s.tabs.iter().any(|t| t.tab_id == tab) { return Err("Herdr tab is no longer in navigation".into()); }
        Self::navigate(&mut s, if close { Navigation::CloseTab(tab.into()) } else { Navigation::FocusTab(tab.into()) }, None)
    }
    pub fn focus_workspace(&self,id:&str)->Result<(),String> {
        valid_id(id)?; let mut s=self.lock()?; s.ready()?;
        if !s.workspaces.iter().any(|w|w.workspace_id==id) { return Err("Herdr workspace is no longer in navigation".into()); }
        Self::navigate(&mut s,Navigation::FocusWorkspace(id.into()),None)
    }
    pub fn rename_workspace(&self,id:&str,label:&str)->Result<(),String> { self.resource_label(id,label,|id,label|Navigation::RenameWorkspace{id,label}) }
    pub fn rename_tab(&self,id:&str,label:&str)->Result<(),String> { self.resource_label(id,label,|id,label|Navigation::RenameTab{id,label}) }
    pub fn rename_pane(&self,id:&str,label:Option<&str>)->Result<(),String> {
        valid_id(id)?; if let Some(label)=label { valid_label(label)?; }
        let mut s=self.lock()?;s.ready()?;Self::navigate(&mut s,Navigation::RenamePane{id:id.into(),label:label.map(str::to_owned)},None)
    }
    fn resource_label(&self,id:&str,label:&str,make:impl FnOnce(String,String)->Navigation)->Result<(),String> {
        valid_id(id)?;valid_label(label)?;let mut s=self.lock()?;s.ready()?;Self::navigate(&mut s,make(id.into(),label.into()),None)
    }
    pub fn close_workspace(&self,id:&str)->Result<(),String> { self.resource(id,Navigation::CloseWorkspace) }
    pub fn new_worktree(&self,id:&str,branch:&str)->Result<(),String>{self.resource_label(id,branch,|id,branch|Navigation::NewWorktree{id,branch})}
    pub fn open_worktree(&self,id:&str,path:&str)->Result<(),String>{self.resource_label(id,path,|id,path|Navigation::OpenWorktree{id,path})}
    pub fn remove_worktree(&self,id:&str)->Result<(),String>{self.resource(id,Navigation::RemoveWorktree)}
    pub fn close_pane(&self,id:&str)->Result<(),String> { self.resource(id,Navigation::ClosePane) }
    pub fn zoom_pane(&self,id:&str)->Result<(),String> { self.resource(id,Navigation::ZoomPane) }
    fn resource(&self,id:&str,make:impl FnOnce(String)->Navigation)->Result<(),String> {
        valid_id(id)?;let mut s=self.lock()?;s.ready()?;Self::navigate(&mut s,make(id.into()),None)
    }
    pub fn split_pane(&self,id:&str,workspace:&str,right:bool)->Result<(),String> {
        valid_id(id)?;valid_id(workspace)?;let mut s=self.lock()?;s.ready()?;
        Self::navigate(&mut s,Navigation::SplitPane{id:id.into(),workspace:workspace.into(),right},None)
    }
    pub fn swap_panes(&self,source:&str,target:&str)->Result<(),String> {
        valid_id(source)?;valid_id(target)?;let mut s=self.lock()?;s.ready()?;
        Self::navigate(&mut s,Navigation::SwapPanes{source:source.into(),target:target.into()},None)
    }
    pub fn toggle_right_click(&self,id:&str,to_pane:bool)->Result<(),String> {
        valid_id(id)?;let mut s=self.lock()?;s.ready()?;Self::navigate(&mut s,Navigation::ToggleRightClick{id:id.into(),to_pane},None)
    }
    pub fn reload_config(&self)->Result<(),String> { let mut s=self.lock()?;s.ready()?;Self::navigate(&mut s,Navigation::ReloadConfig,None) }
    fn navigate(s: &mut State, action: Navigation, target: Option<String>) -> Result<(), String> {
        if s.pending_focus.is_some() { return Err("A Herdr navigation request is still pending".into()); }
        let id = s.next_id;
        if id > MAX_GENERATION { return Err("Herdr request IDs exhausted".into()); }
        s.cancel_unsent();
        s.admit(Command::Navigate { id, action })?;
        s.next_id += 1;
        s.pending_focus = Some(PendingFocus { id, target, requested: Instant::now() });
        Ok(())
    }

    /// Reject UI notifications that were queued before a newer focus request/change.
    pub fn confirms_focus(&self, expected: &Focus) -> bool {
        self.state.lock().is_ok_and(|s| s.ready().is_ok() && s.pending_focus.is_none() && s.focus.as_ref() == Some(expected))
    }

    pub fn input_ready(&self, expected: &Focus) -> bool {
        self.state.lock().is_ok_and(|s| s.input_focus().is_ok_and(|focus| focus == expected))
    }

    pub fn resize(&self, cols: u16, rows: u16, cell_width: u16, cell_height: u16) -> Result<(), String> {
        dimensions(cols, rows, cell_width, cell_height)?;
        let mut s = self.lock()?; s.ready()?;
        if !s.opened { return Err("Herdr client is not started".into()); }
        s.cancel_unsent();
        s.admit(Command::Resize([cols,rows,cell_width,cell_height]))?;
        Ok(())
    }

    /// Metadata remains live while inactive. Unsent input is revoked immediately;
    /// reactivation requires a new endpoint projection and native surface frame.
    pub fn set_active(&self, active: bool) -> Result<(), String> {
        let mut s = self.lock()?;
        if !s.running || s.stopping.is_some() { return Err("Herdr client is stopped".into()); }
        if s.active == active { return Ok(()); }
        s.cancel_unsent();
        if s.hello && s.opened {
            s.admit(Command::Active(active))?;
        }
        s.active = active;
        s.activation_generation = s.focus.as_ref().map_or(0, |focus| focus.generation);
        s.stream_ready = false;
        Ok(())
    }

    pub fn key(&self, expected: &Focus, key: &str) -> Result<(), String> {
        if key.is_empty() || key.len() > 256 { return Err("Invalid logical key".into()); }
        self.input(expected, Input::Key(key.into()))
    }
    pub fn text(&self, expected: &Focus, text: &str) -> Result<(), String> {
        self.text_command(expected, false, text)
    }
    pub fn paste(&self, expected: &Focus, text: &str) -> Result<(), String> {
        self.text_command(expected, true, text)
    }
    fn text_command(&self, expected: &Focus, paste: bool, text: &str) -> Result<(), String> {
        if text.len() > INPUT_LIMIT || text.contains('\0') { return Err("Input exceeds 512 KiB or contains NUL".into()); }
        self.input(expected, if paste { Input::Paste(text.into()) } else { Input::Text(text.into()) })
    }
    pub fn scroll(&self, expected: &Focus, up: bool, lines: u16) -> Result<(), String> {
        if !(1..=1000).contains(&lines) { return Err("Scroll lines must be 1..1000".into()); }
        self.input(expected, Input::Scroll { up, lines })
    }
    /// Returns whether the click also started a fenced pane-focus transition.
    pub fn click(&self,expected:&Focus,pane:&str,column:u16,row:u16,right:bool)->Result<bool,String>{
        valid_id(pane)?;
        let mut s=self.lock()?;
        if s.input_focus()? != expected { return Err("Captured input belongs to a stale focus".into()); }
        if expected.pane_id.as_deref()==Some(pane) {
            s.admit(Command::Input { focus:expected.clone(),event:Input::Click{pane:pane.into(),column,row,right} })?;
            return Ok(false);
        }
        if !s.panes.iter().any(|entry|entry.pane_id==pane) { return Err("Herdr pane is no longer in navigation".into()); }
        // One navigation owns both operations: later typing cannot race ahead
        // while the server is still confirming the newly clicked pane.
        Self::navigate(&mut s,Navigation::ClickFocus { focus:expected.clone(),pane_id:pane.into(),column,row,right },Some(pane.into()))?;
        Ok(true)
    }
    fn input(&self, expected: &Focus, event: Input) -> Result<(), String> {
        let mut s = self.lock()?;
        if s.input_focus()? != expected { return Err("Captured input belongs to a stale focus".into()); }
        s.admit(Command::Input { focus: expected.clone(), event })
    }

    pub fn stop(&mut self)->Result<(),String> {
        if self.worker.as_ref().is_some_and(|w|w.thread().id()==thread::current().id()) { return Err("Cannot join Herdr event thread from its callback".into()); }
        let failure=self.failure();
        if let Ok(mut s)=self.state.lock() { s.running=false; s.stopping=Some(Instant::now()); s.focus=None; s.pending_focus=None; s.outgoing.clear(); s.queued=0; s.wake.notify_waiters(); }
        let cancelled=self.cancellation.cancel(); self.control.cancel(); if let Some(a)=&self.authentication { a.cancel(); }
        let joined=self.worker.take().map_or(Ok(()),|w|w.join().map_err(|_|"Herdr event thread panicked".to_string()).and_then(|r|r));
        cancelled.and(joined).and_then(|()|failure.map_or(Ok(()),|error|Err(error.message)))
    }
    pub fn failure(&self)->Option<crate::ssh::Failure> {
        // The watchdog must not wait for queued GUI delivery to learn that
        // sign-in failed fatally, nor replace it with a closed-channel error.
        if let Some(error)=self.authentication.as_ref().and_then(|auth|auth.failure()).filter(|error|error.fatal)
            .or_else(||self.control.failure().filter(|error|error.fatal)) { return Some(error); }
        match self.state.lock() { Ok(s)=>s.failure.as_ref().map(|m|crate::ssh::Failure { message:m.clone(),fatal:s.terminal_failure }),
            Err(_)=>Some(crate::ssh::Failure { message:"Herdr state lock poisoned".into(),fatal:true }) }
    }
    fn lock(&self)->Result<MutexGuard<'_,State>,String> { self.state.lock().map_err(|_|"Herdr state lock poisoned".into()) }
}
impl Drop for Client { fn drop(&mut self) { let _=self.stop(); } }
async fn commands(state:&Mutex<State>, sender:mpsc::Sender<Command>)->Result<(),String> {
    let wake=state.lock().map_err(|_|"Herdr state lock poisoned")?.wake.clone();
    loop {
        let notified=wake.notified(); tokio::pin!(notified); notified.as_mut().enable();
        let waiting={ let s=state.lock().map_err(|_|"Herdr state lock poisoned")?; if !s.running { return Ok(()); } s.outgoing.is_empty() };
        if waiting { notified.await; continue; }
        // Reserve without removing a command: navigation can still revoke every
        // wholly unsubmitted input while the engine applies backpressure.
        let permit=tokio::select! {
            result=tokio::time::timeout(Duration::from_secs(10),sender.reserve())=>result.map_err(|_|"Herdr stopped accepting input")?.map_err(|_|"Herdr endpoint closed")?,
            _=&mut notified=>continue,
        };
        let mut s=state.lock().map_err(|_|"Herdr state lock poisoned")?;
        if !s.running { return Ok(()); }
        if let Some(command)=s.outgoing.pop_front() { s.queued-=command.size(); permit.send(command); }
    }
}
fn receive(s:&mut State,event:EndpointEvent)->Result<[Option<Event>;2],String> {
    let event=match event {
        EndpointEvent::NavigationFailed { id,message }=>{
            let mut events=[Some(Event::Error { message,fatal:false }),None];
            if s.pending_focus.as_ref().is_some_and(|p|p.id==id) {
                s.pending_focus=None;
                if let Some(focus)=&s.focus { events[1]=Some(Event::Focus(focus.clone())); }
            }
            return Ok(events);
        },
        EndpointEvent::Ack(id)=>{
            if let Some(p)=&s.pending_focus { if p.id==id {
                let focus=s.focus.as_ref().filter(|f|p.target.as_ref().is_none_or(|pane|f.pane_id.as_ref()==Some(pane))).ok_or("Herdr acknowledged navigation without authoritative confirmation")?.clone();
                s.pending_focus=None; return Ok([Some(Event::Focus(focus)),None]);
            } } return Ok([None,None]);
        }, EndpointEvent::Ui(event)=>event,
    };
    match &event {
        Event::Ready=>{ if s.hello { return Err("Duplicate native Herdr welcome".into()); } s.hello=true; },
        Event::Navigation { workspaces,tabs,panes,focused_workspace,.. }=>{
            s.workspaces=workspaces.clone(); s.tabs=tabs.clone(); s.panes=panes.clone(); s.focused_workspace=focused_workspace.clone();
        },
        Event::Focus(focus)=>{
            if s.focus.as_ref().is_some_and(|f|focus.generation<=f.generation) { return Err("Non-increasing Herdr focus generation".into()); }
            s.cancel_unsent(); s.stream_ready=false;
            if s.focus.as_ref().and_then(|f|f.pane_id.as_ref())!=focus.pane_id.as_ref() { s.render_generation=None; }
            s.focus=Some(focus.clone()); if s.pending_focus.is_some() { return Ok([None,None]); }
        },
        Event::Reset { generation }=>{
            if !s.opened || !s.focus.as_ref().is_some_and(|f|f.generation==*generation) { return Err("Herdr reset does not match active generation".into()); }
            s.cancel_unsent(); s.stream_ready=false; s.render_generation=Some(*generation);
        },
        Event::Frame { generation,complete,.. }=>{
            if !s.opened || s.render_generation!=Some(*generation) || !s.focus.as_ref().is_some_and(|f|f.generation==*generation && f.pane_id.is_some()) { return Err("Herdr frame does not match active stream generation".into()); }
            if *complete { s.stream_ready=true; }
        },
        Event::Layout { .. }=>{},
        Event::Error { message,fatal }=>{
            s.cancel_unsent();
            if *fatal { s.stream_ready=false; s.failure=Some(message.clone()); s.terminal_failure=true; s.running=false; s.focus=None; s.pending_focus=None; }
        },
        Event::Unavailable(_) | Event::Disconnected(_)=>{ s.stream_ready=false; s.running=false; s.focus=None; s.pending_focus=None; s.outgoing.clear(); s.queued=0; },
        Event::Authentication(_)=>{},
    }
    Ok([Some(event),None])
}
fn deliver(notify:&impl Fn(Event)->Result<(),String>,event:Event)->Result<(),String> {
    catch_unwind(AssertUnwindSafe(||notify(event))).map_err(|_|"Herdr callback panicked".to_string())?.map_err(|e|format!("Herdr event delivery failed; transport closed to prevent byte loss: {e}"))
}
fn valid_id(s:&str)->Result<(),String> { if s.is_empty() || s.len()>256 || s.bytes().any(|b|b<0x21 || b==0x7f) { Err("Invalid Herdr identity".into()) } else { Ok(()) } }
fn valid_label(s:&str)->Result<(),String> { if s.is_empty() || s.len()>256 || s.chars().any(char::is_control) { Err("Herdr label must be 1..256 bytes without control characters".into()) } else { Ok(()) } }
fn dimensions(cols:u16,rows:u16,cell_width:u16,cell_height:u16)->Result<(),String> {
    if ![cols,rows,cell_width,cell_height].iter().all(|n|(1..=1000).contains(n)) || cols.checked_mul(cell_width).is_none() || rows.checked_mul(cell_height).is_none() { return Err("Invalid terminal grid or pixel dimensions".into()); } Ok(())
}
