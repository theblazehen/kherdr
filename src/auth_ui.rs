// SPDX-License-Identifier: GPL-3.0-or-later
use std::{path::{Path, PathBuf}, thread};
use slint::{ComponentHandle, ModelRc, VecModel};
use zeroize::Zeroizing;
use crate::{App, auth::{Answer, Prompt, PromptKind}, credentials::KeyStore, trust, ui, with_app};

impl App {
    pub(crate) fn key_model(&self, window: &ui::AppWindow) {
        window.set_key_entries(ModelRc::new(VecModel::from(self.keys.entries().iter().map(|entry| ui::KeyView {
            id: entry.id.clone().into(), name: entry.name.clone().into(), path: entry.path.to_string_lossy().into_owned().into(),
            detail: if entry.encrypted { "Passphrase protected" } else { "No passphrase" }.into(),
        }).collect::<Vec<_>>())));
        let identity = window.get_connection_draft().identity;
        window.set_selected_key_name(self.keys.entries().iter()
            .find(|entry| entry.path == Path::new(identity.as_str()))
            .map(|entry| entry.name.clone()).unwrap_or_else(|| identity.to_string()).into());
        if let Some(error) = &self.keys.error { window.set_auth_error(error.clone().into()); }
    }

    pub(crate) fn open_auth_manager(&mut self, window: &ui::AppWindow) {
        if window.get_auth_busy() || window.get_auth_prompt_visible() { return; }
        self.cancel_interaction(window);
        self.keys = KeyStore::load(&self.auth_root);
        window.set_auth_error("".into());
        self.key_model(window);
        window.invoke_navigate(if window.get_key_selection() { ui::Route::KeyPicker } else { ui::Route::Keys });
    }

    pub(crate) fn create_key(&mut self, window: &ui::AppWindow) {
        if window.get_auth_busy() { return; }
        self.cancel_interaction(window);
        window.set_key_id("".into()); window.set_key_name("".into());
        window.invoke_change_key_source("generate".into()); window.set_key_material("".into());
        window.set_key_passphrase("".into()); window.set_key_path("".into());
        window.set_auth_error("".into()); window.invoke_navigate(ui::Route::KeyEditor);
    }

    pub(crate) fn view_key(&mut self, window: &ui::AppWindow, id: &str, edit: bool) {
        if window.get_auth_busy() { return; }
        let Some(entry) = self.keys.entries().iter().find(|entry| entry.id == id).cloned() else {
            window.set_auth_error("This key is no longer available. Reopen Keys & trust.".into()); return;
        };
        self.cancel_interaction(window);
        window.set_key_id(entry.id.into()); window.set_key_name(entry.name.into());
        window.set_key_path(entry.path.to_string_lossy().into_owned().into());
        window.set_key_public_key(entry.public_key.into()); window.set_key_fingerprint(entry.fingerprint.into());
        window.set_key_material("".into()); window.set_key_passphrase("".into());
        window.set_auth_error("".into()); window.invoke_navigate(if edit { ui::Route::KeyEditor } else { ui::Route::KeyDetails });
    }

    pub(crate) fn choose_key(&mut self, window: &ui::AppWindow, path: &str) {
        if window.get_auth_busy() || window.get_route() != ui::Route::KeyPicker { return; }
        if !self.keys.entries().iter().any(|entry| entry.path == Path::new(path)) {
            window.set_auth_error("Choose an available key from Keys & trust.".into()); return;
        }
        self.cancel_interaction(window);
        let mut draft = window.get_connection_draft();
        draft.auth_method = "key".into(); draft.identity = path.into();
        window.set_connection_draft(draft); window.invoke_navigate(ui::Route::Connection);
        self.key_model(window);
        window.set_key_material("".into()); window.set_key_passphrase("".into());
        window.set_auth_error("".into());
        window.invoke_restore_editor_focus();
    }

