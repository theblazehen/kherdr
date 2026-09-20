// SPDX-License-Identifier: GPL-3.0-or-later
// Frozen generation-1 layouts: stock Herdr v0.9.0 protocol/{endpoint,wire}.rs.
use super::{Error, Result};

pub fn uint(out: &mut Vec<u8>, n: u64) {
    if n < 251 { out.push(n as u8); }
    else if n <= u16::MAX as u64 { out.push(251); out.extend_from_slice(&(n as u16).to_le_bytes()); }
    else if n <= u32::MAX as u64 { out.push(252); out.extend_from_slice(&(n as u32).to_le_bytes()); }
    else { out.push(253); out.extend_from_slice(&n.to_le_bytes()); }
}
pub fn string(out: &mut Vec<u8>, s: &str) { uint(out, s.len() as u64); out.extend_from_slice(s.as_bytes()); }
pub fn control(kind: &str, data: &str) -> Vec<u8> { let mut out = vec![20]; string(&mut out, kind); string(&mut out, data); out }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell { pub symbol: String, pub fg: u32, pub bg: u32, pub modifier: u16, pub skip: bool, pub hyperlink: Option<u32> }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor { pub x: u16, pub y: u16, pub visible: bool, pub shape: u8 }
#[derive(Debug)]
pub struct Frame { pub cells: Vec<Cell>, pub width: u16, pub height: u16, pub cursor: Option<Cursor>, pub links: Vec<String>, pub graphics: Vec<u8> }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pane { pub id: String, pub rect:[u16;4], pub inner:[u16;4], pub focused: bool }
#[derive(Debug)]
pub struct Popup { pub id: String, pub frame: Frame }
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub enum Source { Terminal { popup: bool, id: String, image: u32 }, Layer { pane: String, layer: String } }
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct AssetKey { pub source: Source, pub width: u32, pub height: u32, pub format: u32, pub length: u64, pub fingerprint: u64 }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement { pub asset: AssetKey, pub id: u32, pub x: u16, pub y: u16, pub cols: u32, pub rows: u32, pub source_x: u32, pub source_y: u32, pub source_width: u32, pub source_height: u32, pub x_offset: u32, pub y_offset: u32, pub z: i32 }
#[derive(Debug)]
pub struct Scene { pub assets: Vec<(AssetKey, Vec<u8>)>, pub placements: Vec<Placement>, pub retained: Vec<AssetKey> }
#[derive(Debug)]
pub struct Surface { pub boot: String, pub projection: u64, pub revision: u64, pub frame: Frame, pub panes: Vec<Pane>, pub popup: Option<Popup>, pub scene: Scene }
#[derive(Debug)]
pub struct Row { pub x: u16, pub y: u16, pub cells: Vec<Cell> }
#[derive(Debug)]
pub struct Patch { pub boot: String, pub projection: u64, pub base: u64, pub revision: u64, pub rows: Vec<Row>, pub panes: Vec<Pane>, pub cursor: Option<Cursor> }

