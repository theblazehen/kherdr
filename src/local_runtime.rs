// SPDX-License-Identifier: GPL-3.0-or-later
use crate::{credentials, endpoint, shell};
use serde_json::json;
use std::{fs::{self, OpenOptions}, io, os::unix::{fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt}, net::UnixStream, process::CommandExt},
    path::{Path, PathBuf}, process::{Command, Stdio}, thread};

const DEFAULT_CONFIG: &[u8] = b"[terminal]\ndefault_shell = \"/bin/sh\"\nshell_mode = \"non_login\"\n[server]\nheadless_cols = 80\nheadless_rows = 40\n[update]\nversion_check = false\nmanifest_check = false\n";

/// Stable per-installation sockets live on /tmp, not the Kindle's FAT volume.
/// Dropping this handle never stops a server or any of its PTY children.
pub struct Runtime {
    pub api_socket: PathBuf,
    pub endpoint_socket: PathBuf,
    pub panes_root: PathBuf,
    directory: PathBuf,
    config_home: PathBuf,
    executable: PathBuf,
    binary: PathBuf,
    cwd: PathBuf,
}

impl Runtime {
    pub fn new(auth_root: &Path) -> Result<Self, String> {
        fs::create_dir_all(auth_root).map_err(|error| format!("Cannot create app configuration directory: {error}"))?;
        let root = auth_root.canonicalize().map_err(|error| format!("Cannot resolve app configuration directory: {error}"))?;
        let identity = crate::state::runtime_id(&root)?;
        let directory = PathBuf::from(format!("/tmp/kherdr-{identity}"));
        private_directory(&directory)?;
        let panes_root = directory.join("ssh");
        private_directory(&panes_root)?;
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let binary = executable.with_file_name("herdr");
        let config_home = root.join("local-herdr");
        let config = config_home.join("herdr/config.toml");
        if !config.exists() {
            let _lock = credentials::StoreLock::acquire(&config)?;
            if !config.exists() { crate::atomic_file::replace(&config, DEFAULT_CONFIG, 4096, crate::atomic_file::DirectorySync::BestEffort)?; }
        }
        let cwd = if Path::new("/mnt/us").is_dir() { PathBuf::from("/mnt/us") }
            else { std::env::current_dir().map_err(|error| error.to_string())? };
        Ok(Self { api_socket: directory.join("herdr.sock"), endpoint_socket: directory.join("herdr-client.sock"),
            panes_root, directory, config_home, executable, binary, cwd })
    }

    /// Reuse a live server. Only stock Herdr may remove its stale socket; never
    /// restart an existing server just because a GUI is opening or reconnecting.
    pub fn ensure_started(&self) -> Result<(), String> {
        if self.listening()? { return Ok(()); }
        let _lock = credentials::StoreLock::acquire(&self.directory.join("server-start"))?;
        if self.listening()? { return Ok(()); }
        if !self.binary.is_file() {
            return Err(format!("Bundled local Herdr is missing: {}. Install the complete kherdr package.", self.binary.display()));
        }
        let log = OpenOptions::new().create(true).append(true).mode(0o600)
            .open(self.config_home.join("server.log")).map_err(|error| format!("Cannot open local Herdr log: {error}"))?;
        let errors = log.try_clone().map_err(|error| error.to_string())?;
        let mut command = Command::new(&self.binary);
        command.arg("server").current_dir(&self.cwd)
            .env("HERDR_SOCKET_PATH", &self.api_socket)
            .env("HERDR_CLIENT_SOCKET_PATH", &self.endpoint_socket)
            .env("HERDR_STARTUP_CWD", &self.cwd)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env("SHELL", "/bin/sh")
            .stdin(Stdio::null()).stdout(Stdio::from(log)).stderr(Stdio::from(errors));
        for name in ["HERDR_SESSION", "HERDR_PANE_ID", "HERDR_WORKSPACE_ID", "HERDR_TAB_ID",
            "TERM_PROGRAM", "TERM_PROGRAM_VERSION", "TERM_SESSION_ID", "KITTY_WINDOW_ID", "KITTY_PID",
            "KITTY_LISTEN_ON", "GHOSTTY_RESOURCES_DIR", "GHOSTTY_BIN_DIR", "GHOSTTY_SHELL_INTEGRATION",
            "ITERM_SESSION_ID", "ITERM_PROFILE", "WEZTERM_PANE", "WEZTERM_UNIX_SOCKET", "WT_SESSION", "VTE_VERSION"] {
            command.env_remove(name);
        }
        // Only async-signal-safe libc work occurs between fork and exec. The
        // server must not inherit the GUI launcher's controlling session.
        unsafe { command.pre_exec(|| {
            if libc::setsid() == -1 { Err(io::Error::last_os_error()) } else { Ok(()) }
        }); }
        let mut child = command.spawn().map_err(|error| format!("Cannot start bundled local Herdr: {error}"))?;
        let pid = child.id();
        thread::Builder::new().name("local-herdr-reaper".into()).spawn(move || {
            match child.wait() {
                Ok(status) if !status.success() => eprintln!("Local Herdr {pid} exited: {status}"),
                Err(error) => eprintln!("Cannot reap local Herdr {pid}: {error}"),
                _ => {},
            }
        }).map_err(|error| format!("Local Herdr started but its process monitor could not start: {error}"))?;
        Ok(())
    }

