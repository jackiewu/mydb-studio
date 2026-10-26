//! Google Cloud IAP TCP forwarding, by running
//! `gcloud compute start-iap-tunnel` for as long as the connection lives.
//!
//! The app picks a free port on 127.0.0.1, starts gcloud listening there and
//! waits for it to report "Listening on port" (on stdout or stderr,
//! depending on the gcloud release). gcloud does its own test
//! connection first, so a missing permission or a closed firewall shows up
//! as an error here rather than as a dropped database connection later.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

use crate::error::{CoreError, Result};
use crate::profile::IapConfig;

/// gcloud fetches credentials and probes the VM before it listens, which
/// can take a while on a cold start.
const READY_TIMEOUT: Duration = Duration::from_secs(60);

pub struct IapTunnel {
    local_port: u16,
    child: Child,
}

impl IapTunnel {
    /// Starts forwarding 127.0.0.1:<free port> to `remote_port` on the VM.
    pub async fn open(cfg: &IapConfig, remote_port: u16) -> Result<Self> {
        let instance = cfg.instance.trim();
        if instance.is_empty() {
            return Err(CoreError::Config {
                message: "GCP IAP needs the VM instance name".into(),
            });
        }
        let gcloud = find_gcloud(cfg.gcloud_path.as_deref())?;
        let local_port = std::net::TcpListener::bind(("127.0.0.1", 0))?
            .local_addr()?
            .port();

        let mut cmd = Command::new(&gcloud);
        cmd.args([
            "compute",
            "start-iap-tunnel",
            instance,
            &remote_port.to_string(),
            &format!("--local-host-port=127.0.0.1:{local_port}"),
            // IAP's own refusal (e.g. [4010: …]) is only logged at info.
            "--verbosity=info",
        ]);
        for (flag, value) in [("--zone", &cfg.zone), ("--project", &cfg.project)] {
            if !value.trim().is_empty() {
                cmd.arg(format!("{flag}={}", value.trim()));
            }
        }
        // An app started from the Finder gets a bare PATH, but gcloud is a
        // shell script that looks for Python on it.
        cmd.env("PATH", child_path(&gcloud))
            .env("CLOUDSDK_CORE_DISABLE_PROMPTS", "1")
            .env("PYTHONUNBUFFERED", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // Its own process group, so closing the tunnel also stops the Python
        // process the gcloud script starts.
        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd.spawn().map_err(|e| CoreError::Iap {
            message: format!("cannot run gcloud ({}): {e}", gcloud.display()),
        })?;
        // "Listening on port" comes on stdout in recent gcloud releases,
        // errors on stderr. Both are read for as long as gcloud runs, so it
        // never blocks on a full pipe.
        let (tx, mut lines) = tokio::sync::mpsc::unbounded_channel();
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        forward_lines(stdout, tx.clone());
        forward_lines(stderr, tx);
        let mut output = Vec::new();
        let ready = tokio::time::timeout(READY_TIMEOUT, async {
            while let Some(line) = lines.recv().await {
                if line.contains("Listening on port") {
                    return true;
                }
                // gcloud sometimes reports its test connection failing
                // (e.g. [4010: 'destination read failed']) and then listens
                // anyway; nothing would answer behind that port.
                let refused = is_tunnel_error(&line);
                if !line.trim().is_empty() {
                    output.push(line);
                }
                if refused {
                    return false;
                }
            }
            false
        })
        .await;

        let mut tunnel = Self { local_port, child };
        match ready {
            Ok(true) => Ok(tunnel),
            Ok(false) => {
                let _ = tunnel.child.start_kill();
                let status = tunnel.child.wait().await.ok();
                Err(CoreError::Iap {
                    message: failure_message(&output, status.and_then(|s| s.code())),
                })
            }
            Err(_) => Err(CoreError::Iap {
                message: format!(
                    "gcloud did not open the tunnel within {}s. {}",
                    READY_TIMEOUT.as_secs(),
                    failure_message(&output, None)
                )
                .trim()
                .to_string(),
            }),
        }
    }

    pub fn local_port(&self) -> u16 {
        self.local_port
    }
}

impl Drop for IapTunnel {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            // SAFETY: signals the process group gcloud leads; no memory is touched.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGTERM);
            }
        }
        let _ = self.child.start_kill();
    }
}

/// Sends each line of `stream` to `tx` until the stream ends, and keeps
/// reading after nobody listens.
fn forward_lines(
    stream: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    tx: tokio::sync::mpsc::UnboundedSender<String>,
) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = tx.send(line);
        }
    });
}

/// A line where IAP refused or dropped the connection, such as
/// `Error while connecting [4010: 'destination read failed']`.
fn is_tunnel_error(line: &str) -> bool {
    line.find("[40").is_some_and(|i| {
        let code = &line[i + 1..];
        code.len() > 4
            && code[..4].bytes().all(|b| b.is_ascii_digit())
            && code[4..].starts_with(':')
    })
}