pub struct Decoder<'a> { raw: &'a [u8], pos: usize }
impl<'a> Decoder<'a> {
    pub fn new(raw: &'a [u8]) -> Self { Self { raw, pos: 0 } }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.raw.len() - self.pos { return Err(Error::protocol("Truncated endpoint payload")); }
        let start = self.pos; self.pos += n; Ok(&self.raw[start..self.pos])
    }
    pub fn byte(&mut self) -> Result<u8> { Ok(self.take(1)?[0]) }
    pub fn uint(&mut self) -> Result<u64> {
        let byte = self.byte()?;
        Ok(match byte {
            0..=250 => byte as u64,
            251 => u16::from_le_bytes(self.take(2)?.try_into().map_err(|_| Error::protocol("Invalid u16"))?) as u64,
            252 => u32::from_le_bytes(self.take(4)?.try_into().map_err(|_| Error::protocol("Invalid u32"))?) as u64,
            253 => u64::from_le_bytes(self.take(8)?.try_into().map_err(|_| Error::protocol("Invalid u64"))?),
            _ => return Err(Error::protocol("Invalid endpoint integer")),
        })
    }
    pub fn u32(&mut self) -> Result<u32> { self.uint()?.try_into().map_err(|_| Error::protocol("Endpoint u32 overflow")) }
    fn u16(&mut self) -> Result<u16> { self.uint()?.try_into().map_err(|_| Error::protocol("Endpoint u16 overflow")) }
    pub fn boolean(&mut self) -> Result<bool> { match self.byte()? { 0 => Ok(false), 1 => Ok(true), _ => Err(Error::protocol("Invalid endpoint bool/option")) } }
    fn option<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<Option<T>> { if self.boolean()? { Ok(Some(f(self)?)) } else { Ok(None) } }
    pub fn blob(&mut self) -> Result<Vec<u8>> { let n = usize::try_from(self.uint()?).map_err(|_| Error::protocol("Endpoint blob overflow"))?; Ok(self.take(n)?.to_vec()) }
    pub fn string(&mut self) -> Result<String> { String::from_utf8(self.blob()?).map_err(|_| Error::protocol("Invalid endpoint UTF-8")) }
    pub fn optional_string(&mut self) -> Result<Option<String>> { self.option(Self::string) }
    fn vec<T>(&mut self, max: usize, mut f: impl FnMut(&mut Self) -> Result<T>) -> Result<Vec<T>> {
        let n = usize::try_from(self.uint()?).map_err(|_| Error::protocol("Endpoint vector overflow"))?;
        if n > max || n > self.raw.len() - self.pos { return Err(Error::protocol("Oversized endpoint vector")); }
        let mut out = Vec::with_capacity(n); for _ in 0..n { out.push(f(self)?); } Ok(out)
    }
    fn rect(&mut self) -> Result<[u16;4]> { Ok([self.u16()?,self.u16()?,self.u16()?,self.u16()?]) }
    fn cursor(&mut self) -> Result<Cursor> { Ok(Cursor { x: self.u16()?, y: self.u16()?, visible: self.boolean()?, shape: self.byte()? }) }
    fn cell(&mut self) -> Result<Cell> { Ok(Cell { symbol: self.string()?, fg: self.u32()?, bg: self.u32()?, modifier: self.u16()?, skip: self.boolean()?, hyperlink: self.option(Self::u32)? }) }
    fn frame(&mut self) -> Result<Frame> {
        let cells = self.vec(1_000_000, Self::cell)?;
        let width = self.u16()?; let height = self.u16()?;
        let cursor = self.option(Self::cursor)?;
        let links = self.vec(1_000_000, Self::string)?; let graphics = self.blob()?;
        if width > 1000 || height > 1000 || cells.len() != width as usize * height as usize { return Err(Error::protocol("Invalid surface grid")); }
        Ok(Frame { cells, width, height, cursor, links, graphics })
    }
    fn pane(&mut self) -> Result<Pane> {
        let id = self.string()?; self.uint()?; let rect=self.rect()?; let inner=self.rect()?; self.option(Self::rect)?;
        self.option(|d| { d.uint()?; d.uint()?; d.uint()?; Ok(()) })?;
        let focused = self.boolean()?; self.boolean()?; self.boolean()?; self.boolean()?; self.u32()?; self.u32()?;
        Ok(Pane { id, rect, inner, focused })
    }
    fn split(&mut self) -> Result<()> {
        if self.u32()? > 1 { return Err(Error::protocol("Invalid split direction")); }
        self.u16()?; self.rect()?; self.rect()?; self.vec(1024, Self::boolean)?; Ok(())
    }
    fn popup_size(&mut self) -> Result<()> { match self.u32()? { 0 => { self.u16()?; }, 1 => { self.byte()?; }, _ => return Err(Error::protocol("Invalid popup size")) } Ok(()) }
    fn popup(&mut self) -> Result<Popup> {
        let id = self.string()?; self.string()?; self.option(Self::popup_size)?; self.option(Self::popup_size)?;
        let frame = self.frame()?; self.boolean()?; self.boolean()?; self.u32()?; self.u32()?; Ok(Popup { id, frame })
    }
    fn key(&mut self) -> Result<AssetKey> {
        let source = match self.u32()? {
            0 => { let target = self.u32()?; if target > 1 { return Err(Error::protocol("Invalid graphics target")); }
                Source::Terminal { popup: target == 1, id: self.string()?, image: self.u32()? } },
            1 => Source::Layer { pane: self.string()?, layer: self.string()? },
            _ => return Err(Error::protocol("Invalid graphics source")),
        };
        let width = self.u32()?; let height = self.u32()?; let format = self.u32()?;
        if format > 2 { return Err(Error::protocol("Invalid graphics format")); }
        Ok(AssetKey { source, width, height, format, length: self.uint()?, fingerprint: self.uint()? })
    }
    fn placement(&mut self) -> Result<Placement> {
        let asset = self.key()?; let id = self.u32()?; let x = self.u16()?; let y = self.u16()?;
        let cols = self.u32()?; let rows = self.u32()?; let source_x = self.u32()?; let source_y = self.u32()?;
        let source_width = self.u32()?; let source_height = self.u32()?; let x_offset = self.u32()?; let y_offset = self.u32()?;
        let zigzag = self.u32()?; let z = ((zigzag >> 1) as i32) ^ -((zigzag & 1) as i32); self.u32()?;
        Ok(Placement { asset, id, x, y, cols, rows, source_x, source_y, source_width, source_height, x_offset, y_offset, z })
    }
    pub fn surface(&mut self) -> Result<Surface> {
        let boot = self.string()?; let projection = self.uint()?; let revision = self.uint()?;
        let frame = self.frame()?; let panes = self.vec(512, Self::pane)?; self.vec(512, Self::split)?;
        let popup = self.option(Self::popup)?;
        let assets = self.vec(4096, |d| Ok((d.key()?, d.blob()?)))?;
        let placements = self.vec(16384, Self::placement)?; let retained = self.vec(4096, Self::key)?;
        Ok(Surface { boot, projection, revision, frame, panes, popup, scene: Scene { assets, placements, retained } })
    }
    pub fn patch(&mut self) -> Result<Patch> {
        Ok(Patch { boot: self.string()?, projection: self.uint()?, base: self.uint()?, revision: self.uint()?,
            rows: self.vec(1_000_000, |d| Ok(Row { x: d.u16()?, y: d.u16()?, cells: d.vec(1_000_000, Self::cell)? }))?,
            panes: self.vec(512, Self::pane)?, cursor: self.option(Self::cursor)? })
    }
    pub fn finish(&self) -> Result<()> { if self.pos == self.raw.len() { Ok(()) } else { Err(Error::protocol("Trailing endpoint payload")) } }
}

