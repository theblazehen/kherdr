use std::fs::{self, OpenOptions};
use crate::atomic_file::{replace, DirectorySync};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use russh::keys::{ssh_encoding::{Decode, Uint}, ssh_key::{self, private::{Ed25519Keypair, KeypairData, RsaKeypair}}, Algorithm, HashAlg, PrivateKey};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub(crate) const MAX_KEY: usize = 256 * 1024;
const MAX_STORE: usize = 1024 * 1024;
const MAX_ENTRIES: usize = 128;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordEntry {
    host: String,
    port: u16,
    user: String,
    #[serde(serialize_with = "serialize_password", deserialize_with = "deserialize_password")]
    password: Zeroizing<String>,
}
fn serialize_password<S: serde::Serializer>(value: &Zeroizing<String>, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(value.as_str())
}
fn deserialize_password<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Zeroizing<String>, D::Error> {
    String::deserialize(deserializer).map(Zeroizing::new)
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordDisk { version: u32, revision: String, passwords: Vec<PasswordEntry> }

// Deliberately separate from exportable connection profiles. Device storage is
// recoverable plaintext, not encryption with a colocated key. Never derive Debug.
pub struct PasswordStore { path: PathBuf, disk: PasswordDisk, snapshot: Zeroizing<Vec<u8>> }
impl PasswordStore {
    pub fn load(root: &Path) -> Result<Self, String> {
        let path = root.join(".ssh/passwords.json");
        let snapshot = Zeroizing::new(read_optional(&path, MAX_STORE)?);
        let disk = if snapshot.is_empty() { PasswordDisk { version: 1, revision: String::new(), passwords: Vec::new() } }
            else { serde_json::from_slice::<PasswordDisk>(&snapshot).map_err(|_| "Saved password store is invalid; it was not overwritten".to_owned())? };
        if disk.version != 1 || disk.passwords.len() > MAX_ENTRIES
            || disk.passwords.iter().any(|entry| entry.host.is_empty() || entry.user.is_empty() || entry.port == 0 || entry.password.len() > MAX_KEY) {
            return Err("Saved password store is invalid; it was not overwritten".into());
        }
        Ok(Self { path, disk, snapshot })
    }
    pub fn password(&self, host: &str, port: u16, user: &str) -> Option<&str> {
        self.disk.passwords.iter().find(|entry| entry.host == host && entry.port == port && entry.user == user).map(|entry| entry.password.as_str())
    }
    fn persist(&mut self) -> Result<(), String> {
        let _lock = StoreLock::acquire(&self.path)?;
        let current = Zeroizing::new(read_optional(&self.path, MAX_STORE)?);
        if *current != *self.snapshot { return Err("Saved passwords changed during sign-in; connect again to remember this password".into()); }
        self.disk.revision = format!("{:032x}", rand::random::<u128>());
        let bytes = Zeroizing::new(serde_json::to_vec(&self.disk).map_err(|_| "Cannot encode saved password store".to_owned())?);
        replace(&self.path, &bytes, MAX_STORE, DirectorySync::BestEffort)?;
        self.snapshot = bytes;
        Ok(())
    }
    pub fn remember(&mut self, host: &str, port: u16, user: &str, password: Zeroizing<String>) -> Result<(), String> {
        if password.len() > MAX_KEY { return Err("Password exceeds storage limit".into()); }
        self.disk.passwords.retain(|entry| !(entry.host == host && entry.port == port && entry.user == user));
        if self.disk.passwords.len() >= MAX_ENTRIES { return Err("Saved password limit reached; forget saved passwords first".into()); }
        self.disk.passwords.push(PasswordEntry { host: host.into(), port, user: user.into(), password });
        self.persist()
    }
    pub fn reject(&mut self, host: &str, port: u16, user: &str) -> Result<(), String> {
        self.disk.passwords.retain(|entry| !(entry.host == host && entry.port == port && entry.user == user));
        self.persist()
    }
    pub fn forget_all(root: &Path) -> Result<(), String> {
        let path = root.join(".ssh/passwords.json");
        let _lock = StoreLock::acquire(&path)?;
        // A fresh revision also revokes writes from already-open password prompts.
        let disk = PasswordDisk { version: 1, revision: format!("{:032x}", rand::random::<u128>()), passwords: Vec::new() };
        let bytes = Zeroizing::new(serde_json::to_vec(&disk).map_err(|_| "Cannot encode saved password store".to_owned())?);
        let _old = Zeroizing::new(read_optional(&path, MAX_STORE)?);
        replace(&path, &bytes, MAX_STORE, DirectorySync::BestEffort)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyEntry {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub public_key: String,
    pub fingerprint: String,
    pub encrypted: bool,
    pub managed: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiskStore { version: u32, keys: Vec<KeyEntry> }

pub struct KeyStore {
    root: PathBuf,
    keys: Vec<KeyEntry>,
    snapshot: Vec<u8>,
    pub error: Option<String>,
}

pub(crate) fn bounded_read(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    // Opening a FIFO read-only would wait for a writer before fstat can reject
    // it. Nonblocking open preserves symlink support while checking the actual
    // opened object, not a racy pre-open path lookup.
    let file = OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK)
        .open(path).map_err(|e| format!("Cannot open {}: {e}", path.display()))?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() { return Err("Select a regular file".into()); }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take((limit + 1) as u64).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() > limit { return Err(format!("File exceeds {limit} byte limit")); }
    Ok(std::mem::take(&mut *bytes))
}

pub(crate) fn read_optional(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.to_string()),
        Ok(m) if m.file_type().is_symlink() => Err(format!("Refusing to replace symbolic link {}", path.display())),
        Ok(_) => bounded_read(path, limit),
    }
}

pub(crate) struct StoreLock(PathBuf);
impl StoreLock {
    pub(crate) fn acquire(path: &Path) -> Result<Self, String> {
        let parent = path.parent().ok_or("Store has no parent directory")?;
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let lock = path.with_extension("kherdr-lock");
        OpenOptions::new().write(true).create_new(true).mode(0o600).open(&lock)
            .map_err(|e| format!("Store is busy or cannot be locked ({}): {e}. If no kherdr process is writing it, remove the stale lock.", lock.display()))?;
        Ok(Self(lock))
    }
}
impl Drop for StoreLock { fn drop(&mut self) { let _ = fs::remove_file(&self.0); } }

fn key_error() -> String { "Invalid or unsupported private key, or incorrect passphrase. Use OpenSSH, supported PEM, or Dropbear RSA/Ed25519.".into() }
fn encrypted_pem(text: &str) -> bool {
    text.lines().any(|l| l == "-----BEGIN ENCRYPTED PRIVATE KEY-----" || l.trim() == "Proc-Type: 4,ENCRYPTED")
}

fn parse_private(bytes: &[u8], passphrase: Option<&str>, inspect: bool) -> Result<PrivateKey, String> {
    if bytes.starts_with(b"-----BEGIN OPENSSH PRIVATE KEY-----") {
        let key = PrivateKey::from_openssh(bytes).map_err(|_| key_error())?;
        if key.is_encrypted() && !(inspect && passphrase.is_none()) {
            return key.decrypt(passphrase.ok_or("Private key requires a passphrase")?).map_err(|_| key_error());
        }
        return Ok(key);
    }
    if bytes.starts_with(b"-----BEGIN ") {
        let text = std::str::from_utf8(bytes).map_err(|_| key_error())?;
        return russh::keys::decode_secret_key(text, passphrase).map_err(|_| key_error());
    }
    // Dropbear serializes SSH strings, RSA e/n/d/p/q mpints, or a 64-byte
    // Ed25519 seed/public pair. Cryptographic validation stays in the libraries.
    let mut input = bytes;
    let algorithm = String::decode(&mut input).map_err(|_| key_error())?;
    let data = match algorithm.as_str() {
        "ssh-ed25519" => {
            let pair = Zeroizing::new(Vec::<u8>::decode(&mut input).map_err(|_| key_error())?);
            KeypairData::Ed25519(Ed25519Keypair::try_from(pair.as_slice()).map_err(|_| key_error())?)
        }
        "ssh-rsa" => {
            let mut integer = || -> Result<Uint, String> {
                let value = ssh_key::Mpint::decode(&mut input).map_err(|_| key_error())?;
                Uint::try_from(&value).map_err(|_| key_error())
            };
            let e = integer()?;
            let n = integer()?;
            let d = integer()?;
            let p = integer()?;
            let q = integer()?;
            let rsa = rsa::RsaPrivateKey::from_components(n, e, d, vec![p, q]).map_err(|_| key_error())?;
            rsa.validate().map_err(|_| key_error())?;
            KeypairData::Rsa(RsaKeypair::try_from(rsa).map_err(|_| key_error())?)
        }
        _ => return Err(key_error()),
    };
    if !input.is_empty() { return Err(key_error()); }
    PrivateKey::new(data, "").map_err(|_| key_error())
}

pub fn load_private_key(path: &Path, passphrase: Option<&str>) -> Result<PrivateKey, String> {
    let bytes = Zeroizing::new(bounded_read(path, MAX_KEY)?);
    let key = parse_private(&bytes, passphrase, false)?;
    validate_private(&key)?;
    Ok(key)
}

fn validate_private(key: &PrivateKey) -> Result<(), String> {
    if let KeypairData::Rsa(pair) = key.key_data() {
        let rsa = rsa::RsaPrivateKey::try_from(pair).map_err(|_| key_error())?;
        rsa.validate().map_err(|_| key_error())?;
    }
    Ok(())
}

fn metadata(path: &Path, key: &PrivateKey, encrypted: bool) -> Result<KeyEntry, String> {
    // Never carry a user-supplied private-file comment into metadata.
    let public = russh::keys::PublicKey::new(key.public_key().key_data().clone(), "");
    Ok(KeyEntry { id: String::new(), name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(), path: path.to_owned(), public_key: public.to_openssh().map_err(|_| key_error())?, fingerprint: public.fingerprint(HashAlg::Sha256).to_string(), encrypted, managed: false })
}

pub fn inspect_private_key(path: &Path, passphrase: Option<&str>) -> Result<KeyEntry, String> {
    let bytes = Zeroizing::new(bounded_read(path, MAX_KEY)?);
    let pem_encrypted = std::str::from_utf8(&bytes).is_ok_and(encrypted_pem);
    if pem_encrypted && passphrase.is_none() {
        return Ok(KeyEntry { id: String::new(), name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(), path: path.to_owned(), public_key: String::new(), fingerprint: String::new(), encrypted: true, managed: false });
    }
    let locked = if bytes.starts_with(b"-----BEGIN OPENSSH PRIVATE KEY-----") { PrivateKey::from_openssh(bytes.as_slice()).map_err(|_| key_error())?.is_encrypted() } else { false };
    let key = parse_private(&bytes, passphrase, true)?;
    metadata(path, &key, pem_encrypted || locked)
}

fn valid_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) { return Err("Key name must contain 1–128 bytes, without control characters".into()); }
    Ok(name.into())
}
fn same_file(a: &Path, b: &Path) -> bool {
    a == b || fs::canonicalize(a).ok().zip(fs::canonicalize(b).ok()).is_some_and(|(a,b)| a == b)
        || fs::metadata(a).ok().zip(fs::metadata(b).ok()).is_some_and(|(a,b)| a.dev() == b.dev() && a.ino() == b.ino())
}

