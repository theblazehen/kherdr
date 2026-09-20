use std::path::Path;
use hmac::{Hmac, Mac};
use sha1::Sha1;
use russh::keys::ssh_key::sha2::{Digest, Sha256};
use russh::keys::{ssh_key::known_hosts::{Entry, HostPatterns, Marker}, HashAlg, PublicKey};
use crate::credentials::{read_optional, StoreLock};
use crate::atomic_file::{replace, DirectorySync};

const MAX_FILE: usize = 2 * 1024 * 1024;
const MAX_LINE: usize = 16 * 1024;

#[derive(Clone, Debug)]
pub struct TrustEntry {
    pub id: String,
    pub host: String,
    pub algorithm: String,
    pub fingerprint: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verification { Trusted, Unknown, Changed { previous: Vec<String> } }

fn hostname(host: &str, port: u16) -> Result<String, String> {
    if host.is_empty() || host.len() > 253 || port == 0 || host.chars().any(|c| c.is_whitespace() || c.is_control() || ",*!?|#".contains(c)) {
        return Err("Invalid hostname for SSH trust".into());
    }
    Ok(if port == 22 { host.to_ascii_lowercase() } else { format!("[{}]:{port}", host.to_ascii_lowercase()) })
}
fn read(path: &Path) -> Result<String, String> {
    String::from_utf8(read_optional(path, MAX_FILE)?).map_err(|_| "known_hosts is not UTF-8; preserve it and correct the invalid text before connecting".into())
}
fn parse(line: &str, number: usize) -> Result<Option<Entry>, String> {
    if line.len() > MAX_LINE { return Err(format!("known_hosts line {number} exceeds 16 KiB")); }
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') { return Ok(None); }
    // The library parser requires single-space separators. Normalize only the
    // parser's input; all untouched lines are persisted byte-for-byte.
    let normalized = line.split_whitespace().collect::<Vec<_>>().join(" ");
    normalized.parse().map(Some).map_err(|_| format!("Cannot safely interpret known_hosts line {number}. Correct its key/marker syntax before connecting; the file has not been changed."))
}

// OpenSSH pattern-list matching: a negative match always vetoes positives.
// Only '*' and '?' are wildcards; brackets in [host]:port are literal.
fn glob(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t, mut star, mut retry) = (0, 0, None, 0);
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p].eq_ignore_ascii_case(&text[t])) { p += 1; t += 1; }
        else if p < pattern.len() && pattern[p] == b'*' { star = Some(p); p += 1; retry = t; }
        else if let Some(s) = star { retry += 1; t = retry; p = s + 1; }
        else { return false; }
    }
    while p < pattern.len() && pattern[p] == b'*' { p += 1; }
    p == pattern.len()
}
fn matches(patterns: &HostPatterns, host: &str) -> Result<bool, String> {
    match patterns {
        HostPatterns::HashedName { salt, hash } => {
            let mut mac = Hmac::<Sha1>::new_from_slice(salt).map_err(|_| "Invalid known_hosts hash salt")?;
            mac.update(host.as_bytes());
            Ok(mac.verify_slice(hash).is_ok())
        }
        HostPatterns::Patterns(patterns) => {
            let mut found = false;
            for pattern in patterns {
                let (negated, pattern) = match pattern.strip_prefix('!') { Some(p) => (true, p), None => (false, pattern.as_str()) };
                if pattern.is_empty() || pattern.contains('|') || pattern.contains('\\') { return Err("Unsupported known_hosts pattern; correct its syntax before connecting".into()); }
                if glob(pattern.as_bytes(), host.as_bytes()) { if negated { return Ok(false); } found = true; }
            }
            Ok(found)
        }
    }
}
fn verify(text: &str, host: &str, key: &PublicKey) -> Result<Verification, String> {
    let mut previous = Vec::new();
    let mut trusted = false;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        let Some(entry) = parse(line, i + 1)? else { continue };
        if !matches(entry.host_patterns(), host)? { continue; }
        match entry.marker() {
            Some(Marker::Revoked) => {
                if entry.public_key().key_data() == key.key_data() { return Err(format!("Server key is REVOKED in known_hosts line {}. Connection refused; contact the server administrator.", i + 1)); }
                continue;
            }
            Some(Marker::CertAuthority) => return Err(format!("Host matches a certificate-authority rule at known_hosts line {}. This connection requires host-certificate verification, which is not supported; ask the administrator for an explicitly verified plain host key.", i + 1)),
            None => (),
        }
        trusted |= entry.public_key().key_data() == key.key_data();
        previous.push(entry.public_key().fingerprint(HashAlg::Sha256).to_string());
    }
    if trusted { return Ok(Verification::Trusted); }
    previous.sort(); previous.dedup();
    Ok(if previous.is_empty() { Verification::Unknown } else { Verification::Changed { previous } })
}
pub fn check(path: &Path, host: &str, port: u16, key: &PublicKey) -> Result<Verification, String> {
    verify(&read(path)?, &hostname(host, port)?, key)
}

