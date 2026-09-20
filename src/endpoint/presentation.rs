// SPDX-License-Identifier: GPL-3.0-or-later
use super::{array, identity, Error, Result, State, Output, CachedAsset};
use super::codec::{Cell, Cursor, Frame, Surface, Scene, Source};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{Map, Value};
use std::{collections::{HashMap, HashSet}, fmt::Write};
use unicode_width::UnicodeWidthStr;

pub(super) fn truncate(value: &str) -> String {
    let mut end = value.len().min(4096); while !value.is_char_boundary(end) { end -= 1; } value[..end].to_owned()
}
/// Display-only normalization for native endpoint metadata. Escape/control content
/// is never interpreted as native UI markup or retained in metadata.
fn display(value: &Value) -> String {
    let Some(value) = value.as_str() else { return String::new(); };
    let mut chars = value.chars().peekable(); let mut plain = String::new();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            match chars.next() {
                Some('[') => { for c in chars.by_ref() { if ('@'..='~').contains(&c) { break; } } },
                Some(']' | 'P' | 'X' | '^' | '_') => { while let Some(c) = chars.next() { if c == '\x07' { break; } if c == '\x1b' && chars.peek() == Some(&'\\') { chars.next(); break; } } },
                Some(c) if (' '..='/').contains(&c) => { for c in chars.by_ref() { if ('@'..='~').contains(&c) { break; } } },
                _ => {},
            }
        } else if !ch.is_control() { plain.push(ch); }
    }
    truncate(&plain.split_whitespace().collect::<Vec<_>>().join(" "))
}
fn first(values: &[&Value]) -> String { values.iter().find_map(|v| v.as_str().filter(|s| !s.is_empty()).map(|_| display(v))).unwrap_or_default() }
fn pairs(value: &Value) -> Result<Map<String, Value>> {
    if value.is_null() { return Ok(Map::new()); }
    if let Some(map) = value.as_object() { return Ok(map.clone()); }
    let values = value.as_array().ok_or_else(|| Error::protocol("Invalid metadata pairs"))?;
    if values.len() > 4096 { return Err(Error::protocol("Too many metadata pairs")); }
    let mut out = Map::new();
    for pair in values {
        let pair = pair.as_array().filter(|p| p.len() == 2).ok_or_else(|| Error::protocol("Invalid metadata pair"))?;
        let key = pair[0].as_str().ok_or_else(|| Error::protocol("Invalid metadata key"))?;
        if !pair[1].is_string() || out.insert(key.into(), pair[1].clone()).is_some() { return Err(Error::protocol("Duplicate or invalid metadata pair")); }
    } Ok(out)
}
pub(super) fn normalize_pairs(value: &mut Value, key: &str) -> Result<()> { value[key] = Value::Object(pairs(&value[key])?); Ok(()) }
pub(super) fn navigation(snapshot: &Value) -> Result<(Vec<crate::client::Workspace>, Vec<crate::client::Tab>, Vec<crate::client::Pane>, Option<String>, Vec<crate::client::Agent>)> {
    let mut workspaces = HashMap::new(); let mut tabs_by_id = HashMap::new(); let mut agents = HashMap::new();
    let mut positions: HashMap<&str, usize> = HashMap::new(); let mut default_labels = HashMap::new();
    let mut workspace_views = Vec::new();
    for workspace in array(snapshot,"workspaces")? {
        let id=identity(workspace,"workspace_id")?;
        if workspaces.insert(id,workspace).is_some() { return Err(Error::protocol("Duplicate workspace")); }
        let worktree=&workspace["worktree"];
        workspace_views.push(crate::client::Workspace { workspace_id:id.into(), label:first(&[&workspace["label"],&Value::String(id.into())]),
            status:display(&workspace["agent_status"]), focused:workspace["focused"].as_bool()==Some(true), custom_label:workspace["custom_label"].as_bool()==Some(true),
            worktree_key:first(&[&worktree["key"]]), worktree_label:first(&[&worktree["label"]]), worktree_linked:worktree["is_linked_worktree"].as_bool()==Some(true) });
    }
    for tab in array(snapshot,"tabs")? {
        let id = identity(tab,"tab_id")?; let workspace = identity(tab,"workspace_id")?;
        if tabs_by_id.insert(id,tab).is_some() { return Err(Error::protocol("Duplicate tab")); }
        let position = positions.entry(workspace).or_default(); *position += 1; default_labels.insert(id,position.to_string());
    }
    for agent in array(snapshot,"agents")? { if agents.insert(identity(agent,"pane_id")?,agent).is_some() { return Err(Error::protocol("Duplicate agent")); } }
    let empty = Value::Null; let mut panes = Vec::new(); let mut by_tab: HashMap<&str, Vec<(&Value,usize)>> = HashMap::new();
    for pane in array(snapshot,"panes")? {
        let id = identity(pane,"pane_id")?;
        let workspace_id = identity(pane,"workspace_id")?; let tab_id = identity(pane,"tab_id")?;
        let agent = agents.get(id).copied().unwrap_or(&empty); let workspace = workspaces.get(workspace_id).copied().unwrap_or(&empty);
        let tab = tabs_by_id.get(tab_id).copied().unwrap_or(&empty);
        let mut title = first(&[&pane["terminal_title_stripped"],&pane["terminal_title"]]);
        let omp = first(&[&pane["agent"],&agent["agent"]]) == "omp";
        if omp {
            if title == "π" { title.clear(); }
            else if let Some(rest) = title.strip_prefix("π:") { if rest.is_empty() || rest.starts_with(' ') { title = rest.strip_prefix(' ').unwrap_or(rest).into(); } }
            else if let Some(rest) = title.strip_prefix("π ") {
                let mut chars = rest.chars(); if let Some(c) = chars.next() {
                    if ">!:⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".contains(c) {
                        let suffix = chars.as_str(); if suffix.is_empty() || suffix.starts_with(' ') { title = suffix.strip_prefix(' ').unwrap_or(suffix).into(); }
                    }
                }
            }
        }
        let mut name = if omp && pane["custom_label"].as_bool() != Some(true) && !title.is_empty() {
            title.clone()
        } else {
            let title_value = Value::String(title.clone());
            first(&[&pane["title"],&agent["title"],&pane["display_agent"],&agent["display_agent"],&title_value,&pane["agent"],&agent["agent"],&pane["label"],&agent["name"]])
        };
        if name.is_empty() { name = "Shell".into(); }
        let status = display(&pane["agent_status"]); let labels = pairs(&pane["state_labels"])?;
        let state_label = labels.get(&status).map(display).unwrap_or_default();
        let tab_label = if tab["custom_label"].as_bool() == Some(true) || tab["label"].as_str() != default_labels.get(tab_id).map(String::as_str) { display(&tab["label"]) } else { String::new() };
        let mut details = Vec::new();
        for value in [title, tab_label, if state_label.to_lowercase() != status.to_lowercase() { state_label } else { String::new() }, first(&[&pane["foreground_cwd"],&pane["cwd"]])] {
            if !value.is_empty() && value != name && !details.contains(&value) { details.push(value); }
        }
        let mut tokens = pairs(&workspace["tokens"])?; tokens.extend(pairs(&pane["tokens"])?);
        for (key,value) in tokens { if value.as_str().is_some_and(|v| !v.is_empty()) { details.push(format!("{}: {}", display(&Value::String(key)), display(&value))); } }
        let workspace_label = first(&[&workspace["label"],&Value::String(workspace_id.into())]);
        by_tab.entry(tab_id).or_default().push((pane,panes.len()));
        panes.push(crate::client::Pane { pane_id:id.into(),workspace_id:workspace_id.into(),tab_id:tab_id.into(),name,status,workspace:workspace_label,agent:display(&pane["agent"]),detail:truncate(&details.join(" · ")),
            custom_label:pane["custom_label"].as_bool()==Some(true), right_click_passthrough:pane["right_click_passthrough"].as_bool()==Some(true) });
    }
    let mut tabs = Vec::new();
    for tab in array(snapshot,"tabs")? {
        let id = identity(tab,"tab_id")?; let workspace_id = identity(tab,"workspace_id")?;
        let candidates = by_tab.get(id);
        let preferred = candidates.and_then(|rows| rows.iter().find(|(pane,_)| pane["focused"].as_bool() == Some(true)).or_else(|| rows.first())).map(|(_,i)| &panes[*i]);
        let mut name = display(&tab["label"]);
        if tab["custom_label"].as_bool() != Some(true) && tab["label"].as_str() == default_labels.get(id).map(String::as_str) {
            if let Some(pane) = preferred { name = pane.name.clone(); }
        }
        let workspace = workspaces.get(workspace_id).copied().unwrap_or(&empty);
        tabs.push(crate::client::Tab { tab_id:id.into(),workspace_id:workspace_id.into(),workspace:first(&[&workspace["label"],&Value::String(workspace_id.into())]),name,
            status:display(&tab["agent_status"]),pane_id:preferred.map(|p|p.pane_id.clone()).unwrap_or_default(), focused:tab["focused"].as_bool()==Some(true),
            custom_label:tab["custom_label"].as_bool()==Some(true), zoomed:tab["zoomed"].as_bool()==Some(true) });
    }
    let agent_views = array(snapshot, "agents")?.iter().map(|agent| {
        let pane_id = identity(agent, "pane_id")?;
        let pane = panes.iter().find(|pane| pane.pane_id == pane_id);
        Ok(crate::client::Agent {
            pane_id: pane_id.into(), workspace_id: identity(agent, "workspace_id")?.into(),
            tab_id: identity(agent, "tab_id")?.into(),
            name: pane.map(|pane| pane.name.clone()).unwrap_or_else(|| first(&[&agent["title"], &agent["name"], &agent["terminal_title_stripped"]])),
            agent: first(&[&agent["display_agent"], &agent["agent"]]),
            status: display(&agent["agent_status"]),
        })
    }).collect::<Result<Vec<_>>>()?;
    Ok((workspace_views,tabs,panes,snapshot["focused_workspace_id"].as_str().map(str::to_owned),agent_views))
}
fn color(out: &mut String, color: u32, background: bool) -> Result<()> {
    let base = if background { 40 } else { 30 };
    match color {
        0 => { let _ = write!(out,"{}",if background {49} else {39}); },
        1..=16 => { let index = color-1; let _ = write!(out,"{}",if index<8 {base+index} else {base+60+index-8}); },
        n if n >> 24 == 1 => { let _ = write!(out,"{};5;{}",if background {48} else {38},n&255); },
        n if n >> 24 == 2 => { let _ = write!(out,"{};2;{};{};{}",if background {48} else {38},(n>>16)&255,(n>>8)&255,n&255); },
        _ => return Err(Error::protocol("Invalid surface color")),
    } Ok(())
}
fn cells(out: &mut String, cells: &[Cell], x: u16, y: u16, links: &[String]) -> Result<()> {
    let mut style = None; let mut position = true; let mut continuation = 0;
    for (index,cell) in cells.iter().enumerate() {
        if continuation != 0 { continuation -= 1; continue; }
        if cell.skip { position = true; continue; }
        if cell.symbol.chars().any(char::is_control) { return Err(Error::protocol("Control character in surface cell")); }
        if position { let _ = write!(out,"\x1b[{};{}H",u32::from(y)+1,usize::from(x)+index+1); position = false; }
        let current = (cell.fg,cell.bg,cell.modifier);
        if style != Some(current) {
            out.push_str("\x1b[0;"); color(out,cell.fg,false)?; out.push(';'); color(out,cell.bg,true)?;
            for (bit,sgr) in [(1,1),(2,2),(4,3),(8,4),(16,5),(32,6),(64,7),(128,8),(256,9)] { if cell.modifier & bit != 0 { let _ = write!(out,";{sgr}"); } }
            out.push('m'); style = Some(current);
        }
        if let Some(link) = cell.hyperlink {
            let link = links.get(link as usize).ok_or_else(|| Error::protocol("Surface hyperlink out of bounds"))?;
            if link.chars().any(char::is_control) { return Err(Error::protocol("Control character in surface hyperlink")); }
            out.push_str("\x1b]8;;"); out.push_str(link); out.push_str("\x1b\\");
        }
        let symbol = if cell.symbol.is_empty() { " " } else { &cell.symbol };
        out.push_str(symbol); continuation = UnicodeWidthStr::width(symbol).saturating_sub(1);
        if cell.hyperlink.is_some() { out.push_str("\x1b]8;;\x1b\\"); }
    } Ok(())
}
fn cursor(out: &mut String, cursor: Option<&Cursor>, dx: u16, dy: u16) {
    if let Some(cursor) = cursor.filter(|c| c.visible) {
        let _ = write!(out,"\x1b[{};{}H\x1b[{} q\x1b[?25h",u32::from(cursor.y)+u32::from(dy)+1,u32::from(cursor.x)+u32::from(dx)+1,cursor.shape.min(6));
    } else { out.push_str("\x1b[?25l"); }
}
// Retain text/placement metadata only, never a second copy of graphics assets.
// This is independent of the decoded surface, which can advance while hidden.
struct TextFrame { cells: Vec<Cell>, links: Vec<String>, width: u16, height: u16 }
impl TextFrame {
    fn compatible(&self, frame: &Frame) -> bool { self.width == frame.width && self.height == frame.height }
    fn changed(&self, frame: &Frame, y: u16) -> bool {
        let start = usize::from(y)*usize::from(frame.width);
        let end = start+usize::from(frame.width);
        self.links != frame.links || self.cells[start..end] != frame.cells[start..end]
    }
    fn retain(&mut self, frame: &Frame) {
        if !self.compatible(frame) { self.cells.clone_from(&frame.cells); }
        else {
            for (old, new) in self.cells.iter_mut().zip(&frame.cells) {
                if old != new { old.clone_from(new); }
            }
        }
        if self.links != frame.links { self.links.clone_from(&frame.links); }
        self.width = frame.width; self.height = frame.height;
    }
    fn new() -> Self { Self { cells:Vec::new(), links:Vec::new(), width:0, height:0 } }
}
pub(super) struct Presented {
    base: TextFrame, popup: Option<(String, TextFrame)>,
    placements: Vec<super::codec::Placement>,
    pub(super) revision: u64,
}
async fn draw_rows(state: &mut State, output: &Output, frame: &Frame, old: Option<&TextFrame>, dx: u16, dy: u16, width: u16, height: u16, clear_skipped: bool) -> Result<bool> {
    let mut changed = false;
    let mut row = String::new();
    for y in 0..frame.height.min(height) {
        if old.is_some_and(|old| !old.changed(frame,y)) { continue; }
        if !changed { state.frame(output,b"\x1b[?25l").await?; }
        changed = true;
        row.clear(); let start = usize::from(y)*usize::from(frame.width);
        if clear_skipped && frame.cells[start..start+usize::from(frame.width)].iter().any(|cell| cell.skip) {
            // A skipped image/continuation cell is not necessarily a blank
            // symbol. Recreate the old full-clear baseline on this row only.
            let _ = write!(row,"\x1b[0m\x1b[{};1H\x1b[2K",u32::from(y)+1);
        }
        // Whole rows include wide-character leaders and their continuation cells.
        cells(&mut row,&frame.cells[start..start+usize::from(frame.width.min(width))],dx,dy+y,&frame.links)?;
        state.frame(output,row.as_bytes()).await?;
    }
    Ok(changed)
}
impl State {
    pub(super) fn ingest(&mut self, scene: &mut Scene) -> Result<()> {
        let live: HashSet<_> = scene.placements.iter().map(|p| &p.asset).chain(scene.retained.iter()).collect();
        self.assets.retain(|key,asset| { if live.contains(key) { true } else { self.retired.push(asset.id); false } });
        for (key,bytes) in scene.assets.drain(..) {
            if bytes.len() as u64 != key.length || bytes.len() > 32*1024*1024 { return Err(Error::protocol("Invalid graphics asset length")); }
            if let Some(asset) = self.assets.get_mut(&key) {
                // Fingerprints and lengths are part of immutable asset identity.
                if asset.bytes != bytes { return Err(Error::protocol("Graphics asset bytes changed under the same identity")); }
            } else {
                self.asset_serial = self.asset_serial.checked_add(1).ok_or_else(|| Error::new("generation_exhausted","Graphics IDs exhausted"))?;
                self.assets.insert(key,CachedAsset { id:self.asset_serial, bytes, uploaded:false });
            }
        }
        if self.assets.values().map(|a| a.bytes.len()).sum::<usize>() > 64*1024*1024 { return Err(Error::new("line_too_large","Graphics cache exceeds 64MiB")); }
        Ok(())
    }
    pub(super) async fn render(&mut self, output: &Output, surface: &Surface, raw_graphics: bool) -> Result<()> {
        self.stream_ready = false;
        let target = surface.popup.as_ref().map(|p| p.id.clone());
        if target != self.popup { self.popup = target; self.publish_focus(output).await?; }
        let mut previous = self.presented.take();
        let compatible = previous.as_ref().is_some_and(|old| old.base.compatible(&surface.frame)
            && match (&old.popup,&surface.popup) {
                (None,None) => true,
                (Some((id,frame)),Some(popup)) => id == &popup.id && frame.compatible(&popup.frame),
                _ => false,
            });
        if !compatible { self.frame(output,b"\x1b[?25l\x1b[?7l\x1b[0m\x1b[2J").await?; }
        let popup_changed = surface.popup.as_ref().is_some_and(|popup| previous.as_ref()
            .and_then(|old| old.popup.as_ref()).is_none_or(|(_,old)| !old.compatible(&popup.frame)
                || old.cells != popup.frame.cells || old.links != popup.frame.links));
        let base_changed = draw_rows(self,output,&surface.frame,
            previous.as_ref().filter(|_| compatible && !popup_changed).map(|old| &old.base),0,0,surface.frame.width,surface.frame.height,true).await?;
        if raw_graphics { self.frame(output,&surface.frame.graphics).await?; }
        let mut raw = String::new();
        if let Some(popup) = &surface.popup {
            let dx = surface.frame.width.saturating_sub(popup.frame.width)/2;
            let dy = surface.frame.height.saturating_sub(popup.frame.height)/2;
            // Reapply the overlay if base rows were drawn underneath it.
            draw_rows(self,output,&popup.frame,previous.as_ref().filter(|_| compatible && !base_changed)
                .and_then(|old| old.popup.as_ref().map(|(_,frame)| frame)),dx,dy,surface.frame.width-dx,surface.frame.height-dy,false).await?;
            if raw_graphics { self.frame(output,&popup.frame.graphics).await?; }
            cursor(&mut raw,popup.frame.cursor.as_ref(),dx,dy);
        } else { cursor(&mut raw,surface.frame.cursor.as_ref(),0,0); }
        self.frame(output,raw.as_bytes()).await?;
        if !compatible || base_changed || !self.retired.is_empty() || self.assets.values().any(|asset| !asset.uploaded)
            || (raw_graphics && (!surface.frame.graphics.is_empty()
                || surface.popup.as_ref().is_some_and(|popup| !popup.frame.graphics.is_empty())))
            || previous.as_ref().is_none_or(|old| old.placements != surface.scene.placements) {
            self.graphics(output,surface).await?;
        }
        self.commit_frame(output).await?;
        let old = previous.get_or_insert_with(|| Presented { base:TextFrame::new(), popup:None, placements:Vec::new(), revision:0 });
        old.revision = surface.revision;
        old.base.retain(&surface.frame);
        if let Some(popup) = &surface.popup {
            let (id,frame) = old.popup.get_or_insert_with(|| (popup.id.clone(),TextFrame::new()));
            if id != &popup.id { id.clone_from(&popup.id); }
            frame.retain(&popup.frame);
        } else { old.popup = None; }
        if old.placements != surface.scene.placements { old.placements.clone_from(&surface.scene.placements); }
        self.presented = previous;
        Ok(())
    }
    async fn graphics(&mut self, output: &Output, surface: &Surface) -> Result<()> {
        for id in std::mem::take(&mut self.retired) { self.frame(output,format!("\x1b_Ga=d,d=I,i={id},q=2;\x1b\\").as_bytes()).await?; }
        if !self.assets.is_empty() { self.frame(output,b"\x1b_Ga=d,d=a,q=2;\x1b\\").await?; }
        // Temporarily move one immutable cached asset out while emitting: avoid
        // copying the entire RGBA buffer just to update upload state.
        let keys: Vec<_> = self.assets.iter().filter(|(_,a)| !a.uploaded).map(|(k,_)| k.clone()).collect();
        for key in keys {
            let mut asset = self.assets.remove(&key).ok_or_else(|| Error::protocol("Missing graphics asset"))?;
            let result: Result<()> = async {
                let chunks = asset.bytes.len().div_ceil(3072).max(1);
                for index in 0..chunks {
                    let start = index*3072; let end = (start+3072).min(asset.bytes.len());
                    let header = if index == 0 { format!("a=t,f={},s={},v={},i={},q=2,m={}",match key.format {0=>24,1=>32,_=>100},key.width,key.height,asset.id,u8::from(index+1<chunks)) }
                        else { format!("m={}",u8::from(index+1<chunks)) };
                    let encoded = STANDARD.encode(&asset.bytes[start..end]);
                    self.frame(output,format!("\x1b_G{header};{encoded}\x1b\\").as_bytes()).await?;
                } Ok(())
            }.await;
            asset.uploaded = result.is_ok(); self.assets.insert(key,asset); result?;
        }
        for p in &surface.scene.placements {
            let Some(asset) = self.assets.get(&p.asset) else { continue; }; let id = asset.id;
            let (mut dx,mut dy) = (0,0);
            if let Source::Terminal { popup:true, id, .. } = &p.asset.source {
                let Some(popup) = surface.popup.as_ref().filter(|popup| &popup.id == id) else { continue; };
                dx = surface.frame.width.saturating_sub(popup.frame.width)/2; dy = surface.frame.height.saturating_sub(popup.frame.height)/2;
            }
            let raw = format!("\x1b7\x1b[{};{}H\x1b_Ga=p,i={id},p={},q=2,C=1,c={},r={},x={},y={},w={},h={},X={},Y={},z={};\x1b\\\x1b8",
                u32::from(p.y)+u32::from(dy)+1,u32::from(p.x)+u32::from(dx)+1,p.id,p.cols,p.rows,p.source_x,p.source_y,p.source_width,p.source_height,p.x_offset,p.y_offset,p.z);
            self.frame(output,raw.as_bytes()).await?;
        } Ok(())
    }
}

