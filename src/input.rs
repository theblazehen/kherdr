//! Platform-neutral key identity; native Herdr owns terminal encoding.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// Unshifted US physical identity, independently of `KeyInput::text`.
    /// Shifted ASCII punctuation is accepted and mapped to its physical key.
    Character(char),
    Enter, Tab, Backspace, Escape,
    Up, Down, Left, Right, Home, End, PageUp, PageDown, Insert, Delete,
    F(u8),
}

#[derive(Clone, Debug)]
pub struct KeyInput {
    pub key: Key,
    /// Produced UTF-8 before Ctrl/Alt transformations. Function keys use empty
    /// text. A platform-provided C0/DEL/function-key string is never sent as text.
    pub text: String,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub super_key: bool,
    /// Modifiers actually consumed by layout translation, not shortcut matching.
    /// In particular Ctrl-C must NOT consume Ctrl. Touch input normally consumes
    /// Shift when it generated a shifted character, but never Ctrl or Alt.
    pub consumed_shift: bool,
}

impl KeyInput {
    pub fn new(key: Key) -> Self {
        Self { key, text: String::new(), shift: false, ctrl: false, alt: false,
            super_key: false, consumed_shift: false }
    }
}

pub(crate) fn logical_key(input: &KeyInput) -> Result<String, String> {
    let mut value = String::new();
    if input.ctrl { value.push_str("ctrl+"); }
    if input.alt { value.push_str("alt+"); }
    if input.super_key { value.push_str("super+"); }
    if input.shift && !input.consumed_shift { value.push_str("shift+"); }
    let name = match input.key {
        Key::Character(c) => {
            let c = if input.text.is_empty() { c } else {
                let mut chars = input.text.chars();
                let first = chars.next().ok_or("Missing logical key character")?;
                if chars.next().is_some() { return Err("Modified input must be one Unicode character".into()); }
                first
            };
            if c.is_control() { return Err("Control character is not a logical key".into()); }
            match c { '+' => "plus".into(), ' ' => "space".into(), _ => c.to_string() }
        }
        Key::Enter => "enter".into(), Key::Tab => "tab".into(), Key::Escape => "escape".into(),
        Key::Backspace => "backspace".into(), Key::Up => "up".into(), Key::Down => "down".into(),
        Key::Left => "left".into(), Key::Right => "right".into(), Key::Home => "home".into(),
        Key::End => "end".into(), Key::Insert => "insert".into(), Key::Delete => "delete".into(),
        Key::PageUp => "pageup".into(), Key::PageDown => "pagedown".into(), Key::F(n) => format!("f{n}"),
    };
    value.push_str(&name);
    Ok(value)
}

pub(crate) fn shift_character(c: char) -> char {
    if c.is_ascii_lowercase() { return c.to_ascii_uppercase(); }
    "1234567890-=[]\\;',./`".chars().zip("!@#$%^&*()_+{}|:\"<>?~".chars()).find_map(|(from,to)| (c == from).then_some(to)).unwrap_or(c)
}
pub(crate) fn key_input(id: &str) -> Option<KeyInput> {
    let key = match id {
        "escape" => Key::Escape, "tab" => Key::Tab, "enter" => Key::Enter, "backspace" => Key::Backspace,
        "up" => Key::Up, "down" => Key::Down, "left" => Key::Left, "right" => Key::Right,
        "home" => Key::Home, "end" => Key::End, "pageup" => Key::PageUp, "pagedown" => Key::PageDown,
        "insert" => Key::Insert, "delete" => Key::Delete,
        text if text.starts_with('f') && text[1..].parse::<u8>().is_ok() => Key::F(text[1..].parse().ok()?),
        text if text.chars().count() == 1 => Key::Character(text.chars().next()?),
        _ => return None,
    };
    let mut input = KeyInput::new(key);
    if let Key::Character(c) = key { input.text = c.to_string(); }
    Some(input)
}
