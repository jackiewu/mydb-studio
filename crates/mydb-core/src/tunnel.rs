//! SSH local port forwarding (the equivalent of `ssh -L`).
//!
//! The tunnel listens on an ephemeral port on 127.0.0.1 and forwards every
//! accepted connection through a `direct-tcpip` channel to the database host,
//! as seen from the jump host.

use std::sync::{Arc, Mutex};

use russh::client::{self, Handle};
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, load_secret_key};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::error::{CoreError, Result};
use crate::profile::{SshAuth, SshConfig};

pub struct SshTunnel {
    local_port: u16,
    /// Fingerprint of the jump host key seen on this connect.
    host_fingerprint: String,
    accept_task: JoinHandle<()>,
    _session: Arc<Handle<HostKeyCheck>>,
}

impl SshTunnel {
    /// Connects to the jump host and starts forwarding to
    /// `target_host:target_port`.
    pub async fn open(cfg: &SshConfig, target_host: &str, target_port: u16) -> Result<Self> {
        let seen = Arc::new(Mutex::new(None));
        let handler = HostKeyCheck {
            expected: cfg.known_host_fingerprint.clone(),
            seen: seen.clone(),
        };
        let config = Arc::new(client::Config {
            nodelay: true,
            ..Default::default()
        });

        let mut session =
            match client::connect(config, (cfg.host.as_str(), cfg.port), handler).await {
                Ok(s) => s,
                Err(e) => {
                    // A rejected host key surfaces as a generic russh error, so
                    // report the mismatch explicitly when we know about it.
                    if let (Some(expected), Some(actual)) =
                        (&cfg.known_host_fingerprint, seen.lock().unwrap().clone())
                        && *expected != actual
                    {
                        return Err(CoreError::HostKeyMismatch {
                            host: cfg.host.clone(),
                            expected: expected.clone(),
                            actual,
                        });
                    }
                    return Err(e.into());
                }
            };

        let auth =
            match &cfg.auth {
                SshAuth::Password { password } => {
                    session
                        .authenticate_password(cfg.user.clone(), password.clone())
                        .await?
                }
                SshAuth::Key {
                    key_path,
                    passphrase,
                } => {
                    let key = load_secret_key(expand_home(key_path), passphrase.as_deref())
                        .map_err(|e| CoreError::Ssh {
                            message: format!("cannot load key {key_path}: {e}"),
                        })?;
                    let hash = session.best_supported_rsa_hash().await?.flatten();
                    session
                        .authenticate_publickey(
                            cfg.user.clone(),
                            PrivateKeyWithHashAlg::new(Arc::new(key), hash),
                        )
                        .await?
                }
            };
        if !auth.success() {
            return Err(CoreError::Ssh {
                message: format!("authentication failed for {}@{}", cfg.user, cfg.host),
            });
        }

        let host_fingerprint = seen.lock().unwrap().clone().unwrap_or_default();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let local_port = listener.local_addr()?.port();
        let session = Arc::new(session);
        let target_host = target_host.to_string();

        let fwd_session = session.clone();
        let accept_task = tokio::spawn(async move {
            while let Ok((mut socket, peer)) = listener.accept().await {
                let session = fwd_session.clone();
                let target_host = target_host.clone();
                tokio::spawn(async move {
                    let channel = match session
                        .channel_open_direct_tcpip(
                            target_host,
                            target_port as u32,
                            peer.ip().to_string(),
                            peer.port() as u32,
                        )
                        .await
                    {
                        Ok(c) => c,
                        Err(_) => return,
                    };
                    let mut stream = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
                });
            }
        });

        Ok(Self {
            local_port,
            host_fingerprint,
            accept_task,
            _session: session,
        })
    }

    pub fn local_port(&self) -> u16 {
        self.local_port
    }

    pub fn host_fingerprint(&self) -> &str {
        &self.host_fingerprint
    }
}

impl Drop for SshTunnel {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

/// Trust-on-first-use host key check: with no stored fingerprint any key is
/// accepted (and reported back so it can be saved); otherwise it must match.
pub struct HostKeyCheck {
    expected: Option<String>,
    seen: Arc<Mutex<Option<String>>>,
}

impl client::Handler for HostKeyCheck {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        let key = match server_public_key {
            russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } => key.clone(),
            russh::keys::PublicKeyOrCertificate::Certificate(cert) => {
                russh::keys::PublicKey::from(cert.public_key().clone())
            }
        };
        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
        let ok = self.expected.as_ref().is_none_or(|e| *e == fingerprint);
        *self.seen.lock().unwrap() = Some(fingerprint);
        Ok(ok)
    }
}

fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
    {
        return std::path::Path::new(&home)
            .join(rest)
            .to_string_lossy()
            .into_owned();
    }
    path.to_string()
}