pub fn key_event(value: &str) -> Result<Vec<u8>> {
    let mut rest = value; let mut modifiers = 0;
    while let Some((prefix, suffix)) = rest.split_once('+') {
        let bit = match prefix.to_ascii_lowercase().as_str() { "shift" => 1, "ctrl" => 2, "alt" => 4, "super" => 8, _ => break };
        modifiers |= bit; rest = suffix;
    }
    let lowered = rest.to_ascii_lowercase();
    let name = match lowered.as_str() { "escape" => "esc", "return" => "enter", "space" => " ", other => other };
    let names = ["backspace", "enter", "left", "right", "up", "down", "home", "end", "pageup", "pagedown", "tab", "backtab", "delete", "insert", "esc"];
    let mut out = vec![0];
    if let Some(i) = names.iter().position(|n| *n == name) { uint(&mut out, i as u64); }
    else if rest.chars().count() == 1 || name == " " {
        out.push(15); out.extend_from_slice(if name == " " { b" " } else { rest.as_bytes() });
    } else if let Some(n) = lowered.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()).filter(|n| (1..=35).contains(n) && lowered == format!("f{n}")) { out.extend_from_slice(&[16, n]); }
    else { return Err(Error::new("invalid_key", format!("Unsupported semantic key: {value}"))); }
    out.extend_from_slice(&[modifiers, 0, 1, 0, 0, 0, 0, 0]); Ok(out)
}
pub fn mouse_click(column:u16,row:u16,right:bool)->Vec<u8>{
    let mut out=Vec::with_capacity(18);
    for kind in [0u64,1]{out.push(2);uint(&mut out,kind);uint(&mut out,if right{1}else{0});uint(&mut out,0);uint(&mut out,u64::from(column));uint(&mut out,u64::from(row));out.extend([0,0]);uint(&mut out,1);}
    out
}
