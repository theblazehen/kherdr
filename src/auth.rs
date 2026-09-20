// SPDX-License-Identifier: GPL-3.0-or-later
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind { NewHost, ChangedHost, Password, Passphrase }

#[derive(Clone, Debug)]
pub struct Prompt {
    pub id: u64,
    pub kind: PromptKind,
    pub title: String,
    pub detail: String,
    pub fingerprint: String,
    pub previous_fingerprint: String,
}

// Never derive Debug or Serialize for a response containing a secret.
pub struct Answer {
    pub id: u64,
    pub approved: bool,
    pub remember_password: bool,
    pub secret: Zeroizing<String>,
}
