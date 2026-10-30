//! Moving saved connections to another computer: a JSON file without any
//! passwords, key passphrases or key contents.
//!
//! The file keeps each profile as the app saves it (minus secrets and the
//! last-used time), so fields a build doesn't know, such as ones added in a
//! later version, are simply ignored on import.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};
use crate::profile::{ConnectionProfile, SshAuth};
use crate::secrets;
use crate::store::Store;

/// What the file says it is, so a stray JSON file isn't taken for one.
pub const FORMAT: &str = "mydb-studio-connections";
pub const VERSION: u32 = 1;

/// The name the app gives its copy of the AWS RDS CA bundle.
const AWS_CA_FILE: &str = "aws-rds-global-bundle.pem";

#[derive(Serialize, Deserialize)]
struct ExportFile {
    format: String,
    version: u32,
    #[serde(default)]
    exported_at: Option<String>,
    #[serde(default)]
    connections: Vec<serde_json::Value>,
}

/// `profile` as it goes into the file: no secrets, no last-used time, and
/// paths under the home folder written as `~/…` so they fit another user
/// name.
fn exportable(profile: &ConnectionProfile, home: Option<&Path>) -> ConnectionProfile {
    let (mut p, _) = secrets::strip(profile);
    p.last_used_at = None;
    let tilde = |path: &mut Option<String>| {
        if let Some(s) = path.as_mut() {
            *s = to_tilde(s, home);
        }
    };
    tilde(&mut p.ssl.ca_path);
    tilde(&mut p.ssl.cert_path);
    tilde(&mut p.ssl.key_path);
    if let Some(ssh) = p.ssh.as_mut()
        && let SshAuth::Key { key_path, .. } = &mut ssh.auth
    {
        *key_path = to_tilde(key_path, home);
    }
    p
}

fn to_tilde(path: &str, home: Option<&Path>) -> String {
    let Some(home) = home.and_then(|h| h.to_str()).filter(|h| !h.is_empty()) else {
        return path.to_string();
    };
    let home = home.trim_end_matches(['/', '\\']);
    match path.strip_prefix(home) {
        Some(rest) if rest.starts_with(['/', '\\']) => format!("~{}", rest.replace('\\', "/")),
        _ => path.to_string(),
    }
}

fn from_tilde(path: &str, home: Option<&Path>) -> String {
    match (path.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest).to_string_lossy().into_owned(),
        _ => path.to_string(),
    }
}

/// The export file for `profiles`.
pub fn export(profiles: &[ConnectionProfile], home: Option<&Path>, now: &str) -> Result<String> {
    let connections = profiles
        .iter()
        .map(|p| serde_json::to_value(exportable(p, home)))
        .collect::<serde_json::Result<_>>()?;
    let file = ExportFile {
        format: FORMAT.into(),
        version: VERSION,
        exported_at: Some(now.to_string()),
        connections,
    };
    Ok(serde_json::to_string_pretty(&file)?)
}

/// Where this computer keeps things the imported paths may refer to.
#[derive(Default)]
pub struct Local<'a> {
    pub home: Option<&'a Path>,
    /// The app's own copy of the AWS RDS CA bundle.
    pub aws_ca: Option<&'a str>,
}

/// The connections read from an export file, before anything is saved.
#[derive(Debug, Serialize)]
pub struct Parsed {
    pub connections: Vec<ConnectionProfile>,
    /// Entries that couldn't be read, as "#n: reason".
    pub skipped: Vec<String>,
}

/// Reads an export file. Secrets are dropped even if a hand-made file has
/// them, paths are made to fit this computer, and entries without an id
/// get one.
pub fn parse(text: &str, local: &Local) -> Result<Parsed> {
    let not_ours = || CoreError::Config {
        message: "這不是 MyDB Studio 匯出的連線檔。".into(),
    };
    let file: ExportFile = serde_json::from_str(text).map_err(|_| not_ours())?;
    if file.format != FORMAT {
        return Err(not_ours());
    }
    let mut out = Parsed {
        connections: Vec::new(),
        skipped: Vec::new(),
    };
    for (i, mut value) in file.connections.into_iter().enumerate() {
        // Without an id, serde would refuse the entry.
        if let Some(obj) = value.as_object_mut()
            && !obj
                .get("id")
                .is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty()))
        {
            obj.insert("id".into(), uuid::Uuid::new_v4().to_string().into());
        }
        match serde_json::from_value::<ConnectionProfile>(value) {
            Ok(p) => out.connections.push(localize(p, local)),
            Err(e) => out.skipped.push(format!("#{}: {e}", i + 1)),
        }
    }
    Ok(out)
}

