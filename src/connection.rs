// SPDX-License-Identifier: GPL-3.0-or-later
use std::{collections::BTreeMap, fs::{self, File}, io::Read, path::{Path, PathBuf}};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    #[default]
    Key,
    Password,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    Shell,
    #[default]
    Herdr,
}

fn default_herdr_session() -> String { "default".into() }
fn default_herdr_binary() -> String { "herdr".into() }

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: String,
    pub user: Option<String>,
    pub identity: Option<String>,
    /// Legacy/advanced command only; the session backend chooses its transport command.
    pub remote_command: String,
    pub port: u16,
    pub keepalive: u32,
    pub compression: bool,
    #[serde(default)]
    pub auth_method: AuthMethod,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let mut bytes = Vec::new();
        File::open(path).map_err(|e| format!("Cannot open {}: {e}", path.display()))?
            .take(65537).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        if bytes.len() > 65536 { return Err("Connection configuration exceeds 64 KiB".into()); }
        let source = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
        let mut section = false;
        let mut values = BTreeMap::new();
        for (index, raw) in source.lines().enumerate() {
            let line = raw.trim_start_matches([' ', '\t']).trim_end_matches('\r');
            if line.is_empty() || line.starts_with('#') { continue; }
            if line.starts_with('[') {
                if section || line.trim_end_matches([' ', '\t']) != "[connection]" {
                    return Err("Configuration must contain exactly one [connection] section".into());
                }
                section = true;
                continue;
            }
            if !section { return Err(format!("Key outside [connection] on line {}", index + 1)); }
            let (key, value) = line.split_once('=').ok_or_else(|| format!("Invalid connection line {}", index + 1))?;
            let key = key.trim_end_matches([' ', '\t']);
            if !["host", "user", "port", "identity", "backend", "program", "command", "compression", "keepalive"].contains(&key) {
                return Err(format!("Unknown connection key {key:?}"));
            }
            // GKeyFile removes leading unescaped whitespace, but retains trailing spaces.
            let value = unescape(value.trim_start_matches([' ', '\t']))?;
            if values.insert(key, value).is_some() { return Err(format!("Repeated connection key {key:?}")); }
        }
        if !section { return Err("Missing [connection] section".into()); }
        Self::from_legacy_values(values)
    }

    fn validate_values<'a>(values: impl IntoIterator<Item = (&'a str, &'a str)>) -> Result<(), String> {
        for (key, value) in values {
            let limit = if key == "command" { 16384 } else { 4096 };
            if value.is_empty() || value.len() > limit || value.chars().any(|c| c < ' ' || c == '\u{7f}') {
                return Err(match key {
                    "port" => "Port: enter a whole number from 1 to 65535".into(),
                    "keepalive" => "Keepalive: enter seconds from 0 to 2147483647 (0 disables it)".into(),
                    _ => format!("{key}: enter 1–{limit} bytes of text without control characters"),
                });
            }
        }
        Ok(())
    }

    fn from_legacy_values(mut values: BTreeMap<&str, String>) -> Result<Self, String> {
        Self::validate_values(values.iter().map(|(&key, value)| (key, value.as_str())))?;
        let dropbear = match values.remove("backend").as_deref() {
            None | Some("openssh") => false,
            Some("dropbear") => true,
            _ => return Err("Backend: enter openssh or dropbear".into()),
        };
        let program = values.remove("program").unwrap_or_else(|| if dropbear { "dbclient" } else { "ssh" }.into());
        if program.starts_with('-') || (!Path::new(&program).is_absolute() &&
            !program.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-.+".contains(&b))) {
            return Err("Program: enter a command name such as dbclient, or a full executable path starting with /".into());
        }
        let config = Self::from_values(values)?;
        if dropbear && config.compression { return Err("Compression: turn it off when using Dropbear".into()); }
        Ok(config)
    }

    pub(crate) fn from_values(mut values: BTreeMap<&str, String>) -> Result<Self, String> {
        Self::validate_values(values.iter().map(|(&key, value)| (key, value.as_str())))?;
        let host = values.remove("host").ok_or("Host: enter the SSH server hostname or IP address")?;
        destination(&host, true)?;
        let user = values.remove("user");
        if let Some(user) = &user { destination(user, false)?; }
        let auth_method = match values.remove("auth_method").as_deref() {
            None | Some("key") => AuthMethod::Key,
            Some("password") => AuthMethod::Password,
            _ => return Err("Authentication: choose SSH key or password".into()),
        };
        let identity = values.remove("identity");
        if identity.as_ref().is_some_and(|p| !Path::new(p).is_absolute()) {
            return Err("Key file: enter the full path starting with /; ~ is not expanded".into());
        }
        let port = integer(values.remove("port"), 22, 1, 65535, "Port: enter a whole number from 1 to 65535")? as u16;
        let keepalive = integer(values.remove("keepalive"), 30, 0, i32::MAX as u32,
            "Keepalive: enter seconds from 0 to 2147483647 (0 disables it)")?;
        let compression_value = values.remove("compression");
        let compression = match compression_value.as_deref().map(|v| v.trim_end_matches([' ', '\t'])) {
            None | Some("false") | Some("0") => false,
            Some("true") | Some("1") => true,
            _ => return Err("Compression: enter true or false".into()),
        };
        Ok(Self { host, user, identity, port, keepalive, compression, auth_method,
            remote_command: values.remove("command").unwrap_or_default() })
    }

    fn validate(&self) -> Result<(), String> {
        Self::validate_values([
            ("host", self.host.as_str()), ("user", self.user.as_deref().unwrap_or_default()),
            ("identity", self.identity.as_deref().unwrap_or_default()), ("command", self.remote_command.as_str()),
        ].into_iter().filter(|(_, value)| !value.is_empty()))?;
        destination(&self.host, true)?;
        if let Some(user) = self.user.as_deref().filter(|user| !user.is_empty()) { destination(user, false)?; }
        if self.identity.as_deref().filter(|identity| !identity.is_empty()).is_some_and(|identity| !Path::new(identity).is_absolute()) {
            return Err("Key file: enter the full path starting with /; ~ is not expanded".into());
        }
        if self.port == 0 { return Err("Port: enter a whole number from 1 to 65535".into()); }
        if self.keepalive > i32::MAX as u32 { return Err("Keepalive: enter seconds from 0 to 2147483647 (0 disables it)".into()); }
        Ok(())
    }
    pub fn description(&self) -> String {
        match &self.user {
            Some(user) => format!("{user}@{}:{}", self.host, self.port),
            None => format!("{}:{}", self.host, self.port),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub config: Config,
    pub session_kind: SessionKind,
    #[serde(default = "default_herdr_session")]
    pub herdr_session: String,
    #[serde(default = "default_herdr_binary")]
    pub herdr_binary: String,
    #[serde(default)]
    pub remembered_herdr_sessions: Vec<String>,
}

impl Profile {
    fn migrated(id: String, name: String, config: Config) -> Self {
        Self { id, name, config, session_kind: SessionKind::Herdr,
            herdr_session: default_herdr_session(), herdr_binary: default_herdr_binary(),
            remembered_herdr_sessions: Vec::new() }
    }

}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionTwoProfile { id: String, name: String, config: Config }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionTwoSaved { version: u32, selected: Option<String>, profiles: Vec<VersionTwoProfile> }

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    version: u32,
    selected: Option<String>,
    profiles: Vec<Profile>,
}

