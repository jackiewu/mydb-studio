//! Keeping connection passwords out of the app database.
//!
//! The app gives the [`Store`](crate::Store) a [`SecretVault`] (the OS
//! keychain). All passwords then live in one vault entry, read once, and
//! the database only holds profiles without them. Without a vault, or when
//! the vault can't be used, passwords stay in the database as before.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::profile::{ConnectionProfile, SshAuth};

/// Where secrets go: one entry holding a JSON document.
pub trait SecretVault: Send {
    /// The stored document, or `None` when there is none yet.
    fn load(&self) -> Result<Option<String>, String>;
    fn save(&self, data: &str) -> Result<(), String>;
}

/// The secrets of one connection.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConnectionSecrets {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_passphrase: Option<String>,
}

impl ConnectionSecrets {
    pub fn is_empty(&self) -> bool {
        self.password.is_none() && self.ssh_password.is_none() && self.ssh_passphrase.is_none()
    }
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

/// Takes the secrets out of `profile`.
pub fn strip(profile: &ConnectionProfile) -> (ConnectionProfile, ConnectionSecrets) {
    let mut p = profile.clone();
    let mut s = ConnectionSecrets {
        password: p.password.take().filter(|x| !x.is_empty()),
        ..Default::default()
    };
    if let Some(ssh) = p.ssh.as_mut() {
        match &mut ssh.auth {
            SshAuth::Password { password } => {
                s.ssh_password = non_empty(password);
                password.clear();
            }
            SshAuth::Key { passphrase, .. } => {
                s.ssh_passphrase = passphrase.take().filter(|x| !x.is_empty());
            }
        }
    }
    (p, s)
}

/// Puts `secrets` back into `profile`, where it has none of its own.
pub fn fill(profile: &mut ConnectionProfile, secrets: &ConnectionSecrets) {
    if profile.password.as_deref().is_none_or(str::is_empty) {
        profile.password = secrets.password.clone();
    }
    if let Some(ssh) = profile.ssh.as_mut() {
        match &mut ssh.auth {
            SshAuth::Password { password } if password.is_empty() => {
                *password = secrets.ssh_password.clone().unwrap_or_default();
            }
            SshAuth::Key { passphrase, .. } if passphrase.as_deref().is_none_or(str::is_empty) => {
                passphrase.clone_from(&secrets.ssh_passphrase);
            }
            _ => {}
        }
    }
}

/// The vault and its contents, by connection id.
pub struct Secrets {
    vault: Box<dyn SecretVault>,
    /// `None` until first read, or when the vault failed (then the database
    /// keeps passwords, so nothing is lost or overwritten).
    cache: Option<HashMap<String, ConnectionSecrets>>,
    failed: bool,
}

impl Secrets {
    pub fn new(vault: Box<dyn SecretVault>) -> Self {
        Self {
            vault,
            cache: None,
            failed: false,
        }
    }

    fn map(&mut self) -> Option<&mut HashMap<String, ConnectionSecrets>> {
        if self.failed {
            return None;
        }
        if self.cache.is_none() {
            match self.vault.load() {
                Ok(data) => {
                    let map = data
                        .and_then(|d| serde_json::from_str(&d).ok())
                        .unwrap_or_default();
                    self.cache = Some(map);
                }
                Err(_) => {
                    self.failed = true;
                    return None;
                }
            }
        }
        self.cache.as_mut()
    }

    /// The secrets saved for `id`.
    pub fn get(&mut self, id: &str) -> Option<ConnectionSecrets> {
        self.map()?.get(id).cloned()
    }

    /// Keeps `secrets` for `id` (removing them when empty). False when the
    /// vault can't be used, so the caller keeps them itself.
    pub fn put(&mut self, id: &str, secrets: ConnectionSecrets) -> bool {
        let Some(map) = self.map() else { return false };
        let before = map.get(id).cloned();
        if before.as_ref() == Some(&secrets) || (before.is_none() && secrets.is_empty()) {
            return true;
        }
        if secrets.is_empty() {
            map.remove(id);
        } else {
            map.insert(id.to_string(), secrets);
        }
        let data = serde_json::to_string(&*map).unwrap_or_default();
        if self.vault.save(&data).is_ok() {
            return true;
        }
        // Undo, and stop using the vault this run.
        let map = self.cache.as_mut().unwrap();
        match before {
            Some(b) => map.insert(id.to_string(), b),
            None => map.remove(id),
        };
        self.failed = true;
        false
    }
}

/// A vault in memory, for tests.
#[derive(Default, Clone)]
pub struct MemoryVault(pub std::sync::Arc<std::sync::Mutex<Option<String>>>);

impl SecretVault for MemoryVault {
    fn load(&self) -> Result<Option<String>, String> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn save(&self, data: &str) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(data.to_string());
        Ok(())
    }
}