#[cfg(test)]
mod presentation_tests {
    use super::*;
    use crate::client::{EndpointEvent, Event};
    use std::sync::{Arc, atomic::AtomicU64};
    use tokio::sync::{Semaphore, mpsc};

    #[test]
    fn omp_detected_title_outweighs_generic_agent_metadata_but_not_custom_names() {
        let mut snapshot = serde_json::json!({
            "workspaces": [{"workspace_id":"w1", "label":"project"}],
            "tabs": [{"tab_id":"t1", "workspace_id":"w1", "label":"1"}],
            "panes": [{"pane_id":"p1", "workspace_id":"w1", "tab_id":"t1",
                "agent":"omp", "title":"omp", "display_agent":"omp",
                "terminal_title":"π ⠙ Touchscreen navigation", "custom_label":false}],
            "agents": []
        });
        let (_, tabs, panes, _, _) = navigation(&snapshot).unwrap();
        assert_eq!(panes[0].name, "Touchscreen navigation");
        assert_eq!(tabs[0].name, "Touchscreen navigation");

        snapshot["panes"][0]["custom_label"] = Value::Bool(true);
        snapshot["panes"][0]["title"] = Value::String("Release review".into());
        let (_, tabs, panes, _, _) = navigation(&snapshot).unwrap();
        assert_eq!(panes[0].name, "Release review");
        assert_eq!(tabs[0].name, "Release review");
    }