// These types exist only at the schema-1 read boundary. Never serialize executable
// selection into the native configuration or relax old-store validation.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyConfig {
    host: String,
    user: Option<String>,
    identity: Option<String>,
    program: String,
    remote_command: String,
    port: u16,
    keepalive: u32,
    dropbear: bool,
    compression: bool,
}

impl LegacyConfig {
    fn migrate(self) -> Result<Config, String> {
        let mut values = BTreeMap::from([
            ("host", self.host), ("program", self.program), ("command", self.remote_command),
            ("port", self.port.to_string()), ("keepalive", self.keepalive.to_string()),
            ("backend", if self.dropbear { "dropbear" } else { "openssh" }.into()),
            ("compression", self.compression.to_string()),
        ]);
        if let Some(user) = self.user { values.insert("user", user); }
        if let Some(identity) = self.identity { values.insert("identity", identity); }
        Config::from_legacy_values(values)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyProfile {
    id: String,
    name: String,
    config: LegacyConfig,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacySaved {
    version: u32,
    selected: Option<String>,
    profiles: Vec<LegacyProfile>,
}

impl Saved {
    fn decode(bytes: &[u8]) -> Result<Self, String> {
        #[derive(Deserialize)]
        struct Version { version: u32 }
        let invalid = |error| format!("Invalid connections store: {error}");
        let version: Version = serde_json::from_slice(bytes).map_err(invalid)?;
        match version.version {
            1 => {
                let legacy: LegacySaved = serde_json::from_slice(bytes).map_err(invalid)?;
                if legacy.version != 1 || legacy.profiles.len() > 32 {
                    return Err("Unsupported store version or more than 32 connections".into());
                }
                let profiles = legacy.profiles.into_iter().map(|profile| {
                    Ok(Profile::migrated(profile.id, profile.name, profile.config.migrate()?))
                }).collect::<Result<Vec<_>, String>>()?;
                Ok(Self { version: 4, selected: legacy.selected, profiles })
            }
            2 => {
                let old: VersionTwoSaved = serde_json::from_slice(bytes).map_err(invalid)?;
                if old.version != 2 || old.profiles.len() > 32 {
                    return Err("Unsupported store version or more than 32 connections".into());
                }
                for profile in &old.profiles {
                    Config::validate_values([("command", profile.config.remote_command.as_str())])?;
                }
                Ok(Self { version: 4, selected: old.selected, profiles: old.profiles.into_iter()
                    .map(|p| Profile::migrated(p.id, p.name, p.config)).collect() })
            }
            3 => {
                let mut saved: Self = serde_json::from_slice(bytes).map_err(invalid)?;
                saved.version = 4;
                Ok(saved)
            }
            4 => serde_json::from_slice(bytes).map_err(invalid),
            _ => Err("Unsupported store version or more than 32 connections".into()),
        }
    }
}

pub struct Store {
    path: PathBuf,
    saved: Saved,
    pub error: Option<String>,
}

impl Store {
    pub fn load(legacy: &Path) -> Self {
        let mut store = Self { path: legacy.with_file_name("connections.json"),
            saved: Saved { version: 4, ..Default::default() }, error: None };
        let result = (|| -> Result<(), String> {
            match File::open(&store.path) {
                Ok(file) => {
                    let mut bytes = Vec::new();
                    file.take(1_048_577).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
                    if bytes.len() > 1_048_576 { return Err("Connections store exceeds 1 MiB".into()); }
                    let saved = Saved::decode(&bytes)?;
                    Self::validate(&saved)?;
                    store.saved = saved;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    match fs::metadata(legacy) {
                        Ok(_) => {
                            let config = Config::load(legacy)?;
                            let profile = Profile::migrated("legacy".into(), config.description(), config);
                            let saved = Saved { version: 4, selected: Some(profile.id.clone()), profiles: vec![profile] };
                            Self::validate(&saved)?;
                            store.saved = saved;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(format!("Cannot inspect legacy configuration: {error}")),
                    }
                }
                Err(error) => return Err(format!("Cannot read connections store: {error}")),
            }
            Ok(())
        })();
        if let Err(error) = result {
            store.error = Some(format!("{error}. Fix {} (or the legacy configuration) and restart; no files were replaced.", store.path.display()));
        }
        store
    }

    fn validate(saved: &Saved) -> Result<(), String> {
        if saved.version != 4 || saved.profiles.len() > 32 { return Err("Unsupported store version or more than 32 connections".into()); }
        let mut ids = std::collections::HashSet::new();
        for profile in &saved.profiles {
            if profile.id.is_empty() || profile.id.len() > 80 || !profile.id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') || !ids.insert(profile.id.as_str()) {
                return Err("Invalid or duplicate connection ID".into());
            }
            if profile.name.trim().is_empty() || profile.name.len() > 128 || profile.name.chars().any(char::is_control) {
                return Err("Connection name must contain 1–128 bytes of text".into());
            }
            profile.config.validate()?;
            session_preferences(&profile.herdr_session, &profile.herdr_binary)?;
            if profile.remembered_herdr_sessions.len() > 256 {
                return Err("A host cannot remember more than 256 Herdr sessions".into());
            }
            let mut sessions = std::collections::HashSet::new();
            for session in &profile.remembered_herdr_sessions {
                session_preferences(session, &profile.herdr_binary)?;
                if !sessions.insert(session.as_str()) { return Err("A host has duplicate remembered Herdr sessions".into()); }
            }
        }
        if saved.selected.as_ref().is_some_and(|id| !ids.contains(id.as_str())) || (saved.selected.is_none() && !saved.profiles.is_empty()) {
            return Err("Selected connection does not exist".into());
        }
        Ok(())
    }

    fn commit(&mut self, saved: Saved) -> Result<(), String> {
        if let Some(error) = &self.error { return Err(error.clone()); }
        Self::validate(&saved)?;
        let bytes = serde_json::to_vec_pretty(&saved).map_err(|e| e.to_string())?;
        if bytes.len() > 1_048_576 { return Err("Connections store exceeds 1 MiB".into()); }
        crate::atomic_file::replace(&self.path, &bytes, 1_048_576, crate::atomic_file::DirectorySync::Required)?;
        // Rename is the commit point: never claim the old state survived after it.
        self.saved = saved;
        Ok(())
    }

    pub fn profiles(&self) -> &[Profile] { &self.saved.profiles }
    pub fn selected(&self) -> Option<&Profile> { self.saved.profiles.iter().find(|p| Some(&p.id) == self.saved.selected.as_ref()) }
    pub fn profile(&self, id: &str) -> Result<&Profile, String> {
        if let Some(error) = &self.error { return Err(error.clone()); }
        self.saved.profiles.iter().find(|p| p.id == id).ok_or_else(|| "Connection no longer exists".into())
    }
    /// Atomically save SSH settings and session preferences. Empty ID creates a host;
    /// otherwise update that host. Selection does not represent a runtime session.
    pub fn save(&mut self, mut profile: Profile) -> Result<String, String> {
        if profile.name.trim().is_empty() { profile.name = profile.config.description(); }
        let mut saved = self.saved.clone();
        let id;
        if profile.id.is_empty() {
            id = (1u64..).map(|n| format!("connection-{n}")).find(|id| !saved.profiles.iter().any(|p| &p.id == id)).unwrap();
            if saved.selected.is_none() { saved.selected = Some(id.clone()); }
            profile.id = id.clone();
            profile.remembered_herdr_sessions.clear();
            saved.profiles.push(profile);
        } else {
            id = profile.id.clone();
            let existing = saved.profiles.iter_mut().find(|p| p.id == profile.id).ok_or("Connection no longer exists")?;
            profile.remembered_herdr_sessions = std::mem::take(&mut existing.remembered_herdr_sessions);
            *existing = profile;
        }
        self.commit(saved)?;
        Ok(id)
    }
    pub fn select(&mut self, id: &str) -> Result<Profile, String> {
        let profile = self.profile(id)?.clone();
        if self.saved.selected.as_deref() == Some(id) { return Ok(profile); }
        let mut saved = self.saved.clone(); saved.selected = Some(id.into());
        self.commit(saved)?;
        Ok(profile)
    }
    pub fn remember_sessions(&mut self, id: &str, names: &[String]) -> Result<(), String> {
        let mut saved = self.saved.clone();
        let profile = saved.profiles.iter_mut().find(|profile| profile.id == id).ok_or("Connection no longer exists")?;
        let mut changed = false;
        for name in names {
            session_preferences(name, &profile.herdr_binary)?;
            if !profile.remembered_herdr_sessions.contains(name) { profile.remembered_herdr_sessions.push(name.clone()); changed = true; }
        }
        if !changed { return Ok(()); }
        profile.remembered_herdr_sessions.sort();
        self.commit(saved)
    }
    pub fn forget_session(&mut self, id: &str, name: &str) -> Result<(), String> {
        let mut saved = self.saved.clone();
        let profile = saved.profiles.iter_mut().find(|profile| profile.id == id).ok_or("Connection no longer exists")?;
        let before = profile.remembered_herdr_sessions.len();
        profile.remembered_herdr_sessions.retain(|session| session != name);
        if profile.remembered_herdr_sessions.len() == before { return Ok(()); }
        self.commit(saved)
    }
    pub fn remove(&mut self, id: &str) -> Result<(), String> {
        self.profile(id)?;
        let mut saved = self.saved.clone();
        saved.profiles.retain(|p| p.id != id);
        if saved.selected.as_deref() == Some(id) { saved.selected = saved.profiles.first().map(|p| p.id.clone()); }
        self.commit(saved)
    }
}

fn session_preferences(session: &str, binary: &str) -> Result<(), String> {
    if session.trim().is_empty() || session.len() > 128 || session.chars().any(char::is_control) {
        return Err("Herdr session: enter 1–128 bytes without control characters".into());
    }
    if binary.is_empty() || binary.len() > 4096 || binary.chars().any(char::is_control) ||
        (!Path::new(binary).is_absolute() && !binary.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-.+".contains(&b))) ||
        binary.starts_with('-') {
        return Err("Herdr binary: enter a command name or an absolute executable path, not a command line".into());
    }
    Ok(())
}

fn destination(value: &str, host: bool) -> Result<(), String> {
    if value.is_empty() || value.len() > 255 || value.starts_with('-') || !value.bytes().all(|b|
        b.is_ascii_alphanumeric() || b"_-.".contains(&b) ||
        (host && b":[]%".contains(&b)) || (!host && b == b'$')) {
        return Err(if host {
            "Host: enter a hostname or IP address, not a URL or user@host; no spaces or leading - (255 bytes maximum)"
        } else {
            "User: use letters, digits, _, -, . or $; no spaces or leading - (255 bytes maximum)"
        }.into());
    }
    Ok(())
}

fn integer(value: Option<String>, default: u32, min: u32, max: u32, error: &str) -> Result<u32, String> {
    let Some(value) = value else { return Ok(default); };
    let text = value.trim_end_matches([' ', '\t']);
    let digits = text.strip_prefix('+').unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) { return Err(error.into()); }
    let number = digits.parse::<u32>().map_err(|_| error.to_string())?;
    if !(min..=max).contains(&number) { return Err(error.into()); }
    Ok(number)
}

fn unescape(value: &str) -> Result<String, String> {
    let mut result = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' { result.push(c); continue; }
        result.push(match chars.next() {
            Some('s') => ' ', Some('n') => '\n', Some('t') => '\t', Some('r') => '\r', Some('\\') => '\\',
            _ => return Err("Invalid GKeyFile string escape".into()),
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn remembered_sessions_survive_migration_profile_edits_and_reload() {
        let root = std::env::temp_dir().join(format!("kherdr-connections-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&root).unwrap();
        let legacy = root.join("connection.ini");
        let path = root.join("connections.json");
        let version_three = serde_json::json!({
            "version": 3,
            "selected": "host-1",
            "profiles": [{
                "id": "host-1", "name": "Host", "session_kind": "herdr",
                "herdr_session": "default", "herdr_binary": "herdr",
                "config": { "host": "example.test", "user": null, "identity": null,
                    "remote_command": "", "port": 22, "keepalive": 30,
                    "compression": false, "auth_method": "key" }
            }]
        });
        fs::write(&path, serde_json::to_vec(&version_three).unwrap()).unwrap();

        let mut store = Store::load(&legacy);
        assert!(store.error.is_none());
        store.remember_sessions("host-1", &["work".into(), "default".into(), "work".into()]).unwrap();
        let mut profile = store.profile("host-1").unwrap().clone();
        profile.name = "Renamed host".into();
        store.save(profile).unwrap();

        let mut reloaded = Store::load(&legacy);
        assert_eq!(reloaded.profile("host-1").unwrap().remembered_herdr_sessions, ["default", "work"]);
        reloaded.forget_session("host-1", "default").unwrap();
        assert_eq!(Store::load(&legacy).profile("host-1").unwrap().remembered_herdr_sessions, ["work"]);

        fs::remove_dir_all(root).unwrap();
    }
}