impl KeyStore {
    pub fn load(root: &Path) -> Self {
        let root = fs::canonicalize(root).or_else(|_| std::path::absolute(root));
        let mut store = Self { root: root.as_ref().cloned().unwrap_or_default(), keys: Vec::new(), snapshot: Vec::new(), error: None };
        let result = (|| {
            let root = root.map_err(|e| e.to_string())?;
            let bytes = read_optional(&root.join("keys.json"), MAX_STORE)?;
            if bytes.is_empty() { return Ok(()); }
            let disk: DiskStore = serde_json::from_slice(&bytes).map_err(|_| "Invalid key metadata; original file left unchanged".to_string())?;
            if disk.version != 1 || disk.keys.len() > MAX_ENTRIES { return Err("Unsupported key metadata version or too many keys".into()); }
            let mut ids = std::collections::HashSet::new();
            for entry in &disk.keys {
                if entry.id.len() != 32 || !entry.id.bytes().all(|b| b.is_ascii_hexdigit()) || !ids.insert(&entry.id) || valid_name(&entry.name).is_err() || !entry.path.is_absolute() || (entry.managed && entry.path != root.join("keys").join(&entry.id)) { return Err("Invalid key metadata ownership or identity; original file left unchanged".into()); }
            }
            store.snapshot = bytes;
            store.keys = disk.keys;
            Ok(())
        })();
        if let Err(error) = result { store.error = Some(error); }
        store
    }
    pub fn entries(&self) -> &[KeyEntry] { &self.keys }
    fn writable(&self) -> Result<(), String> { match &self.error { Some(e) => Err(e.clone()), None => Ok(()) } }
    fn commit(&mut self, keys: Vec<KeyEntry>) -> Result<(), String> {
        self.writable()?;
        let path = self.root.join("keys.json");
        let _lock = StoreLock::acquire(&path)?;
        if read_optional(&path, MAX_STORE)? != self.snapshot { return Err("Key library changed; reopen it before editing".into()); }
        let bytes = serde_json::to_vec_pretty(&DiskStore { version: 1, keys: keys.clone() }).map_err(|_| "Cannot encode key metadata")?;
        replace(&path, &bytes, MAX_STORE, DirectorySync::BestEffort)?;
        self.snapshot = bytes;
        self.keys = keys;
        Ok(())
    }
    fn add(&mut self, name: &str, key: PrivateKey, passphrase: Option<&str>) -> Result<KeyEntry, String> {
        self.writable()?;
        validate_private(&key)?;
        let name = valid_name(name)?;
        if self.keys.len() >= MAX_ENTRIES { return Err("Key library is limited to 128 entries".into()); }
        fs::create_dir_all(self.root.join("keys")).map_err(|e| e.to_string())?;
        if fs::symlink_metadata(self.root.join("keys")).map_err(|e| e.to_string())?.file_type().is_symlink() {
            return Err("Managed key directory must not be a symbolic link".into());
        }
        let root = fs::canonicalize(&self.root).map_err(|e| e.to_string())?;
        let id = format!("{:032x}", rand::random::<u128>());
        let path = root.join("keys").join(&id);
        let passphrase = passphrase.filter(|s| !s.is_empty());
        let key = match passphrase { Some(p) => key.encrypt(&mut rand::rng(), p).map_err(|_| "Cannot encrypt key")?, None => key };
        let mut entry = metadata(&path, &key, key.is_encrypted())?;
        entry.id = id; entry.name = name; entry.managed = true;
        let encoded = key.to_openssh(ssh_key::LineEnding::LF).map_err(|_| "Cannot encode private key")?;
        // A random stable name is created exclusively; never overwrite existing identities.
        let _lock = StoreLock::acquire(&path)?;
        if fs::symlink_metadata(&path).is_ok() { return Err("Generated key identity already exists; try again".into()); }
        replace(&path, encoded.as_bytes(), MAX_KEY, DirectorySync::BestEffort)?;
        let mut keys = self.keys.clone(); keys.push(entry.clone());
        if let Err(e) = self.commit(keys) { let _ = fs::remove_file(&path); return Err(e); }
        Ok(entry)
    }
    pub fn generate(&mut self, name: &str, passphrase: Option<&str>) -> Result<KeyEntry, String> {
        self.writable()?;
        valid_name(name)?;
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).map_err(|_| "Cannot generate Ed25519 key")?;
        self.add(name, key, passphrase)
    }
    pub fn import_file(&mut self, name: &str, path: &Path, passphrase: Option<&str>) -> Result<KeyEntry, String> {
        let key = load_private_key(path, passphrase)?;
        self.add(name, key, passphrase)
    }
    pub fn import_text(&mut self, name: &str, text: &str, passphrase: Option<&str>) -> Result<KeyEntry, String> {
        let text = text.trim();
        if text.len() > MAX_KEY { return Err("Private key exceeds 256 KiB".into()); }
        let key = parse_private(text.as_bytes(), passphrase, false)?;
        self.add(name, key, passphrase)
    }
    pub fn register_existing(&mut self, path: &Path) -> Result<(), String> {
        self.writable()?;
        if self.keys.iter().any(|e| same_file(&e.path, path)) { return Ok(()); }
        if self.keys.len() >= MAX_ENTRIES { return Err("Key library is limited to 128 entries".into()); }
        let path = fs::canonicalize(path).map_err(|e| e.to_string())?;
        let mut entry = inspect_private_key(&path, None)?;
        entry.id = format!("{:032x}", rand::random::<u128>());
        entry.name = valid_name(&entry.name)?;
        let mut keys = self.keys.clone(); keys.push(entry);
        self.commit(keys)
    }
    pub fn rename(&mut self, id: &str, name: &str) -> Result<(), String> {
        let name = valid_name(name)?;
        let mut keys = self.keys.clone();
        keys.iter_mut().find(|e| e.id == id).ok_or("Key no longer exists")?.name = name;
        self.commit(keys)
    }
    pub fn remove(&mut self, id: &str, used: &[PathBuf]) -> Result<(), String> {
        let entry = self.keys.iter().find(|e| e.id == id).ok_or("Key no longer exists")?.clone();
        if used.iter().any(|p| same_file(p, &entry.path)) { return Err("This key is used by a saved connection. Choose another key there first.".into()); }
        if entry.managed {
            let owned = fs::canonicalize(&self.root).map_err(|e| e.to_string())?.join("keys").join(id);
            if entry.path != owned || fs::symlink_metadata(self.root.join("keys")).map_err(|e| e.to_string())?.file_type().is_symlink() { return Err("Refusing to delete a key outside the managed library".into()); }
        }
        let keys = self.keys.iter().filter(|e| e.id != id).cloned().collect();
        self.commit(keys)?;
        if entry.managed {
            match fs::remove_file(&entry.path) { Ok(()) => (), Err(e) if e.kind() == std::io::ErrorKind::NotFound => (), Err(e) => return Err(format!("Key removed from library but private file could not be deleted: {e}")) }
        }
        Ok(())
    }
}