    pub(crate) fn save_key(&mut self, window: &ui::AppWindow) {
        if window.get_auth_busy() || window.get_route() != ui::Route::KeyEditor { return; }
        let id = window.get_key_id().to_string();
        let name = window.get_key_name().to_string();
        let source = window.get_key_source().to_string();
        let path = PathBuf::from(window.get_key_path().as_str());
        let material = Zeroizing::new(window.get_key_material().to_string());
        let passphrase = Zeroizing::new(window.get_key_passphrase().to_string());
        window.set_key_material("".into()); window.set_key_passphrase("".into());
        self.start_key_job(window, move |store| {
            if !id.is_empty() { store.rename(&id, &name)?; return Ok(Some(id)); }
            let passphrase = (!passphrase.is_empty()).then_some(passphrase.as_str());
            let entry = match source.as_str() {
                "generate" => store.generate(&name, passphrase),
                "file" => store.import_file(&name, &path, passphrase),
                "paste" => store.import_text(&name, &material, passphrase),
                _ => Err("Choose Generate, Import file or Paste key.".into()),
            }?;
            Ok(Some(entry.id))
        });
    }

    pub(crate) fn delete_key(&mut self, window: &ui::AppWindow, id: &str) {
        if window.get_auth_busy() { return; }
        if self.discovery_pending { window.set_auth_error("Wait for local SSH controls to finish reattaching before removing a key.".into()); return; }
        let id = id.to_owned();
        let mut used: Vec<PathBuf> = self.connections.profiles().iter().filter_map(|profile|
            profile.config.identity.as_ref().map(PathBuf::from)).collect();
        used.extend(self.connection_key_paths());
        self.start_key_job(window, move |store| { store.remove(&id, &used)?; Ok(None) });
    }

    fn start_key_job(&mut self, window: &ui::AppWindow,
        operation: impl FnOnce(&mut KeyStore) -> Result<Option<String>, String> + Send + 'static) {
        self.key_job = self.key_job.wrapping_add(1);
        let job = self.key_job;
        let generation = window.get_auth_generation();
        let root = self.auth_root.clone();
        let weak = window.as_weak();
        window.set_auth_busy(true); window.set_auth_error("".into());
        let started = thread::Builder::new().name("ssh-key-manager".into()).spawn(move || {
            let mut store = KeyStore::load(&root);
            let result = match &store.error { Some(error) => Err(error.clone()), None => operation(&mut store) };
            if let Err(error) = weak.upgrade_in_event_loop(move |window| {
                with_app(|app| {
                    if app.key_job != job { return; }
                    app.keys = store;
                    window.set_auth_busy(false);
                    app.key_model(&window);
                    if window.get_auth_generation() != generation || window.get_auth_prompt_visible() { return; }
                    match result {
                        Ok(Some(id)) => {
                            if window.get_key_selection() {
                                window.invoke_navigate(ui::Route::KeyPicker);
                            } else { app.view_key(&window, &id, false); }
                        },
                        Ok(None) => window.invoke_navigate(ui::Route::Keys),
                        Err(error) => { window.set_auth_error(error.into()); window.invoke_restore_editor_focus(); },
                    }
                });
            }) { eprintln!("Key-manager completion could not reach UI: {error}"); }
        });
        if let Err(error) = started {
            window.set_auth_busy(false);
            window.set_auth_error(format!("Cannot start key operation: {error}").into());
        }
    }

    pub(crate) fn copy_public_key(&self, window: &ui::AppWindow) {
        let id = window.get_key_id();
        let Some(entry) = self.keys.entries().iter().find(|entry| entry.id == id.as_str()) else {
            window.set_auth_error("Choose an available key first.".into()); return;
        };
        match self.clipboard.copy(entry.public_key.clone()) {
            Ok(()) => window.set_auth_error("Public key copied. Add it to the server account's authorized_keys.".into()),
            Err(error) => window.set_auth_error(error.into()),
        }
    }

