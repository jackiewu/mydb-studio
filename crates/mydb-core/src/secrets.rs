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
    /// Where these secrets were saved for (see [`destination`]). They are
    /// only handed to a profile that still points there, so a changed (or
    /// planted) database can't send them to another server. `None` for
    /// secrets saved before this existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
}

/// The servers a profile's secrets go to, and how they are checked: the
/// database (or Redis) server and its TLS mode, and the SSH jump host and
/// its pinned key. Turning TLS off or clearing the pin changes it too.
pub fn destination(profile: &ConnectionProfile) -> String {
    describe(profile, true)
}

fn describe(profile: &ConnectionProfile, with_tls_files: bool) -> String {
    let at = |user: &str, host: &str, port: u16| {
        format!("{}@{}:{port}", user.trim(), host.trim().to_lowercase())
    };
    let path = |p: &Option<String>| {
        p.as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .unwrap_or("-")
            .to_string()
    };
    let mut d = format!(
        "{:?} {} tls={:?}",
        profile.kind,
        at(&profile.user, &profile.host, profile.port),
        profile.ssl.mode,
    );
    if with_tls_files {
        d.push_str(&format!(
            " ca={} cert={} key={}",
            path(&profile.ssl.ca_path),
            path(&profile.ssl.cert_path),
            path(&profile.ssl.key_path),
        ));
    }
    if let Some(iap) = &profile.iap {
        d.push_str(&format!(
            " iap={}/{}/{}",
            iap.project.trim(),
            iap.zone.trim(),
            iap.instance.trim()
        ));
    }
    if let Some(ssh) = &profile.ssh {
        d.push_str(&format!(
            " via {} key={}",
            at(&ssh.user, &ssh.host, ssh.port),
            ssh.known_host_fingerprint.as_deref().unwrap_or("-")
        ));
        if let Some(sock) = ssh
            .socket_path
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            d.push_str(&format!(" socket={sock}"));
        }
    }
    d
}

impl ConnectionSecrets {
    /// Whether these may be given to `profile`. Secrets saved before they
    /// knew their whole destination fit only when `unbound_ok`.
    pub fn fits(&self, profile: &ConnectionProfile, unbound_ok: bool) -> bool {
        match &self.destination {
            Some(d) if *d == destination(profile) => true,
            // Bound before the TLS files were part of the destination.
            Some(d) => unbound_ok && *d == describe(profile, false),
            None => unbound_ok,
        }
    }

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
        destination: Some(destination(profile)),
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

    /// Forgets the secrets of `id` (a deleted connection).
    pub fn remove(&mut self, id: &str) {
        let Some(map) = self.map() else { return };
        if map.remove(id).is_some() {
            let data = serde_json::to_string(&*map).unwrap_or_default();
            let _ = self.vault.save(&data);
        }
    }

    /// Keeps `secrets` for `id` (removing them when empty). False when the
    /// vault can't be used, so the caller keeps them itself.
    pub fn put(&mut self, id: &str, secrets: ConnectionSecrets) -> bool {
        let Some(map) = self.map() else { return false };
        let before = map.get(id).cloned();
        if before.as_ref() == Some(&secrets) || (before.is_none() && secrets.is_empty()) {
            return true;
        }
        // A profile saved without passwords because the saved ones are for
        // somewhere else (it was changed behind the app's back), or were
        // never bound to it, mustn't wipe them.
        if secrets.is_empty()
            && before
                .as_ref()
                .is_some_and(|b| b.destination != secrets.destination)
        {
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
