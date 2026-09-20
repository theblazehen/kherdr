// SPDX-License-Identifier: GPL-3.0-or-later
//! Package-independent state, with a one-time copy from the manual installation.
use crate::{
    atomic_file::{self, DirectorySync},
    credentials,
};
use serde_json::Value;
use sha1::{Digest, Sha1};
use std::{
    fs,
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, PermissionsExt},
    },
    path::{Component, Path},
};

const ID_FILE: &str = "local-runtime-id";

fn path_identity(root: &Path) -> String {
    format!("{:x}", Sha1::digest(root.as_os_str().as_bytes()))
}

/// Persist the old path-derived identity before any later relocation.
pub(crate) fn runtime_id(root: &Path) -> Result<String, String> {
    let path = root.join(ID_FILE);
    let _lock = credentials::StoreLock::acquire(&path)?;
    let bytes = credentials::read_optional(&path, 40)?;
    if bytes.is_empty() {
        let id = path_identity(root);
        if path.exists() {
            return Err("Empty local runtime identity; existing sessions were not replaced".into());
        }
        atomic_file::replace(&path, id.as_bytes(), 40, DirectorySync::BestEffort)?;
        return Ok(id);
    }
    let id = String::from_utf8(bytes).map_err(|_| "Invalid local runtime identity")?;
    if id.len() != 40 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Invalid local runtime identity; existing sessions were not replaced".into());
    }
    Ok(id.to_ascii_lowercase())
}

fn copy_directory(source: &Path, destination: &Path) -> Result<(), String> {
    for item in fs::read_dir(source).map_err(|e| e.to_string())? {
        let item = item.map_err(|e| e.to_string())?;
        let kind = item.file_type().map_err(|e| e.to_string())?;
        let target = destination.join(item.file_name());
        if kind.is_dir() {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&target)
                .map_err(|e| e.to_string())?;
            copy_directory(&item.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(item.path(), &target).map_err(|e| e.to_string())?;
            fs::set_permissions(&target, fs::Permissions::from_mode(0o600))
                .map_err(|e| e.to_string())?;
            fs::File::open(&target)
                .and_then(|file| file.sync_all())
                .map_err(|e| e.to_string())?;
        } else {
            return Err(format!(
                "Cannot migrate non-regular state entry {}; original state is unchanged",
                item.path().display()
            ));
        }
    }
    if let Ok(directory) = fs::File::open(destination) {
        let _ = directory.sync_all();
    }
    Ok(())
}

fn rebase(value: &mut Value, source: &Path, destination: &Path) {
    let Some(path) = value.as_str() else { return };
    let Ok(relative) = Path::new(path).strip_prefix(source) else {
        return;
    };
    // An external key expressed using '..' must not be redirected to a new file.
    if relative
        .components()
        .any(|part| part == Component::ParentDir)
    {
        return;
    }
    *value = Value::String(destination.join(relative).to_string_lossy().into_owned());
}

fn rebase_json(stage: &Path, name: &str, source: &Path, destination: &Path) -> Result<(), String> {
    let path = stage.join(name);
    let bytes = credentials::read_optional(&path, 1024 * 1024)?;
    if bytes.is_empty() {
        return Ok(());
    }
    let mut document: Value = serde_json::from_slice(&bytes)
        .map_err(|_| format!("Cannot migrate invalid {name}; original state is unchanged"))?;
    if name == "connections.json" {
        let profiles = document
            .get_mut("profiles")
            .and_then(Value::as_array_mut)
            .ok_or("Invalid connections profile list")?;
        for profile in profiles {
            if let Some(identity) = profile
                .get_mut("config")
                .and_then(|config| config.get_mut("identity"))
            {
                rebase(identity, source, destination);
            }
        }
    } else {
        let keys = document
            .get_mut("keys")
            .and_then(Value::as_array_mut)
            .ok_or("Invalid key metadata list")?;
        for key in keys {
            if let Some(path) = key.get_mut("path") {
                rebase(path, source, destination);
            }
        }
    }
    let bytes = serde_json::to_vec_pretty(&document).map_err(|e| e.to_string())?;
    atomic_file::replace(&path, &bytes, 1024 * 1024, DirectorySync::BestEffort)
}