    pub(crate) fn browse_key_files(&mut self, window: &ui::AppWindow, path: &str) {
        if window.get_auth_busy() { return; }
        let initial = if path.is_empty() {
            if Path::new("/mnt/us").is_dir() { PathBuf::from("/mnt/us") } else { self.auth_root.clone() }
        } else { PathBuf::from(path) };
        let result = (|| -> Result<(PathBuf, Vec<ui::FileView>), String> {
            let directory = initial.canonicalize().map_err(|error| format!("Cannot open folder: {error}"))?;
            let mut entries = Vec::new();
            for entry in std::fs::read_dir(&directory).map_err(|error| format!("Cannot list folder: {error}"))? {
                let entry = entry.map_err(|error| format!("Cannot read folder entry: {error}"))?;
                let path = entry.path();
                let metadata = match path.metadata() {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(format!("Cannot inspect {}: {error}", path.display())),
                };
                if !metadata.is_dir() && !metadata.is_file() { continue; }
                if entries.len() == 1000 { return Err("This folder contains more than 1,000 items. Choose a smaller folder.".into()); }
                entries.push(ui::FileView { path: path.to_string_lossy().into_owned().into(),
                    name: entry.file_name().to_string_lossy().into_owned().into(), directory: metadata.is_dir() });
            }
            entries.sort_by(|a, b| b.directory.cmp(&a.directory).then_with(|| a.name.as_str().cmp(b.name.as_str())));
            if let Some(parent) = directory.parent() {
                entries.insert(0, ui::FileView { path: parent.to_string_lossy().into_owned().into(), name: ".. (parent folder)".into(), directory: true });
            }
            Ok((directory, entries))
        })();
        match result {
            Ok((directory, entries)) => {
                self.cancel_interaction(window);
                window.set_key_directory(directory.to_string_lossy().into_owned().into());
                window.set_key_files(ModelRc::new(VecModel::from(entries)));
                window.set_auth_error("".into()); window.invoke_navigate(ui::Route::KeyFiles);
            }
            Err(error) => window.set_auth_error(error.into()),
        }
    }

    pub(crate) fn choose_key_file(&mut self, window: &ui::AppWindow, path: &str) {
        if window.get_auth_busy() { return; }
        match std::fs::metadata(path) {
            Ok(metadata) if metadata.is_file() && metadata.len() <= crate::credentials::MAX_KEY as u64 => {
                self.cancel_interaction(window);
                window.invoke_change_key_source("file".into()); window.set_key_path(path.into());
                window.invoke_navigate(ui::Route::KeyEditor); window.set_auth_error("".into());
            }
            Ok(_) => window.set_auth_error(format!("Choose a private-key file no larger than {} KiB.", crate::credentials::MAX_KEY / 1024).into()),
            Err(error) => window.set_auth_error(format!("Cannot open key file: {error}").into()),
        }
    }

    pub(crate) fn open_trusted_hosts(&mut self, window: &ui::AppWindow) {
        if window.get_auth_busy() { return; }
        self.cancel_interaction(window);
        window.invoke_navigate(ui::Route::TrustedHosts); window.set_auth_error("".into());
        match trust::entries(&self.auth_root.join(".ssh/known_hosts")) {
            Ok(entries) => window.set_trusted_hosts(ModelRc::new(VecModel::from(entries.into_iter().map(|entry|
                ui::TrustedHostView { id: entry.id.into(), host: entry.host.into(), algorithm: entry.algorithm.into(), fingerprint: entry.fingerprint.into() }).collect::<Vec<_>>()))),
            Err(error) => { window.set_trusted_hosts(ModelRc::default()); window.set_auth_error(error.into()); }
        }
    }

    pub(crate) fn remove_trusted_host(&mut self, window: &ui::AppWindow, id: &str) {
        if window.get_auth_busy() { return; }
        match trust::remove(&self.auth_root.join(".ssh/known_hosts"), id) {
            Ok(()) => self.open_trusted_hosts(window),
            Err(error) => window.set_auth_error(error.into()),
        }
    }

    pub(crate) fn authentication_prompt(&mut self, window: &ui::AppWindow, prompt: Prompt, owner: crate::PromptOwner) {
        self.cancel_interaction(window);
        self.auth_prompt = Some(prompt.id);
        self.auth_owner = Some(owner);
        window.set_auth_secret("".into());
        window.set_auth_remember_password(false);
        window.set_auth_password_saving_allowed(prompt.kind == PromptKind::Password);
        let kind = match prompt.kind { PromptKind::NewHost => 0, PromptKind::ChangedHost => 1,
            PromptKind::Password => 2, PromptKind::Passphrase => 3 };
        window.set_auth_prompt_title(prompt.title.into()); window.set_auth_prompt_detail(prompt.detail.into());
        window.set_auth_fingerprint(prompt.fingerprint.into()); window.set_auth_previous_fingerprint(prompt.previous_fingerprint.into());
        window.set_auth_error("".into()); window.invoke_present_auth_prompt(kind);
    }

