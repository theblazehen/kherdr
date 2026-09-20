//! Admission of local editor, terminal, pointer and clipboard input.
use crate::{
    App, INPUT_LIMIT,
    input::{Key, KeyInput, key_input, logical_key},
    platform, status,
    ui::{self, AppWindow},
    with_app,
};
use slint::{ComponentHandle, Model};
use std::time::Duration;

fn dispatch_editor_key(
    ui: &AppWindow,
    text: slint::SharedString,
    mut modifiers: Vec<slint::SharedString>,
    paste: bool,
) {
    use slint::platform::WindowEvent;
    let physical = platform::physical_modifiers();
    if !paste {
        for modifier in &physical {
            if !modifiers.contains(modifier) {
                modifiers.push(modifier.clone());
            }
        }
    }
    let dispatch = |event| {
        if let Err(error) = ui.window().try_dispatch_event(event) {
            ui.set_connection_error(format!("Local input delivery failed: {error}").into());
        }
    };
    // Synthetic events must not release a modifier still physically held. Paste
    // also needs a neutral modifier state even if Ctrl+V is still held down.
    for modifier in &physical {
        dispatch(WindowEvent::KeyReleased {
            text: modifier.clone(),
        });
    }
    for modifier in &modifiers {
        dispatch(WindowEvent::KeyPressed {
            text: modifier.clone(),
        });
    }
    dispatch(WindowEvent::KeyPressed { text: text.clone() });
    dispatch(WindowEvent::KeyReleased { text });
    for modifier in modifiers.into_iter().rev() {
        dispatch(WindowEvent::KeyReleased { text: modifier });
    }
    for modifier in physical {
        dispatch(WindowEvent::KeyPressed { text: modifier });
    }
}

pub(crate) fn physical_input(text: &str) -> Option<KeyInput> {
    let specials = [
        (slint::platform::Key::Return, "enter"),
        (slint::platform::Key::Escape, "escape"),
        (slint::platform::Key::Tab, "tab"),
        (slint::platform::Key::Backspace, "backspace"),
        (slint::platform::Key::UpArrow, "up"),
        (slint::platform::Key::DownArrow, "down"),
        (slint::platform::Key::LeftArrow, "left"),
        (slint::platform::Key::RightArrow, "right"),
        (slint::platform::Key::Home, "home"),
        (slint::platform::Key::End, "end"),
        (slint::platform::Key::PageUp, "pageup"),
        (slint::platform::Key::PageDown, "pagedown"),
        (slint::platform::Key::Insert, "insert"),
        (slint::platform::Key::Delete, "delete"),
        (slint::platform::Key::Backtab, "tab"),
        (slint::platform::Key::F1, "f1"),
        (slint::platform::Key::F2, "f2"),
        (slint::platform::Key::F3, "f3"),
        (slint::platform::Key::F4, "f4"),
        (slint::platform::Key::F5, "f5"),
        (slint::platform::Key::F6, "f6"),
        (slint::platform::Key::F7, "f7"),
        (slint::platform::Key::F8, "f8"),
        (slint::platform::Key::F9, "f9"),
        (slint::platform::Key::F10, "f10"),
        (slint::platform::Key::F11, "f11"),
        (slint::platform::Key::F12, "f12"),
    ];
    for (key, id) in specials {
        let value: slint::SharedString = key.into();
        if text == value.as_str() {
            return key_input(id);
        }
    }
    for key in [
        slint::platform::Key::Shift,
        slint::platform::Key::ShiftR,
        slint::platform::Key::Control,
        slint::platform::Key::ControlR,
        slint::platform::Key::Alt,
        slint::platform::Key::AltGr,
        slint::platform::Key::Meta,
        slint::platform::Key::MetaR,
        slint::platform::Key::CapsLock,
    ] {
        let value: slint::SharedString = key.into();
        if text == value.as_str() {
            return None;
        }
    }
    key_input(text)
}