/// The launcher holds both the destination and legacy UI locks. Copy into a
/// sibling, then rename once; never merge into, move, or remove existing state.
/// The retained legacy copy also keeps HOME/key paths of live PTYs valid.
pub(crate) fn prepare(destination: &Path, source: Option<&Path>) -> Result<(), String> {
    let parent = destination
        .parent()
        .ok_or("State directory has no parent")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let _lock = credentials::StoreLock::acquire(destination)?;
    if destination.exists() {
        if !destination.is_dir() {
            return Err("State destination is not a directory".into());
        }
        return Ok(());
    }
    let parent = parent.canonicalize().map_err(|e| e.to_string())?;
    let destination = parent.join(
        destination
            .file_name()
            .ok_or("State directory has no name")?,
    );
    let stage = parent.join(format!(".state-import-{:032x}", rand::random::<u128>()));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&stage)
        .map_err(|e| e.to_string())?;
    let result = (|| {
        if let Some(source) = source {
            let source = source.canonicalize().map_err(|e| e.to_string())?;
            if destination.starts_with(&source) {
                return Err("State destination must be outside the legacy directory".into());
            }
            copy_directory(&source, &stage)?;
            rebase_json(&stage, "connections.json", &source, &destination)?;
            rebase_json(&stage, "keys.json", &source, &destination)?;
            // Native Store::load imports the INI only when no JSON store exists.
            // Parse with that same importer instead of rewriting escaped INI text.
            if !stage.join("connections.json").exists() && stage.join("connection.ini").exists() {
                let mut store = crate::connection::Store::load(&stage.join("connection.ini"));
                if let Some(error) = &store.error {
                    return Err(error.clone());
                }
                if let Some(profile) = store.selected().cloned() {
                    store.save(profile)?;
                }
                rebase_json(&stage, "connections.json", &source, &destination)?;
            }
            if stage.join("connections.json").exists() && stage.join("connection.ini").exists() {
                fs::remove_file(stage.join("connection.ini")).map_err(|e| e.to_string())?;
            }
            if !stage.join(ID_FILE).exists() {
                atomic_file::replace(
                    &stage.join(ID_FILE),
                    path_identity(&source).as_bytes(),
                    40,
                    DirectorySync::BestEffort,
                )?;
            }
        } else {
            atomic_file::replace(
                &stage.join(ID_FILE),
                path_identity(&destination).as_bytes(),
                40,
                DirectorySync::BestEffort,
            )?;
        }
        runtime_id(&stage)?;
        if !stage.join(".ssh").exists() {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(stage.join(".ssh"))
                .map_err(|e| e.to_string())?;
        }
        if let Ok(directory) = fs::File::open(&stage) {
            let _ = directory.sync_all();
        }
        fs::rename(&stage, &destination).map_err(|e| e.to_string())?;
        if let Ok(directory) = fs::File::open(&parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&stage);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{connection, local_runtime::Runtime};
    use serde_json::json;
    use std::path::PathBuf;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir()
                .join(format!("kherdr-state-test-{:032x}", rand::random::<u128>()));
            fs::create_dir_all(root.join("legacy/.ssh")).unwrap();
            Self(root)
        }
        fn legacy(&self) -> PathBuf {
            self.0.join("legacy")
        }
        fn target(&self) -> PathBuf {
            self.0.join("persistent/etc")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let runtime = PathBuf::from(format!("/tmp/kherdr-{}", path_identity(&self.legacy())));
            let _ = fs::remove_dir_all(runtime);
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn relocation_preserves_sessions_secrets_and_managed_key_ownership() {
        let fixture = Fixture::new();
        let source = fixture.legacy();
        let target = fixture.target();
        let identity = source.join(".ssh/identity");
        fs::write(&identity, b"private key bytes\n").unwrap();
        fs::write(source.join(".ssh/known_hosts"), b"trusted server key\n").unwrap();
        fs::write(
            source.join(".ssh/passwords.json"),
            b"opaque saved-password bytes",
        )
        .unwrap();
        fs::write(
            source.join("connection.ini"),
            format!(
                "[connection]\nhost=example.invalid\nuser=test\nidentity={}\n",
                identity.display()
            ),
        )
        .unwrap();
        let mut store = connection::Store::load(&source.join("connection.ini"));
        let profile = store.selected().unwrap().clone();
        store.save(profile.clone()).unwrap();
        let mut external = profile.clone();
        external.id.clear();
        external.name = "External key".into();
        external.config.identity = Some("/mnt/us/external-key".into());
        store.save(external).unwrap();
        let key_id = "11111111111111111111111111111111";
        fs::create_dir(source.join("keys")).unwrap();
        fs::write(
            source.join("keys").join(key_id),
            b"managed private key bytes",
        )
        .unwrap();
        fs::write(source.join("keys.json"), serde_json::to_vec(&json!({"version":1,"keys":[{
            "id":key_id,"name":"Managed key","path":source.join("keys").join(key_id),
            "public_key":"public metadata","fingerprint":"SHA256:fixture","encrypted":false,"managed":true
        }]})).unwrap()).unwrap();
        let original = fs::read(source.join("connections.json")).unwrap();

        prepare(&target, Some(&source)).unwrap();
        let migrated = connection::Store::load(&target.join("connection.ini"));
        assert!(migrated.error.is_none(), "{:?}", migrated.error);
        assert_eq!(
            migrated.selected().unwrap().config.identity.as_deref(),
            target.join(".ssh/identity").to_str()
        );
        assert_eq!(
            migrated.profiles()[1].config.identity.as_deref(),
            Some("/mnt/us/external-key")
        );
        let keys = credentials::KeyStore::load(&target);
        assert_eq!(keys.entries()[0].path, target.join("keys").join(key_id));
        for name in [
            ".ssh/identity",
            ".ssh/known_hosts",
            ".ssh/passwords.json",
            "keys/11111111111111111111111111111111",
        ] {
            assert_eq!(
                fs::read(target.join(name)).unwrap(),
                fs::read(source.join(name)).unwrap()
            );
        }
        let runtime = Runtime::new(&target).unwrap();
        assert_eq!(
            runtime.api_socket,
            PathBuf::from(format!("/tmp/kherdr-{}/herdr.sock", path_identity(&source)))
        );
        assert_eq!(fs::read(source.join("connections.json")).unwrap(), original);
        assert!(!source.join(ID_FILE).exists());
        assert!(!target.join("connection.ini").exists());
        fs::write(
            target.join("text-size.json"),
            b"user changed this after migration",
        )
        .unwrap();
        prepare(&target, Some(&source)).unwrap();
        assert_eq!(
            fs::read(target.join("text-size.json")).unwrap(),
            b"user changed this after migration"
        );
    }

    #[test]
    fn ini_only_installation_uses_the_native_importer_before_relocation() {
        let fixture = Fixture::new();
        let source = fixture.legacy();
        let target = fixture.target();
        fs::write(
            source.join("connection.ini"),
            format!(
                "[connection]\nhost=example.invalid\nidentity={}\n",
                source.join(".ssh/identity").display()
            ),
        )
        .unwrap();
        prepare(&target, Some(&source)).unwrap();
        let store = connection::Store::load(&target.join("connection.ini"));
        assert!(store.error.is_none(), "{:?}", store.error);
        assert_eq!(
            store.selected().unwrap().config.identity.as_deref(),
            target.join(".ssh/identity").to_str()
        );
        assert!(!source.join("connections.json").exists());
    }

    #[test]
    fn corrupt_import_does_not_commit_or_overwrite_a_destination() {
        let fixture = Fixture::new();
        let source = fixture.legacy();
        let target = fixture.target();
        fs::write(source.join("connections.json"), b"broken json").unwrap();
        assert!(prepare(&target, Some(&source)).is_err());
        assert!(!target.exists());
        assert_eq!(
            fs::read(source.join("connections.json")).unwrap(),
            b"broken json"
        );
        fs::remove_file(source.join("connections.json")).unwrap();
        fs::write(source.join(ID_FILE), b"").unwrap();
        assert!(prepare(&target, Some(&source)).is_err());
        assert!(!target.exists());
        assert_eq!(fs::read(source.join(ID_FILE)).unwrap(), b"");
    }
}