pub fn remember(path: &Path, host: &str, port: u16, key: &PublicKey, expected: &Verification) -> Result<(), String> {
    let host = hostname(host, port)?;
    let _lock = StoreLock::acquire(path)?;
    let before = read(path)?;
    if &verify(&before, &host, key)? != expected { return Err("Host trust changed while approval was open. Reconnect and verify the current fingerprint.".into()); }
    if *expected == Verification::Trusted { return Ok(()); }
    let mut after = String::with_capacity(before.len() + 512);
    for (i, line) in before.split_inclusive('\n').enumerate() {
        let Some(entry) = parse(line, i + 1)? else { after.push_str(line); continue };
        if !matches(entry.host_patterns(), &host)? || entry.marker().is_some() { after.push_str(line); continue; }
        if !matches!(expected, Verification::Changed { .. }) { after.push_str(line); continue; }
        // Replacing a wildcard or hashed pin would affect a rule beyond a single
        // exact host. Require deliberate removal rather than weakening that rule.
        let HostPatterns::Patterns(patterns) = entry.host_patterns() else {
            return Err("Changed key is pinned by a hashed known_hosts entry. Forget that specific entry in Trusted hosts after verifying with the administrator, then reconnect.".into());
        };
        if patterns.iter().any(|p| p.contains('*') || p.contains('?') || p.starts_with('!')) {
            return Err("Changed key matches a wildcard/negated known_hosts rule. Review that rule with the administrator; it cannot be replaced for one host automatically.".into());
        }
        let remaining: Vec<&str> = patterns.iter().filter(|p| !p.eq_ignore_ascii_case(&host)).map(String::as_str).collect();
        if !remaining.is_empty() {
            // Preserve other host aliases and the original key/comment/line ending.
            let trimmed = line.trim_start();
            let end = trimmed.find(char::is_whitespace).ok_or("Invalid known_hosts line")?;
            after.push_str(&line[..line.len() - trimmed.len()]);
            after.push_str(&remaining.join(","));
            after.push_str(&trimmed[end..]);
        }
    }
    if !after.is_empty() && !after.ends_with('\n') { after.push('\n'); }
    let public = PublicKey::new(key.key_data().clone(), "");
    after.push_str(&host); after.push(' ');
    after.push_str(&public.to_openssh().map_err(|_| "Cannot encode host public key")?); after.push('\n');
    if read(path)? != before { return Err("known_hosts changed during approval; reconnect and try again".into()); }
    replace(path, after.as_bytes(), MAX_FILE, DirectorySync::BestEffort)
}

fn identity(line: &str, occurrence: usize) -> String {
    // Digest identifies exact persisted bytes; occurrence distinguishes duplicates.
    use base64::Engine;
    let digest = Sha256::digest(line.as_bytes());
    format!("{}:{occurrence}", base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest))
}
pub fn entries(path: &Path) -> Result<Vec<TrustEntry>, String> {
    let text = read(path)?;
    let mut entries = Vec::new();
    let mut occurrences = std::collections::HashMap::<&str, usize>::new();
    for (i, line) in text.split_inclusive('\n').enumerate() {
        let occurrence = occurrences.entry(line).or_default();
        let id = identity(line, *occurrence); *occurrence += 1;
        let Some(entry) = parse(line, i + 1)? else { continue };
        let marker = entry.marker().map(|m| format!("{m} ")).unwrap_or_default();
        entries.push(TrustEntry { id, host: format!("{marker}{}", entry.host_patterns().to_string()), algorithm: entry.public_key().algorithm().to_string(), fingerprint: entry.public_key().fingerprint(HashAlg::Sha256).to_string() });
    }
    Ok(entries)
}
pub fn remove(path: &Path, id: &str) -> Result<(), String> {
    let _lock = StoreLock::acquire(path)?;
    let before = read(path)?;
    let mut after = String::with_capacity(before.len());
    let mut removed = false;
    let mut occurrences = std::collections::HashMap::<&str, usize>::new();
    for (i, line) in before.split_inclusive('\n').enumerate() {
        let occurrence = occurrences.entry(line).or_default();
        let current = identity(line, *occurrence); *occurrence += 1;
        if current == id {
            let entry = parse(line, i + 1)?.ok_or("Selected host entry is not a key")?;
            if entry.marker() == Some(&Marker::Revoked) { return Err("Revocation rules cannot be forgotten in this manager. Ask the administrator to review the original known_hosts file.".into()); }
            removed = true;
        } else { after.push_str(line); }
    }
    if !removed { return Err("Trusted host entry changed or was already removed; reopen Trusted hosts".into()); }
    if read(path)? != before { return Err("known_hosts changed; reopen Trusted hosts".into()); }
    replace(path, after.as_bytes(), MAX_FILE, DirectorySync::BestEffort)
}