fn localize(profile: ConnectionProfile, local: &Local) -> ConnectionProfile {
    let (mut p, _) = secrets::strip(&profile);
    p.last_used_at = None;
    // The database and Redis code read TLS files as given, without `~`.
    for path in [
        &mut p.ssl.ca_path,
        &mut p.ssl.cert_path,
        &mut p.ssl.key_path,
    ] {
        if let Some(s) = path.as_mut() {
            *s = from_tilde(s, local.home);
        }
    }
    // The bundled AWS CA lives in the app's folder, which differs by user.
    if p.ssl.cloud.as_deref() == Some("aws")
        && let (Some(ca), Some(here)) = (p.ssl.ca_path.as_mut(), local.aws_ca)
        && Path::new(ca.replace('\\', "/").as_str()).file_name() == Some(AWS_CA_FILE.as_ref())
    {
        *ca = here.to_string();
    }
    p
}

/// Files `profile` names that aren't on this computer.
pub fn missing_files(profile: &ConnectionProfile, home: Option<&Path>) -> Vec<String> {
    let mut paths: Vec<&str> = [
        &profile.ssl.ca_path,
        &profile.ssl.cert_path,
        &profile.ssl.key_path,
    ]
    .into_iter()
    .filter_map(|p| p.as_deref())
    .collect();
    if let Some(ssh) = &profile.ssh
        && let SshAuth::Key { key_path, .. } = &ssh.auth
    {
        paths.push(key_path);
    }
    paths
        .into_iter()
        .map(str::trim)
        .filter(|p| !p.is_empty() && !Path::new(&from_tilde(p, home)).exists())
        .map(str::to_string)
        .collect()
}

/// `name`, or `name (2)`, `name (3)` … whichever no saved connection has.
pub fn unique_name(name: &str, taken: &[String]) -> String {
    let free = |n: &str| {
        !taken
            .iter()
            .any(|t| t.trim().eq_ignore_ascii_case(n.trim()))
    };
    if free(name) {
        return name.to_string();
    }
    (2..)
        .map(|i| format!("{name} ({i})"))
        .find(|n| free(n))
        .unwrap()
}

/// What to do with one imported connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportAction {
    /// Add it as it is.
    New,
    /// Overwrite the saved connection `target`.
    Replace,
    /// Add it beside the one with the same name, under a numbered name.
    Copy,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImportItem {
    pub profile: ConnectionProfile,
    pub action: ImportAction,
    #[serde(default)]
    pub target: Option<String>,
}