/// What gcloud said before giving up: IAP's refusal or gcloud's `ERROR:`
/// and what follows, without progress lines, info logging and the NumPy
/// tip.
fn failure_message(output: &[String], code: Option<i32>) -> String {
    let from = output
        .iter()
        .position(|l| l.starts_with("ERROR:") || is_tunnel_error(l))
        .unwrap_or(0);
    let noise = [
        "Picking local unused port",
        "Testing if tunnel connection works",
        "WARNING:",
        "consider installing NumPy",
        "increasing_the_tcp_upload_bandwidth",
    ];
    let useful: Vec<&str> = output[from..]
        .iter()
        .map(|l| l.trim())
        .filter(|l| !noise.iter().any(|n| l.contains(n)))
        .filter_map(|l| match l.strip_prefix("INFO:") {
            // `INFO: [-1] Received WebSocket Close message [4010: …]`
            Some(rest) if is_tunnel_error(l) => Some(
                rest.trim_start()
                    .trim_start_matches(|c: char| c != ']')
                    .trim_start_matches(']')
                    .trim_start(),
            ),
            Some(_) => None,
            None => Some(l),
        })
        .collect();
    match (useful.is_empty(), code) {
        (false, _) => useful.join("\n"),
        (true, Some(c)) => format!("gcloud exited with status {c} without opening the tunnel"),
        (true, None) => "gcloud stopped without opening the tunnel".into(),
    }
}

#[cfg(windows)]
const GCLOUD_NAMES: &[&str] = &["gcloud.cmd", "gcloud.exe"];
#[cfg(not(windows))]
const GCLOUD_NAMES: &[&str] = &["gcloud"];

/// The gcloud to run: the configured path, else the first one on PATH or
/// in the usual install locations.
pub fn find_gcloud(configured: Option<&str>) -> Result<PathBuf> {
    if let Some(path) = configured.map(str::trim).filter(|p| !p.is_empty()) {
        let path = PathBuf::from(crate::tunnel::expand_home(path));
        return if path.is_file() {
            Ok(path)
        } else {
            Err(CoreError::Iap {
                message: format!("gcloud not found at {}", path.display()),
            })
        };
    }
    search_dirs()
        .iter()
        .flat_map(|dir| GCLOUD_NAMES.iter().map(move |n| dir.join(n)))
        .find(|p| p.is_file())
        .or_else(gcloud_from_login_shell)
        .ok_or_else(|| CoreError::Iap {
            message: "gcloud not found on PATH or in the usual install locations".into(),
        })
}

/// Where the user's own shell finds gcloud. An SDK unpacked anywhere adds
/// itself to PATH from the shell's rc file, which a Finder-launched app
/// never reads.
#[cfg(unix)]
fn gcloud_from_login_shell() -> Option<PathBuf> {
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/zsh".into());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let out = std::process::Command::new(shell)
            .args(["-ilc", "command -v gcloud"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        let _ = tx.send(out);
    });
    // An rc file that hangs must not hang the connect.
    let out = rx.recv_timeout(Duration::from_secs(5)).ok()?.ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .rev()
        .map(|l| PathBuf::from(l.trim()))
        .find(|p| p.is_absolute() && p.is_file())
}

#[cfg(not(unix))]
fn gcloud_from_login_shell() -> Option<PathBuf> {
    None
}

fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    if let Some(home) = &home {
        dirs.push(Path::new(home).join("google-cloud-sdk/bin"));
    }
    for d in [
        "/opt/homebrew/bin",
        "/opt/homebrew/share/google-cloud-sdk/bin",
        "/usr/local/bin",
        "/usr/local/share/google-cloud-sdk/bin",
        "/usr/local/Caskroom/google-cloud-sdk/latest/google-cloud-sdk/bin",
        "/usr/lib/google-cloud-sdk/bin",
        "/snap/bin",
        "/usr/bin",
    ] {
        dirs.push(PathBuf::from(d));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(Path::new(&local).join("Google/Cloud SDK/google-cloud-sdk/bin"));
    }
    if let Some(pf) = std::env::var_os("ProgramFiles(x86)") {
        dirs.push(Path::new(&pf).join("Google/Cloud SDK/google-cloud-sdk/bin"));
    }
    dirs
}