    fn listening(&self) -> Result<bool, String> {
        match UnixStream::connect(&self.api_socket) {
            Ok(_) => Ok(true),
            Err(error) if matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused) => Ok(false),
            Err(error) => Err(format!("Cannot inspect local Herdr socket: {error}")),
        }
    }

    pub fn launch_ssh(&self, prepared: &shell::Prepared, focus: bool) -> Result<(), String> {
        let executable = self.executable.to_str().ok_or("The native executable path is not valid UTF-8")?;
        let descriptor = prepared.descriptor.to_str().ok_or("The SSH descriptor path is not valid UTF-8")?;
        let cwd = self.cwd.to_str().ok_or("The local shell directory is not valid UTF-8")?;
        // Omitting tab_id creates a new tab. Never replace a user's existing
        // terminal or type shell commands into an already-running shell.
        // Kindle's fsp mount rejects access(X_OK) despite permitting execve.
        // Let stock Herdr validate /bin/sh, then exec the helper with positional
        // arguments: neither executable nor descriptor is parsed as shell text.
        endpoint::api_request(&self.api_socket, "layout.apply", json!({
            "tab_label": format!("{} · SSH", prepared.profile.name), "focus": focus,
            "root": {"type":"pane", "label":prepared.profile.name, "cwd":cwd,
                "command":["/bin/sh", "-c", "exec \"$@\"", "kherdr-ssh-pane", executable, "--ssh-pane", descriptor],
                "env":{"NO_COLOR":"1", "CLICOLOR":"0", "FORCE_COLOR":"0"}}
        }))?;
        Ok(())
    }
}

fn private_directory(path: &Path) -> Result<(), String> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {},
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {},
        Err(error) => return Err(format!("Cannot create private runtime directory {}: {error}", path.display())),
    }
    let directory = OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path).map_err(|error| format!("Cannot open private runtime directory {}: {error}", path.display()))?;
    let metadata = directory.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(format!("Runtime directory is not a private owned directory: {}", path.display()));
    }
    // Kindle framework startup recursively grants javausers access to /var/tmp
    // (also /tmp). Restore our own directory without following a substituted
    // symlink or touching another owner's runtime; keep live sockets intact.
    if metadata.mode() & 0o7777 != 0o700 {
        directory.set_permissions(fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("Cannot restore private runtime directory {}: {error}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::{fs::symlink, net::UnixListener};

    #[test]
    fn restores_framework_permissions_without_discarding_live_socket() {
        let root = std::env::temp_dir().join(format!("kherdr-runtime-{:032x}", rand::random::<u128>()));
        private_directory(&root).unwrap();
        let socket = root.join("live.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o2770)).unwrap();
        private_directory(&root).unwrap();
        assert_eq!(fs::metadata(&root).unwrap().mode() & 0o7777, 0o700);
        let _client = UnixStream::connect(&socket).unwrap();
        let _connection = listener.accept().unwrap();
        drop((_client, _connection, listener));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_symlink_without_changing_target_permissions() {
        let root = std::env::temp_dir().join(format!("kherdr-runtime-{:032x}", rand::random::<u128>()));
        fs::create_dir(&root).unwrap();
        let target = root.join("target");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        let link = root.join("link");
        symlink(&target, &link).unwrap();
        assert!(private_directory(&link).is_err());
        assert_eq!(fs::metadata(&target).unwrap().mode() & 0o7777, 0o755);
        fs::remove_dir_all(root).unwrap();
    }
}