/// Saves imported connections. Returns them as saved, without passwords.
/// A replaced connection that still points at the same servers keeps the
/// passwords saved for it here.
pub fn save(store: &Store, items: Vec<ImportItem>) -> Result<Vec<ConnectionProfile>> {
    let mut saved = store.list_connections()?;
    let mut out = Vec::new();
    for item in items {
        // Passwords never come from a file.
        let (mut profile, _) = secrets::strip(&item.profile);
        profile.last_used_at = None;
        match item.action {
            ImportAction::Replace => {
                let target = item.target.unwrap_or_default();
                let Some(existing) = store.get_connection(&target)? else {
                    return Err(CoreError::Config {
                        message: format!("找不到要取代的連線「{}」。", profile.name),
                    });
                };
                profile.id = target;
                let (_, kept) = secrets::strip(&existing);
                if kept.destination == Some(secrets::destination(&profile)) {
                    secrets::fill(&mut profile, &kept);
                }
            }
            ImportAction::New | ImportAction::Copy => {
                if profile.id.is_empty() || saved.iter().any(|s| s.id == profile.id) {
                    profile.id = uuid::Uuid::new_v4().to_string();
                }
                if item.action == ImportAction::Copy {
                    let names: Vec<String> = saved.iter().map(|s| s.name.clone()).collect();
                    profile.name = unique_name(&profile.name, &names);
                }
            }
        }
        store.save_connection(&profile)?;
        let (listed, _) = secrets::strip(&profile);
        saved.retain(|s| s.id != listed.id);
        saved.push(listed.clone());
        out.push(listed);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{SshConfig, SslMode};

    fn sample() -> ConnectionProfile {
        let mut p = ConnectionProfile::new("prod", "db.internal", "app");
        p.password = Some("hunter2".into());
        p.last_used_at = Some("2026-10-01T00:00:00Z".into());
        p.ssl.mode = SslMode::VerifyCa;
        p.ssl.ca_path = Some("/Users/jackie/certs/ca.pem".into());
        p.ssh = Some(SshConfig {
            host: "bastion".into(),
            port: 22,
            user: "jackie".into(),
            auth: SshAuth::Key {
                key_path: "/Users/jackie/.ssh/id_ed25519".into(),
                passphrase: Some("secret-phrase".into()),
            },
            known_host_fingerprint: Some("SHA256:abc".into()),
        });
        p
    }

    #[test]
    fn export_has_no_secrets_and_portable_paths() {
        let text = export(&[sample()], Some(Path::new("/Users/jackie")), "now").unwrap();
        assert!(!text.contains("hunter2") && !text.contains("secret-phrase"));
        assert!(!text.contains("last_used_at\": \"2026"));
        assert!(text.contains("\"~/certs/ca.pem\"") && text.contains("\"~/.ssh/id_ed25519\""));
        // The pinned host key travels, so the new computer still checks it.
        assert!(text.contains("SHA256:abc"));
        let mut ssh_pw = sample();
        ssh_pw.ssh.as_mut().unwrap().auth = SshAuth::Password {
            password: "jump-pw".into(),
        };
        assert!(!export(&[ssh_pw], None, "now").unwrap().contains("jump-pw"));
    }

    #[test]
    fn import_fits_paths_to_this_computer() {
        let text = export(&[sample()], Some(Path::new("/Users/jackie")), "now").unwrap();
        let local = Local {
            home: Some(Path::new("/Users/jackie.w")),
            aws_ca: None,
        };
        let parsed = parse(&text, &local).unwrap();
        let p = &parsed.connections[0];
        assert_eq!(
            p.ssl.ca_path.as_deref(),
            Some("/Users/jackie.w/certs/ca.pem")
        );
        // The SSH code expands `~` itself.
        match &p.ssh.as_ref().unwrap().auth {
            SshAuth::Key {
                key_path,
                passphrase,
            } => {
                assert_eq!(key_path, "~/.ssh/id_ed25519");
                assert!(passphrase.is_none());
            }
            _ => panic!(),
        }
        assert_eq!(p.id, sample_id(&text));
        assert!(p.password.is_none() && p.last_used_at.is_none());
    }

    fn sample_id(text: &str) -> String {
        let v: serde_json::Value = serde_json::from_str(text).unwrap();
        v["connections"][0]["id"].as_str().unwrap().to_string()
    }

    #[test]
    fn import_drops_secrets_in_hand_made_files_and_tolerates_unknown_fields() {
        let text = r#"{
          "format": "mydb-studio-connections", "version": 2, "something_new": true,
          "connections": [
            {"name":"a","host":"h","port":3306,"user":"u","password":"pw","iap":{"zone":"x"}},
            {"name":"broken"},
            {"id":"r1","name":"cache","kind":"redis","host":"h","port":6379,"user":"",
             "ssh":{"host":"j","user":"u","auth":{"method":"password","password":"jump"}}}
          ]}"#;
        let parsed = parse(text, &Local::default()).unwrap();
        assert_eq!(parsed.connections.len(), 2);
        assert_eq!(parsed.skipped.len(), 1);
        assert!(parsed.skipped[0].starts_with("#2"));
        let a = &parsed.connections[0];
        assert!(a.password.is_none() && !a.id.is_empty());
        match &parsed.connections[1].ssh.as_ref().unwrap().auth {
            SshAuth::Password { password } => assert!(password.is_empty()),
            _ => panic!(),
        }
        assert_eq!(parsed.connections[1].id, "r1");
    }

    #[test]
    fn other_json_is_refused() {
        assert!(parse("{}", &Local::default()).is_err());
        assert!(parse(r#"{"format":"x","version":1}"#, &Local::default()).is_err());
        assert!(parse("not json", &Local::default()).is_err());
    }

    #[test]
    fn aws_ca_points_at_this_computers_copy() {
        let mut p = ConnectionProfile::new("rds", "x.rds.amazonaws.com", "admin");
        p.ssl.cloud = Some("aws".into());
        p.ssl.ca_path =
            Some("/Users/old/Library/Application Support/x/certs/aws-rds-global-bundle.pem".into());
        let text = export(&[p], None, "now").unwrap();
        let local = Local {
            home: None,
            aws_ca: Some("/here/aws-rds-global-bundle.pem"),
        };
        let parsed = parse(&text, &local).unwrap();
        assert_eq!(
            parsed.connections[0].ssl.ca_path.as_deref(),
            Some("/here/aws-rds-global-bundle.pem")
        );
    }

    #[test]
    fn missing_files_are_listed() {
        let mut p = sample();
        p.ssl.ca_path = Some("/definitely/not/here.pem".into());
        let missing = missing_files(&p, Some(Path::new("/nowhere")));
        assert_eq!(
            missing,
            vec!["/definitely/not/here.pem", "/Users/jackie/.ssh/id_ed25519"]
        );
    }

    #[test]
    fn names_get_a_number_when_taken() {
        let taken = vec!["prod".to_string(), "Prod (2)".to_string()];
        assert_eq!(unique_name("prod", &taken), "prod (3)");
        assert_eq!(unique_name("dev", &taken), "dev");
    }

    #[test]
    fn saving_imports_keeps_local_passwords_only_for_the_same_servers() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .use_vault(Box::new(crate::secrets::MemoryVault::default()))
            .unwrap();
        let mut here = ConnectionProfile::new("prod", "db.internal", "app");
        here.password = Some("local-pw".into());
        store.save_connection(&here).unwrap();

        let text = export(&[here.clone()], None, "now").unwrap();
        let incoming = parse(&text, &Local::default()).unwrap().connections;
        let item = |action, target: Option<&str>| ImportItem {
            profile: incoming[0].clone(),
            action,
            target: target.map(str::to_string),
        };

        // Same id: new gets a fresh id; copy also a numbered name.
        let added = save(&store, vec![item(ImportAction::New, None)]).unwrap();
        assert_ne!(added[0].id, here.id);
        assert_eq!(added[0].name, "prod");
        let copied = save(&store, vec![item(ImportAction::Copy, None)]).unwrap();
        assert_eq!(copied[0].name, "prod (2)");
        assert!(
            store
                .get_connection(&copied[0].id)
                .unwrap()
                .unwrap()
                .password
                .is_none()
        );

        // Replacing with the same servers keeps the saved password.
        save(&store, vec![item(ImportAction::Replace, Some(&here.id))]).unwrap();
        let kept = store.get_connection(&here.id).unwrap().unwrap();
        assert_eq!(kept.password.as_deref(), Some("local-pw"));

        // Replacing with another server doesn't hand it over.
        let mut elsewhere = item(ImportAction::Replace, Some(&here.id));
        elsewhere.profile.host = "other.example".into();
        save(&store, vec![elsewhere]).unwrap();
        let moved = store.get_connection(&here.id).unwrap().unwrap();
        assert_eq!(moved.host, "other.example");
        assert!(moved.password.is_none());

        // A password slipped into the item is dropped.
        let mut sneaky = item(ImportAction::New, None);
        sneaky.profile.password = Some("from-file".into());
        let s = save(&store, vec![sneaky]).unwrap();
        assert!(
            store
                .get_connection(&s[0].id)
                .unwrap()
                .unwrap()
                .password
                .is_none()
        );

        assert!(save(&store, vec![item(ImportAction::Replace, Some("gone"))]).is_err());
    }
}