/// PATH for gcloud: its own directory and the common tool directories
/// first, then whatever the app was started with.
fn child_path(gcloud: &Path) -> std::ffi::OsString {
    let mut dirs: Vec<PathBuf> = gcloud.parent().map(Path::to_path_buf).into_iter().collect();
    #[cfg(not(windows))]
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"].map(PathBuf::from));
    if let Some(p) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    std::env::join_paths(dirs).unwrap_or_default()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A stand-in for gcloud that runs `body` with the real arguments.
    fn fake_gcloud(name: &str, body: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("mydb-iap-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("gcloud");
        let args = dir.join("args");
        std::fs::write(
            &script,
            format!("#!/bin/sh\necho \"$@\" > '{}'\n{body}\n", args.display()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        (script, args)
    }

    fn config(gcloud: &Path) -> IapConfig {
        IapConfig {
            instance: "db-vm".into(),
            zone: "asia-east1-b".into(),
            project: "my-proj".into(),
            gcloud_path: Some(gcloud.display().to_string()),
        }
    }

    #[tokio::test]
    async fn waits_for_listening_and_passes_the_target() {
        let (gcloud, args) = fake_gcloud(
            "ok",
            "echo 'Testing if tunnel connection works.' >&2\necho 'Listening on port [1].'\nexec sleep 30",
        );
        let tunnel = IapTunnel::open(&config(&gcloud), 3306).await.unwrap();
        let port = tunnel.local_port();
        let args = std::fs::read_to_string(args).unwrap();
        assert_eq!(
            args.trim(),
            format!(
                "compute start-iap-tunnel db-vm 3306 --local-host-port=127.0.0.1:{port} \
                 --verbosity=info --zone=asia-east1-b --project=my-proj"
            )
        );
        let pid = tunnel.child.id().unwrap() as i32;
        drop(tunnel);
        tokio::time::sleep(Duration::from_millis(300)).await;
        // SAFETY: signal 0 only checks whether the process still exists.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        assert!(!alive, "gcloud kept running after the tunnel closed");
    }

    #[tokio::test]
    async fn reports_what_gcloud_said_when_it_fails() {
        let (gcloud, _) = fake_gcloud(
            "fail",
            "echo 'Testing if tunnel connection works.' >&2\n\
             echo \"ERROR: (gcloud.compute.start-iap-tunnel) Error while connecting [4033: 'not authorized'].\" >&2\n\
             exit 1",
        );
        let Err(CoreError::Iap { message }) = IapTunnel::open(&config(&gcloud), 3306).await else {
            panic!("expected an IAP error");
        };
        assert!(message.contains("4033"), "{message}");
        assert!(!message.contains("Testing"), "{message}");
    }

    #[test]
    fn failure_message_keeps_the_error_only() {
        let out: Vec<String> = [
            "Picking local unused port [40901].",
            "WARNING:",
            "To increase the performance of the tunnel, consider installing NumPy. For instructions,",
            "please see https://cloud.google.com/iap/docs/using-tcp-forwarding#increasing_the_tcp_upload_bandwidth",
            "Testing if tunnel connection works.",
            "ERROR: (gcloud.compute.start-iap-tunnel) Could not fetch resource:",
            " - Request is missing required authentication credential.",
        ]
        .map(String::from)
        .into();
        assert_eq!(
            failure_message(&out, Some(1)),
            "ERROR: (gcloud.compute.start-iap-tunnel) Could not fetch resource:\n\
             - Request is missing required authentication credential."
        );
        let refused: Vec<String> = [
            "Testing if tunnel connection works.",
            "INFO: [-1] Connecting with URL ['wss://tunnel.cloudproxy.app/v4/connect?port=3306']",
            "INFO: [-1] Received WebSocket Close message [4010: 'destination read failed'].",
        ]
        .map(String::from)
        .into();
        assert_eq!(
            failure_message(&refused, None),
            "Received WebSocket Close message [4010: 'destination read failed']."
        );
        assert_eq!(
            failure_message(&out[..2], Some(1)),
            "gcloud exited with status 1 without opening the tunnel"
        );
    }

    /// Runs the installed gcloud against a project that doesn't exist.
    #[tokio::test]
    #[ignore = "needs gcloud installed"]
    async fn real_gcloud_reports_its_error() {
        let cfg = IapConfig {
            instance: "no-such-vm".into(),
            zone: "asia-east1-b".into(),
            project: "no-such-project-mydb".into(),
            gcloud_path: None,
        };
        let Err(CoreError::Iap { message }) = IapTunnel::open(&cfg, 3306).await else {
            panic!("expected an IAP error");
        };
        assert!(message.starts_with("ERROR:"), "{message}");
    }

    #[tokio::test]
    async fn fails_when_the_test_connection_fails_even_if_gcloud_listens() {
        let (gcloud, _) = fake_gcloud(
            "listen-anyway",
            "echo \"Error while connecting [4010: 'destination read failed'].\" >&2\n\
             echo 'Listening on port [1].' >&2\nexec sleep 30",
        );
        let Err(CoreError::Iap { message }) = IapTunnel::open(&config(&gcloud), 3306).await else {
            panic!("expected an IAP error");
        };
        assert!(message.contains("4010"), "{message}");
        assert!(is_tunnel_error(
            "Received WebSocket Close message [4003: 'failed to connect to backend']."
        ));
        assert!(!is_tunnel_error("Listening on port [40123]."));
    }

    #[test]
    fn a_wrong_gcloud_path_is_reported() {
        let err = find_gcloud(Some("/nonexistent/gcloud")).unwrap_err();
        assert!(err.to_string().contains("gcloud not found"), "{err}");
    }
}