    pub(crate) fn answer_auth(&mut self, window: &ui::AppWindow, approved: bool) {
        let Some(id) = self.auth_prompt.take() else { return; };
        let secret = Zeroizing::new(window.get_auth_secret().to_string());
        let remember_password = approved && window.get_auth_password_saving_allowed() && window.get_auth_remember_password();
        window.set_auth_remember_password(false); window.set_auth_password_saving_allowed(false);
        window.set_auth_secret("".into()); window.invoke_hide_auth_prompt();
        self.cancel_interaction(window);
        let answer = Answer { id, approved, remember_password, secret };
        self.answer_connection_auth(window, answer);
    }
}

pub(crate) fn register(window: &ui::AppWindow) {
    let weak = window.as_weak();
    window.on_forget_saved_passwords(move || { if let Some(window) = weak.upgrade() { with_app(|app| {
        if window.get_auth_busy() { return; }
        match crate::credentials::PasswordStore::forget_all(&app.auth_root) {
            Ok(()) => window.set_auth_error("Saved passwords forgotten. Existing connections remain open.".into()),
            Err(error) => window.set_auth_error(error.into()),
        }
    }); } });
    let weak = window.as_weak();
    window.on_open_auth_manager(move || { if let Some(window) = weak.upgrade() { with_app(|app| app.open_auth_manager(&window)); } });
    let weak = window.as_weak();
    window.on_create_key(move || { if let Some(window) = weak.upgrade() { with_app(|app| app.create_key(&window)); } });
    let weak = window.as_weak();
    window.on_edit_key(move |id| { if let Some(window) = weak.upgrade() { with_app(|app| app.view_key(&window, &id, true)); } });
    let weak = window.as_weak();
    window.on_view_key(move |id| { if let Some(window) = weak.upgrade() { with_app(|app| app.view_key(&window, &id, false)); } });
    let weak = window.as_weak();
    window.on_save_key(move || { if let Some(window) = weak.upgrade() { with_app(|app| app.save_key(&window)); } });
    let weak = window.as_weak();
    window.on_delete_key(move |id| { if let Some(window) = weak.upgrade() { with_app(|app| app.delete_key(&window, &id)); } });
    let weak = window.as_weak();
    window.on_copy_public_key(move || { if let Some(window) = weak.upgrade() { with_app(|app| app.copy_public_key(&window)); } });
    let weak = window.as_weak();
    window.on_copy_fingerprint(move || { if let Some(window) = weak.upgrade() {
        if window.get_auth_prompt_visible() && window.get_auth_prompt_kind() < 2 {
            with_app(|app| {
                let result = app.clipboard.copy(window.get_auth_fingerprint().to_string());
                window.set_auth_error(match result { Ok(()) => "Fingerprint copied.".into(), Err(error) => error.into() });
            });
        }
    } });
    let weak = window.as_weak();
    window.on_choose_key(move |path| { if let Some(window) = weak.upgrade() { with_app(|app| app.choose_key(&window, &path)); } });
    let weak = window.as_weak();
    window.on_browse_key_files(move |path| { if let Some(window) = weak.upgrade() { with_app(|app| app.browse_key_files(&window, &path)); } });
    let weak = window.as_weak();
    window.on_choose_key_file(move |path| { if let Some(window) = weak.upgrade() { with_app(|app| app.choose_key_file(&window, &path)); } });
    let weak = window.as_weak();
    window.on_open_trusted_hosts(move || { if let Some(window) = weak.upgrade() { with_app(|app| app.open_trusted_hosts(&window)); } });
    let weak = window.as_weak();
    window.on_remove_trusted_host(move |id| { if let Some(window) = weak.upgrade() { with_app(|app| app.remove_trusted_host(&window, &id)); } });
    let weak = window.as_weak();
    window.on_answer_auth(move |approved| { if let Some(window) = weak.upgrade() { with_app(|app| app.answer_auth(&window, approved)); } });
}