    fn fixture() -> (State, Output, mpsc::Receiver<super::super::Delivery>, Surface) {
        let mut state = State::new(HashSet::new());
        state.focus = Some(Some("pane".into()));
        let (sender, receiver) = mpsc::channel(64);
        let output = Output { sender, capacity:Arc::new(Semaphore::new(super::super::OUTPUT_LIMIT)), progress:Arc::new(AtomicU64::new(0)) };
        let cell = |text: &str| Cell { symbol:text.into(), fg:0, bg:0, modifier:0, skip:false, hyperlink:None };
        let surface = Surface { boot:"boot".into(), projection:1, revision:1,
            frame:Frame { cells:vec![cell("A"),cell(" "),cell("B"),cell(" ")], width:2, height:2, cursor:None, links:Vec::new(), graphics:Vec::new() },
            panes:Vec::new(), popup:None, scene:Scene { assets:Vec::new(), placements:Vec::new(), retained:Vec::new() } };
        (state,output,receiver,surface)
    }
    fn drain(receiver: &mut mpsc::Receiver<super::super::Delivery>) -> (Vec<u8>, usize) {
        let mut bytes = Vec::new(); let mut commits = 0;
        while let Ok((event,_permit)) = receiver.try_recv() {
            if let EndpointEvent::Ui(Event::Frame { bytes:part,complete,.. }) = event {
                assert_eq!(commits,0,"data or duplicate commit after presentation completed");
                if complete { assert!(part.is_empty()); commits += 1; }
                else { bytes.extend(part); }
            }
        }
        (bytes,commits)
    }
    #[tokio::test]
    async fn full_snapshots_diff_committed_rows_and_commit_once() {
        let (mut state,output,mut receiver,mut surface) = fixture();
        state.render(&output,&surface,true).await.unwrap();
        let (bytes,commits) = drain(&mut receiver);
        assert_eq!(commits,1);
        assert!(bytes.windows(4).any(|part| part == b"\x1b[2J"));
        surface.revision += 1;
        state.render(&output,&surface,true).await.unwrap();
        let (bytes,commits) = drain(&mut receiver);
        assert_eq!(commits,1);
        assert!(!bytes.contains(&b'A') && !bytes.contains(&b'B'),"unchanged text was rewritten");
        assert!(!bytes.windows(4).any(|part| part == b"\x1b[2J" || part == b"\x1b[2K"),"unchanged text was erased");
        // The decoded cache may advance without presentation. The next emitted
        // update must include both changes, not merely the newest patch range.
        surface.frame.cells[0].symbol = "界".into();
        surface.frame.cells[1].skip = true;
        surface.revision += 1;
        surface.frame.cells[2].symbol = "C".into();
        surface.revision += 1;
        state.render(&output,&surface,false).await.unwrap();
        let (bytes,commits) = drain(&mut receiver);
        assert_eq!(commits,1);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains('界') && text.contains('C'));
        assert!(!text.contains("\x1b[2J"));
        surface.frame.links.push("https://example.com/first".into());
        surface.frame.cells[2].hyperlink = Some(0);
        state.render(&output,&surface,true).await.unwrap();
        drain(&mut receiver);
        surface.frame.links[0] = "https://example.com/second".into();
        state.render(&output,&surface,true).await.unwrap();
        let (bytes,commits) = drain(&mut receiver);
        assert_eq!(commits,1);
        assert!(String::from_utf8(bytes).unwrap().contains("https://example.com/second"));
        surface.frame.cells[2].skip = true;
        state.render(&output,&surface,false).await.unwrap();
        let (bytes,commits) = drain(&mut receiver);
        assert_eq!(commits,1);
        assert!(!bytes.contains(&b'C'),"skipped cell was redrawn");
        assert!(bytes.windows(4).any(|part| part == b"\x1b[2K"),"previous glyph under skipped cell was not erased");
        assert!(!bytes.windows(4).any(|part| part == b"\x1b[2J"),"one skipped cell cleared the whole surface");
        state.publish_focus(&output).await.unwrap();
        drain(&mut receiver);
        state.render(&output,&surface,true).await.unwrap();
        let (bytes,commits) = drain(&mut receiver);
        assert_eq!(commits,1);
        assert!(bytes.windows(4).any(|part| part == b"\x1b[2J"));
    }
    #[tokio::test]
    async fn failed_row_never_commits_or_retains_partial_presentation() {
        let (mut state,output,mut receiver,mut surface) = fixture();
        surface.frame.cells[2].symbol = "\n".into();
        assert!(state.render(&output,&surface,true).await.is_err());
        let (bytes,commits) = drain(&mut receiver);
        assert!(bytes.contains(&b'A'));
        assert_eq!(commits,0);
        assert!(!state.stream_ready);
        assert!(state.presented.is_none());
    }
}
