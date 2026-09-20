//! Touch keyboard state and projection, independent of endpoint/session ownership.
use crate::{
    input::{KeyInput, key_input, shift_character},
    ui::{AppWindow, KeyRow, TouchKey},
};
use slint::{ModelRc, VecModel};
use std::time::{Duration, Instant};

#[derive(Default)]
pub(crate) struct Keyboard {
    shift: bool,
    caps_lock: bool,
    ctrl: bool,
    alt: bool,
    page: u8,
    last_shift_tap: Option<Instant>,
}
impl Keyboard {
    pub(crate) fn toggle(&mut self, id: &str) -> bool {
        match id {
            "ctrl" => {
                self.ctrl = !self.ctrl;
                true
            }
            "alt" => {
                self.alt = !self.alt;
                true
            }
            "shift" => {
                let now = std::time::Instant::now();
                if self.caps_lock {
                    self.caps_lock = false;
                    self.shift = false;
                    self.last_shift_tap = None;
                } else if self.shift
                    && self
                        .last_shift_tap
                        .is_some_and(|last| now.duration_since(last) <= Duration::from_millis(450))
                {
                    self.caps_lock = true;
                    self.shift = false;
                    self.last_shift_tap = None;
                } else {
                    self.shift = !self.shift;
                    self.last_shift_tap = self.shift.then_some(now);
                }
                true
            }
            "symbols" => {
                self.page = if self.page == 0 { 1 } else { 0 };
                true
            }
            "functions" => {
                self.page = if self.page == 2 { 1 } else { 2 };
                true
            }

            _ => false,
        }
    }
    pub(crate) fn input(&self, value: &str, literal: bool) -> Option<KeyInput> {
        let mut key = key_input(value)?;
        key.ctrl = self.ctrl;
        key.alt = self.alt;
        key.shift = self.shift;
        let caps = self.caps_lock
            && !self.ctrl
            && !self.alt
            && !literal
            && !key.text.is_empty()
            && key.text.chars().all(|c| c.is_ascii_alphabetic());
        if !literal && (self.shift || caps) && !key.text.is_empty() {
            key.text = key.text.chars().map(shift_character).collect();
            key.shift = true;
            key.consumed_shift = true;
        }
        // Alternate legends remain literal, even with Shift or Caps active.
        if literal {
            key.shift = false;
        }
        Some(key)
    }
    pub(crate) fn consume(&mut self) -> bool {
        let changed = self.ctrl || self.alt || self.shift;
        self.ctrl = false;
        self.alt = false;
        self.shift = false;
        self.last_shift_tap = None;
        changed
    }
    pub(crate) fn cancel(&mut self) -> bool {
        let caps = self.caps_lock;
        self.caps_lock = false;
        self.consume() || caps
    }
    pub(crate) fn render(&self, ui: &AppWindow) {
        let make = |id: &str, label: &str, stretch: f32| TouchKey {
            id: id.into(),
            label: label.into(),
            secondary: "".into(),
            stretch,
            active: match id {
                "ctrl" => self.ctrl,
                "alt" => self.alt,
                "shift" => self.shift || self.caps_lock,
                "symbols" => self.page == 1,
                "functions" => self.page == 2,
                _ => false,
            },
        };
        let character = |c: char, stretch| {
            let id = c.to_string();
            if self.shift || (self.caps_lock && !self.ctrl && !self.alt && c.is_ascii_alphabetic())
            {
                make(&id, &shift_character(c).to_string(), stretch)
            } else {
                make(&id, &id, stretch)
            }
        };
        let letter = |(c, secondary): (char, char)| {
            let mut key = character(c, 1.);
            key.secondary = secondary.to_string().into();
            key
        };
        let characters = |text: &str| text.chars().map(|c| character(c, 1.)).collect::<Vec<_>>();
        let centers = [
            vec![
                make("escape", "Esc", 1.),
                make("ctrl", "Ctrl", 1.),
                make("alt", "Alt", 1.),
            ],
            vec![
                make("home", "Home", 1.),
                make("up", "↑", 1.),
                make("pageup", "PgUp", 1.),
            ],
            vec![
                make("left", "←", 1.),
                make("down", "↓", 1.),
                make("right", "→", 1.),
            ],
            vec![
                make("end", "End", 1.),
                make("tab", "Tab", 1.),
                make("pagedown", "PgDn", 1.),
            ],
        ];
        let (mut left, mut right) = match self.page {
            1 => {
                let numbers = |text: &str, holds: &str| {
                    text.chars()
                        .zip(holds.chars())
                        .map(letter)
                        .collect::<Vec<_>>()
                };
                let mut symbols = characters(">\\|");
                symbols.push(letter(('`', '~')));
                symbols.push(make("backspace", "⌫", 1.));
                let mut punctuation = vec![make("functions", "Fn", 1.)];
                punctuation.extend(characters(";:'\""));
                (
                    vec![numbers("12345", "!@#$%"), characters("[]{}<"), punctuation],
                    vec![numbers("67890", "^&*()"), symbols, characters("=+-_/")],
                )
            }
            2 => {
                let functions = |range: std::ops::RangeInclusive<i32>| {
                    range
                        .map(|n| make(&format!("f{n}"), &format!("F{n}"), 1.))
                        .collect::<Vec<_>>()
                };
                let mut editing = functions(11..=12);
                editing.extend([
                    make("insert", "Ins", 1.),
                    make("delete", "Del", 1.),
                    character('`', 1.),
                ]);
                let mut symbols = characters("\\|~=");
                symbols.push(make("backspace", "⌫", 1.));
                let mut punctuation = vec![make("functions", "123", 1.)];
                punctuation.extend(characters("!@#$"));
                (
                    vec![functions(1..=5), editing, punctuation],
                    vec![functions(6..=10), symbols, characters("+_?!/")],
                )
            }
            _ => {
                let letters = |text: &str, holds: &str| {
                    text.chars()
                        .zip(holds.chars())
                        .map(letter)
                        .collect::<Vec<_>>()
                };
                let mut lower_left =
                    vec![make("shift", if self.caps_lock { "⇪" } else { "⇧" }, 1.)];
                lower_left.extend(letters("zxcv", "_£\"'"));
                let mut home_right = letters("hjkl", "+=()");
                home_right.push(make("backspace", "⌫", 1.));
                let mut lower_right = letters("bnm", ":;/");
                lower_right.extend([character('!', 1.), character('?', 1.)]);
                (
                    vec![
                        letters("qwert", "12345"),
                        letters("asdfg", "@#&*-"),
                        lower_left,
                    ],
                    vec![letters("yuiop", "67890"), home_right, lower_right],
                )
            }
        };
        let layer = make("symbols", if self.page == 0 { "123" } else { "ABC" }, 1.);
        left.push(vec![layer, character(',', 1.), make(" ", "", 3.)]);
        right.push(vec![
            make(" ", "", 3.),
            make(".", ".", 1.),
            make("enter", "↵", 1.),
        ]);
        let rows = left
            .into_iter()
            .zip(centers)
            .zip(right)
            .map(|((left, center), right)| KeyRow {
                left: ModelRc::new(VecModel::from(left)),
                center: ModelRc::new(VecModel::from(center)),
                right: ModelRc::new(VecModel::from(right)),
            })
            .collect::<Vec<_>>();
        ui.set_keyboard_rows(ModelRc::new(VecModel::from(rows)));
    }
}
