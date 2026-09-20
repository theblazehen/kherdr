// Source-only proof driver: production clients, stdin NDJSON commands, stdout NDJSON events.
#![allow(dead_code)]
#[path = "../src/atomic_file.rs"] mod atomic_file;
#[path = "../src/auth.rs"] mod auth;
#[path = "../src/connection.rs"] mod connection;
#[path = "../src/credentials.rs"] mod credentials;
#[path = "../src/trust.rs"] mod trust;
#[path = "../src/ssh.rs"] mod ssh;
#[path = "../src/client.rs"] mod client;
#[path = "../src/endpoint.rs"] mod endpoint;
#[path = "../src/local_runtime.rs"] mod local_runtime;
#[path = "../src/shell.rs"] mod shell;
#[path = "../src/setup.rs"] mod setup;
#[path = "../src/state.rs"] mod state;
use std::{io::{BufRead, Write}, path::PathBuf, sync::{Arc, Mutex}};
use serde_json::{json, Value};
use base64::{Engine, engine::general_purpose::STANDARD};
fn emit(value: Value) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{value}").and_then(|_| out.flush()).map_err(|e| e.to_string())
}
fn run() -> Result<(), String> {
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let init: Value = serde_json::from_str(&lines.next().ok_or("missing init")?.map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
    let root = PathBuf::from(init["root"].as_str().ok_or("missing root")?);
    let focus = Arc::new(Mutex::new(None::<client::Focus>));
    let mut herdr = None;
    let mut pane = None;
    let mut setup_job = None;
    let profile = || -> Result<connection::Profile,String> {
        Ok(connection::Profile { id:"owned-session".into(), name:"Owned session fixture".into(),
            config:serde_json::from_value(init["config"].clone()).map_err(|e|e.to_string())?, session_kind:connection::SessionKind::Herdr,
            herdr_session:init["session"].as_str().unwrap_or("default").into(), herdr_binary:init["binary"].as_str().unwrap_or("herdr").into(),
            remembered_herdr_sessions:Vec::new() })
    };
    if init["kind"] == "setup" {
        let reject_delivery = init["reject_delivery"].as_bool().unwrap_or(false);
        let action = match init["action"].as_str() {
            Some("inspect")=>setup::Action::Inspect,
            Some("start")=>setup::Action::Start, _=>return Err("invalid setup action".into()),
        };
        setup_job = Some(setup::Setup::start(profile()?, root, action, move |event| {
            if reject_delivery && !matches!(event, setup::Event::Authentication(_)) { return Err("deliberately rejected setup delivery".into()); }
            emit(match event {
                setup::Event::Authentication(p)=>json!({"event":"auth","id":p.id,"kind":format!("{:?}",p.kind),"fingerprint":p.fingerprint}),
                setup::Event::Inspected{version,binary,session_running,sessions,start_allowed,detail}=>json!({"event":"inspected","version":version,"binary":binary,"session_running":session_running,"sessions":sessions,"start_allowed":start_allowed,"detail":detail}),
                setup::Event::Completed{binary,detail}=>json!({"event":"completed","binary":binary,"detail":detail}),
                setup::Event::Failed{message}=>json!({"event":"failed","message":message}),
            })
        }, || {})?);
    } else if init["kind"] == "ssh-pane" {
        let panes_root = PathBuf::from(init["panes_root"].as_str().ok_or("missing panes_root")?);
        let prepared = if let Some(descriptor) = init["descriptor"].as_str() {
            shell::discover(&panes_root)?.into_iter().find(|prepared| prepared.descriptor == PathBuf::from(descriptor)).ok_or("live descriptor was not discovered")?
        } else {
            shell::prepare(&panes_root, profile()?, root,
                PathBuf::from(init["api_socket"].as_str().ok_or("missing api_socket")?))?
        };
        pane = Some(shell::Client::attach(&prepared, |event| {
            emit(match event {
                shell::Event::Attached(m) => json!({"event":"attached","launch":m.launch_id,"pane":m.pane_id,"terminal":m.terminal_id}),
                shell::Event::Authentication(p) => json!({"event":"auth","id":p.id,"kind":format!("{:?}",p.kind),"fingerprint":p.fingerprint}),
                shell::Event::Ready => json!({"event":"ready"}),
                shell::Event::Closed{message,fatal} => json!({"event":"closed","message":message,"fatal":fatal}),
                shell::Event::ControlLost(message) => json!({"event":"control_lost","message":message}),
            })
        }, || {})?);
        emit(json!({"event":"prepared","descriptor":prepared.descriptor,"launch":prepared.launch_id}))?;
    } else {
        let seen = focus.clone();
        let notify = move |event| {
            emit(match event {
                client::Event::Authentication(p) => json!({"event":"auth","id":p.id,"kind":format!("{:?}",p.kind),"fingerprint":p.fingerprint}),
                client::Event::Ready => json!({"event":"ready"}),
                client::Event::Focus(f) => {
                    *seen.lock().map_err(|_| "focus poisoned")? = Some(f.clone());
                    json!({"event":"focus","generation":f.generation,"pane":f.pane_id})
                },
                client::Event::Navigation{workspaces,tabs,panes,focused_workspace,agents} => json!({"event":"navigation","focused_workspace":focused_workspace,
                    "agents":agents.into_iter().map(|a|json!({"pane":a.pane_id,"workspace":a.workspace_id,"tab":a.tab_id,"name":a.name,"agent":a.agent,"status":a.status})).collect::<Vec<_>>(),
                    "workspaces":workspaces.into_iter().map(|w|json!({"id":w.workspace_id,"label":w.label,"focused":w.focused,"custom_label":w.custom_label,"worktree_key":w.worktree_key,"worktree_label":w.worktree_label,"worktree_linked":w.worktree_linked})).collect::<Vec<_>>(),
                    "tabs":tabs.into_iter().map(|t| json!({"id":t.tab_id,"workspace":t.workspace_id,"name":t.name,"focused":t.focused,"custom_label":t.custom_label,"zoomed":t.zoomed})).collect::<Vec<_>>(),
                    "panes":panes.into_iter().map(|p|json!({"id":p.pane_id,"tab":p.tab_id,"workspace":p.workspace_id,"name":p.name,"custom_label":p.custom_label,"right_click_passthrough":p.right_click_passthrough})).collect::<Vec<_>>() }),
                client::Event::Layout{panes,popup}=>json!({"event":"layout","panes":panes.into_iter().map(|p|json!({"id":p.pane_id,"rect":p.rect,"inner":p.inner})).collect::<Vec<_>>(),"popup":popup}),
                client::Event::Frame{generation,bytes,complete} => json!({"event":"frame","generation":generation,"data":STANDARD.encode(bytes),"complete":complete}),
                client::Event::Reset{generation} => json!({"event":"reset","generation":generation}),
                client::Event::Unavailable(message) => json!({"event":"unavailable","message":message}),
                client::Event::Disconnected(message) => json!({"event":"closed","message":message}),
                client::Event::Error{message,fatal} => json!({"event":"error","message":message,"fatal":fatal}),
            })
        };
        herdr = Some(if init["kind"] == "local" {
            client::Client::start_local(PathBuf::from(init["endpoint_socket"].as_str().ok_or("missing endpoint_socket")?), notify, || {})?
        } else if init["kind"] == "herdr" {
            let mut profile = profile()?;
            profile.config.remote_command = client::remote_command(&profile.herdr_binary, &profile.herdr_session)?;
            client::Client::start(profile.config, "Owned Herdr fixture".into(), root, notify, || {})?
        } else { return Err("kind must be local, herdr, ssh-pane, or setup".into()); });
    }
    for line in lines {
        let cmd: Value = serde_json::from_str(&line.map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
        let result = (|| -> Result<(),String> {
            let op = cmd["op"].as_str().ok_or("missing op")?;
            if op == "auth" {
                let answer = auth::Answer{id:cmd["id"].as_u64().ok_or("missing auth id")?,approved:cmd["approved"].as_bool().unwrap_or(true),
                    remember_password:cmd["remember_password"].as_bool().unwrap_or(false),
                    secret:zeroize::Zeroizing::new(cmd["secret"].as_str().unwrap_or("").to_string())};
                if let Some(c)=&setup_job { return c.answer_auth(answer); }
                return if let Some(c)=&herdr { c.answer_auth(answer) } else { pane.as_ref().ok_or("missing pane controller")?.answer_auth(answer) };
            }
            if let Some(setup)=&setup_job {
                return match op {
                    "stop"=>Ok(()),
                    "failure"=>emit(json!({"event":"cached_failure","message":setup.failure()})),
                    _=>Err("unknown setup operation".into()),
                };
            }
            let cols = cmd["cols"].as_u64().unwrap_or(80) as u16;
            let rows = cmd["rows"].as_u64().unwrap_or(24) as u16;
            if let Some(c)=&herdr {
                match op {
                    "open"=>c.open(cols,rows,8,16), "resize"=>c.resize(cols,rows,8,16),
                    "active"=>c.set_active(cmd["value"].as_bool().ok_or("missing active value")?),
                    "input_ready"=>{
                        let f=focus.lock().map_err(|_|"focus poisoned")?;
                        emit(json!({"event":"input_ready","ready":f.as_ref().is_some_and(|f|c.input_ready(f))}))
                    },
                    "new_tab"=>c.new_tab(), "new_tab_in"=>c.new_tab_in(cmd["workspace"].as_str().ok_or("missing workspace")?),
                    "new_workspace"=>c.new_workspace(),
                    "focus_tab"=>c.focus_tab(cmd["tab"].as_str().ok_or("missing tab")?),
                    "focus_workspace"=>c.focus_workspace(cmd["workspace"].as_str().ok_or("missing workspace")?),
                    "focus"=>c.focus(cmd["pane"].as_str().ok_or("missing pane")?),
                    "rename_workspace"=>c.rename_workspace(cmd["workspace"].as_str().ok_or("missing workspace")?,cmd["label"].as_str().ok_or("missing label")?),
                    "close_workspace"=>c.close_workspace(cmd["workspace"].as_str().ok_or("missing workspace")?),
                    "new_worktree"=>c.new_worktree(cmd["workspace"].as_str().ok_or("missing workspace")?,cmd["branch"].as_str().ok_or("missing branch")?),
                    "open_worktree"=>c.open_worktree(cmd["workspace"].as_str().ok_or("missing workspace")?,cmd["path"].as_str().ok_or("missing path")?),
                    "remove_worktree"=>c.remove_worktree(cmd["workspace"].as_str().ok_or("missing workspace")?),
                    "rename_tab"=>c.rename_tab(cmd["tab"].as_str().ok_or("missing tab")?,cmd["label"].as_str().ok_or("missing label")?),
                    "close_tab"=>c.close_tab(cmd["tab"].as_str().ok_or("missing tab")?),
                    "rename_pane"=>c.rename_pane(cmd["pane"].as_str().ok_or("missing pane")?,cmd.get("label").and_then(Value::as_str)),
                    "split_right"=>c.split_pane(cmd["pane"].as_str().ok_or("missing pane")?,cmd["workspace"].as_str().ok_or("missing workspace")?,true),
                    "split_down"=>c.split_pane(cmd["pane"].as_str().ok_or("missing pane")?,cmd["workspace"].as_str().ok_or("missing workspace")?,false),
                    "swap"=>c.swap_panes(cmd["pane"].as_str().ok_or("missing pane")?,cmd["target"].as_str().ok_or("missing target")?),
                    "right_click"=>c.toggle_right_click(cmd["pane"].as_str().ok_or("missing pane")?,cmd["to_pane"].as_bool().ok_or("missing to_pane")?),
                    "zoom"=>c.zoom_pane(cmd["pane"].as_str().ok_or("missing pane")?),
                    "close_pane"=>c.close_pane(cmd["pane"].as_str().ok_or("missing pane")?),
                    "reload_config"=>c.reload_config(),
                    "text"|"key"|"paste"=>{
                        let f=focus.lock().map_err(|_|"focus poisoned")?.clone().ok_or("no focus")?;
                        match op { "text"=>c.text(&f,cmd["text"].as_str().ok_or("missing text")?),
                            "paste"=>c.paste(&f,cmd["text"].as_str().ok_or("missing text")?),
                            _=>c.key(&f,cmd["key"].as_str().ok_or("missing key")?) }
                    },
                    "click"=>c.click(&focus.lock().map_err(|_|"focus poisoned")?.clone().ok_or("no focus")?,
                        cmd["pane"].as_str().ok_or("missing pane")?,cmd["column"].as_u64().ok_or("missing column")?.try_into().map_err(|_|"column overflow")?,
                        cmd["row"].as_u64().ok_or("missing row")?.try_into().map_err(|_|"row overflow")?,cmd.get("right").and_then(Value::as_bool).unwrap_or(false)).map(|_|()),
                    "stop"=>Ok(()), _=>Err("unknown Herdr operation".into()),
                }
            } else {
                let c=pane.as_ref().ok_or("missing pane controller")?;
                match op { "close"=>c.close(),"stop"=>Ok(()),_=>Err("SSH controller does not carry terminal input or output".into()) }
            }
        })();
        emit(json!({"event":"ack","id":cmd["request"],"error":result.err()}))?;
        if cmd["op"]=="stop" { break; }
    }
    if let Some(mut c)=herdr { let _=c.stop(); }
    if let Some(mut c)=pane { let _=c.stop(); }
    if let Some(mut c)=setup_job { let _=c.stop(); }
    Ok(())
}
fn main() {
    let mut args = std::env::args().skip(1);
    let result = match args.next().as_deref() {
        None => run(),
        Some("--ssh-pane") => args.next().ok_or("missing private descriptor".to_string()).and_then(|descriptor| {
            if args.next().is_some() { return Err("unexpected helper argument".into()); }
            shell::run(&PathBuf::from(descriptor))
        }),
        _ => Err("unknown probe argument".into()),
    };
    if let Err(error)=result { eprintln!("{error}");std::process::exit(1); }
}