impl App {
    pub(crate) fn local_input(&self, ui: &AppWindow, input: &KeyInput) -> bool {
        if !ui.get_local_editor_active() {
            return false;
        }
        use slint::platform::Key as LocalKey;
        let text: slint::SharedString = match input.key {
            Key::Character(_) => input.text.clone().into(),
            Key::Enter => LocalKey::Return.into(),
            Key::Escape => LocalKey::Escape.into(),
            Key::Tab => LocalKey::Tab.into(),
            Key::Backspace => LocalKey::Backspace.into(),
            Key::Delete => LocalKey::Delete.into(),
            Key::Insert => LocalKey::Insert.into(),
            Key::Left => LocalKey::LeftArrow.into(),
            Key::Right => LocalKey::RightArrow.into(),
            Key::Up => LocalKey::UpArrow.into(),
            Key::Down => LocalKey::DownArrow.into(),
            Key::Home => LocalKey::Home.into(),
            Key::End => LocalKey::End.into(),
            Key::PageUp => LocalKey::PageUp.into(),
            Key::PageDown => LocalKey::PageDown.into(),
            _ => return true,
        };
        let modifiers: Vec<slint::SharedString> = [
            (input.ctrl, LocalKey::Control),
            (input.alt, LocalKey::Alt),
            (input.shift, LocalKey::Shift),
            (input.super_key, LocalKey::Meta),
        ]
        .into_iter()
        .filter_map(|(enabled, key)| enabled.then(|| key.into()))
        .collect();
        let generation = ui.get_local_editor_generation();
        let editor_target = ui.get_editor_target();
        let keyboard_generation = ui.global::<ui::KeyboardInteraction>().get_generation();
        let auth_generation = ui.get_auth_generation();
        let weak = ui.as_weak();
        slint::Timer::single_shot(Duration::ZERO, move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if !ui.get_application_active()
                || !ui.get_local_editor_active()
                || (ui.get_local_editor_generation() != generation
                    || ui.get_editor_target() != editor_target)
                || ui.get_auth_generation() != auth_generation
                || ui.global::<ui::KeyboardInteraction>().get_generation() != keyboard_generation
            {
                return;
            }
            dispatch_editor_key(&ui, text, modifiers, false);
        });
        true
    }

    pub(crate) fn cancel_interaction(&mut self, ui: &AppWindow) {
        self.clipboard.cancel();
        ui.invoke_cancel_input();
        let modifiers_changed = self.keyboard.cancel();
        self.selection = None;
        ui.set_selection_start(-1);
        ui.set_selection_end(-1);
        if modifiers_changed {
            self.keyboard.render(ui);
        }
    }

    pub(crate) fn send(&mut self, ui: &AppWindow, input: &KeyInput) {
        if !ui.get_remote_input_allowed() {
            return;
        }
        if !self.input_ready {
            status(ui, "Terminal is not ready for input");
            return;
        }
        let result = (|| -> Result<(), String> {
            let focus = self
                .endpoints
                .active
                .focus
                .as_ref()
                .ok_or("No confirmed terminal focus")?;
            let client = self
                .endpoints
                .active
                .client
                .as_ref()
                .ok_or("Not connected")?;
            if !input.ctrl && !input.alt && !input.super_key && !input.text.is_empty() {
                client.text(focus, &input.text)
            } else {
                client.key(focus, &logical_key(input)?)
            }
        })();
        if let Err(error) = result {
            status(ui, error);
        }
    }

    pub(crate) fn key(&mut self, ui: &AppWindow, id: &str) {
        if !self.input_ready && !ui.get_local_editor_active() {
            return;
        }
        if self.keyboard.toggle(id) {
            self.keyboard.render(ui);
        } else if id == "paste" {
            self.paste(ui);
        } else if !id.is_empty() {
            self.keyboard_input(ui, id, false);
        }
    }

    pub(crate) fn keyboard_input(&mut self, ui: &AppWindow, value: &str, literal: bool) {
        let Some(key) = self.keyboard.input(value, literal) else {
            status(ui, "Unknown keyboard key");
            return;
        };
        if !self.local_input(ui, &key) {
            if !self.input_ready {
                return;
            }
            self.send(ui, &key);
        }
        if self.keyboard.consume() {
            self.keyboard.render(ui);
        }
    }

    pub(crate) fn paste(&mut self, ui: &AppWindow) {
        if ui.get_local_editor_active() {
            ui.invoke_restore_editor_focus();
            let generation = ui.get_local_editor_generation();
            let editor_target = ui.get_editor_target();
            let keyboard_generation = ui.global::<ui::KeyboardInteraction>().get_generation();
            let auth_generation = ui.get_auth_generation();
            let weak = ui.as_weak();
            let result = self.clipboard.request(false, 65536, move |result| {
                if let Err(error) = weak.upgrade_in_event_loop(move |ui| {
                    if !ui.get_application_active()
                        || !ui.get_local_editor_active()
                        || (ui.get_local_editor_generation() != generation
                            || ui.get_editor_target() != editor_target)
                        || ui.get_auth_generation() != auth_generation
                        || ui.global::<ui::KeyboardInteraction>().get_generation()
                            != keyboard_generation
                    {
                        return;
                    }
                    match result {
                        Ok(text) => {
                            // Window events execute outside APP's mutable borrow, just like physical typing.
                            dispatch_editor_key(&ui, text.into(), Vec::new(), true);
                        }
                        Err(error) => ui.set_auth_error(error.into()),
                    }
                }) {
                    eprintln!("editor paste delivery failed: {error}");
                }
            });
            if let Err(error) = result {
                ui.set_auth_error(error.into());
            }
            return;
        }
        if !ui.get_remote_input_allowed() {
            return;
        }
        if !self.input_ready {
            status(ui, "Terminal is not ready for paste");
            return;
        }
        let focus = self.endpoints.active.focus.clone();
        let session_id = self.endpoints.active.id;
        let epoch = self.epoch;
        let keyboard_generation = ui.global::<ui::KeyboardInteraction>().get_generation();
        let weak = ui.as_weak();
        let result = self.clipboard.request(false, INPUT_LIMIT, move |result| {
            if let Err(error) = weak.upgrade_in_event_loop(move |ui| {
                with_app(|app| {
                    if app.epoch != epoch
                        || app.endpoints.active.id != session_id
                        || app.endpoints.active.focus != focus
                        || !app.input_ready
                        || !ui.get_remote_input_allowed()
                        || ui.global::<ui::KeyboardInteraction>().get_generation()
                            != keyboard_generation
                    {
                        status(
                            &ui,
                            "Paste cancelled — connection or terminal focus changed",
                        );
                        return;
                    }
                    let result = result.and_then(|text| {
                        app.endpoints
                            .active
                            .client
                            .as_ref()
                            .ok_or("Not connected")?
                            .paste(focus.as_ref().ok_or("No confirmed focus")?, &text)
                    });
                    match result {
                        Ok(()) => status(&ui, "Paste queued"),
                        Err(error) => status(&ui, error),
                    }
                });
            }) {
                eprintln!("paste completion could not reach UI: {error}");
            }
        });
        if let Err(error) = result {
            status(ui, error);
        }
    }

    pub(crate) fn scroll(&mut self, ui: &AppWindow, up: bool, ticks: i32) {
        if !self.input_ready || ticks <= 0 {
            return;
        }
        let result = (|| -> Result<(), String> {
            let focus = self
                .endpoints
                .active
                .focus
                .as_ref()
                .ok_or("No confirmed terminal focus")?;
            let client = self
                .endpoints
                .active
                .client
                .as_ref()
                .ok_or("Not connected")?;
            let lines_per_tick = (u64::from(self.height.max(1)) + 2) / 3;
            // Child-owned mouse/alternate scrolling routes one wheel event per
            // request, regardless of lines. Never combine distinct ticks.
            for _ in 0..ticks {
                let mut remaining = lines_per_tick;
                // Every bounded request retains Client's focus/generation fence.
                while remaining > 0 {
                    let lines = remaining.min(1000) as u16;
                    client.scroll(focus, up, lines)?;
                    remaining -= u64::from(lines);
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            status(ui, error);
        }
    }

    pub(crate) fn terminal_click(&mut self, ui: &AppWindow, column: i32, row: i32) {
        self.selection = None;
        ui.set_selection_start(-1);
        ui.set_selection_end(-1);
        let column = column.clamp(0, i32::from(self.cols.saturating_sub(1))) as u16;
        let row = row.clamp(0, i32::from(self.height.saturating_sub(1))) as u16;
        let target = if let Some([width, height]) = self.endpoints.active.popup_size {
            let x = self.cols.saturating_sub(width) / 2;
            let y = self.height.saturating_sub(height) / 2;
            (column >= x
                && row >= y
                && column < x.saturating_add(width)
                && row < y.saturating_add(height))
            .then(|| {
                self.endpoints
                    .active
                    .focus
                    .as_ref()
                    .and_then(|focus| focus.pane_id.as_ref())
                    .map(|id| (id.clone(), column - x, row - y))
            })
            .flatten()
        } else {
            self.endpoints
                .active
                .surface_panes
                .iter()
                .find(|pane| {
                    let [x, y, width, height] = pane.inner;
                    column >= x
                        && row >= y
                        && column < x.saturating_add(width)
                        && row < y.saturating_add(height)
                })
                .map(|pane| {
                    let [x, y, ..] = pane.inner;
                    (pane.pane_id.clone(), column - x, row - y)
                })
        };
        let Some((pane, column, row)) = target else {
            return;
        };
        let result = (|| -> Result<bool, String> {
            let focus = self
                .endpoints
                .active
                .focus
                .as_ref()
                .ok_or("No confirmed terminal focus")?;
            self.endpoints
                .active
                .client
                .as_ref()
                .ok_or("Not connected")?
                .click(focus, &pane, column, row, false)
        })();
        match result {
            Ok(true) => self.navigation_result(ui, Ok(())),
            Ok(false) => {},
            Err(error) => status(ui, error),
        }
    }

    pub(crate) fn select(&mut self, ui: &AppWindow, col: i32, row: i32, start: bool) {
        let index = row.clamp(0, i32::from(self.height) - 1) as usize * usize::from(self.cols)
            + col.clamp(0, i32::from(self.cols) - 1) as usize;
        let anchor = if start {
            index
        } else {
            self.selection.map_or(index, |(anchor, _)| anchor)
        };
        self.selection = Some((anchor, index));
        ui.set_selection_start(anchor as i32);
        ui.set_selection_end(index as i32);
    }

    pub(crate) fn copy(&self, ui: &AppWindow) {
        if ui.get_local_editor_active() {
            if !ui.get_local_copy_allowed() {
                return;
            }
            ui.invoke_restore_editor_focus();
            let generation = ui.get_local_editor_generation();
            let editor_target = ui.get_editor_target();
            let auth_generation = ui.get_auth_generation();
            let weak = ui.as_weak();
            slint::Timer::single_shot(Duration::ZERO, move || {
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                if ui.get_application_active()
                    && ui.get_local_editor_active()
                    && ui.get_local_copy_allowed()
                    && ui.get_local_editor_generation() == generation
                    && ui.get_editor_target() == editor_target
                    && ui.get_auth_generation() == auth_generation
                {
                    dispatch_editor_key(
                        &ui,
                        "c".into(),
                        vec![slint::platform::Key::Control.into()],
                        true,
                    );
                }
            });
            return;
        }
        let Some((anchor, end)) = self.selection else {
            status(ui, "Select terminal text before copying");
            return;
        };
        let (start, end) = (anchor.min(end), anchor.max(end));
        let columns = usize::from(self.cols);
        let mut text = String::new();
        for row_index in start / columns..=end / columns {
            let Some(row) = self.presentation.rows.row_data(row_index) else {
                continue;
            };
            for cell in row.cells.iter() {
                let index = row_index * columns + cell.column as usize;
                if index <= end && index + cell.span.max(1) as usize > start {
                    if text.len() + cell.text.len() > INPUT_LIMIT {
                        status(ui, "Selection exceeds 512 KiB");
                        return;
                    }
                    text.push_str(&cell.text);
                }
            }
            if row_index != end / columns {
                while text.ends_with(' ') {
                    text.pop();
                }
                text.push('\n');
            }
        }
        if let Err(error) = self.clipboard.copy(text) {
            status(ui, error);
        }
    }
}
