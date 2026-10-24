//! Local AI model support: finding llama.cpp and model files.
//!
//! The model only ever runs on this computer, through llama.cpp's
//! `llama-server`. Nothing here is required for the rest of the app.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// Where `llama-server` is usually installed, besides `PATH`.
const SERVER_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"];

/// Finds `llama-server`: `configured` when it is set (and exists), else the
/// first one on `PATH` or in the usual install locations.
pub fn find_llama_server(configured: &str) -> Option<PathBuf> {
    if !configured.trim().is_empty() {
        let p = PathBuf::from(configured.trim());
        return p.is_file().then_some(p);
    }
    let name = if cfg!(windows) {
        "llama-server.exe"
    } else {
        "llama-server"
    };
    let path_dirs = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    path_dirs
        .into_iter()
        .chain(SERVER_DIRS.iter().map(PathBuf::from))
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// A `.gguf` model file found on disk.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelFile {
    pub path: String,
    pub name: String,
    pub size_bytes: u64,
    /// Base (completion) models don't follow instructions well; the advisor
    /// needs an instruct or chat model.
    pub instruct: bool,
}

impl ModelFile {
    pub fn from_path(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        let name = path.file_name()?.to_string_lossy().into_owned();
        let lower = name.to_lowercase();
        Some(Self {
            path: path.to_string_lossy().into_owned(),
            instruct: ["instruct", "chat", "-it", "_it", "coder"]
                .iter()
                .any(|w| lower.contains(w))
                && !lower.contains("base"),
            name,
            size_bytes: meta.len(),
        })
    }
}

/// Folders searched for models: the app's own, llama.cpp's download cache,
/// and a few common places.
pub fn model_dirs(app_models: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![app_models.to_path_buf()];
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(home.join("Library/Caches/llama.cpp"));
        dirs.push(home.join(".cache/llama.cpp"));
        dirs.push(home.join("models"));
        dirs.push(home.join(".cache/lm-studio/models"));
        dirs.push(home.join(".lmstudio/models"));
        dirs.push(home.join("Library/Application Support/CodeZhuyin/models"));
    }
    dirs
}

/// `.gguf` files under `dirs` (two levels deep), by path.
pub fn find_models(dirs: &[PathBuf]) -> Vec<ModelFile> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<ModelFile>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if depth > 0 {
                    walk(&path, depth - 1, out);
                }
            } else if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("gguf"))
            {
                // A model split into parts is loaded from its first part.
                let p = path.to_string_lossy();
                let first_part = !p.contains("-of-0") || p.contains("-00001-of-");
                if first_part && let Some(m) = ModelFile::from_path(&path) {
                    out.push(m);
                }
            }
        }
    }
    let mut out = Vec::new();
    for d in dirs {
        walk(d, 2, &mut out);
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out.dedup_by(|a, b| a.path == b.path);
    out
}

/// What the AI settings page shows.
#[derive(Debug, Clone, Serialize)]
pub struct AiStatus {
    /// Path of `llama-server`, when found.
    pub llama_server: Option<String>,
    /// The configured model file, when it exists.
    pub model: Option<ModelFile>,
    /// Models found on disk to choose from.
    pub found_models: Vec<ModelFile>,
    /// Where the app keeps downloaded models.
    pub models_dir: String,
    /// Usable CPU threads, to suggest a limit.
    pub cpu_threads: usize,
}

pub fn status(llama_server: &str, model_path: &str, app_models: &Path) -> AiStatus {
    AiStatus {
        llama_server: find_llama_server(llama_server).map(|p| p.to_string_lossy().into_owned()),
        model: (!model_path.trim().is_empty())
            .then(|| ModelFile::from_path(Path::new(model_path.trim())))
            .flatten(),
        found_models: find_models(&model_dirs(app_models)),
        models_dir: app_models.to_string_lossy().into_owned(),
        cpu_threads: std::thread::available_parallelism().map_or(4, |n| n.get()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_models_and_tells_instruct_from_base() {
        let dir = std::env::temp_dir().join(format!("mydb-ai-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("qwen2.5-coder-7b-instruct-q4_k_m.gguf"), b"x").unwrap();
        std::fs::write(dir.join("sub/Breeze-7B-Base-v1_0.Q3_K_M.gguf"), b"xy").unwrap();
        std::fs::write(dir.join("big-00002-of-00003.gguf"), b"x").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        let found = find_models(std::slice::from_ref(&dir));
        let names: Vec<(&str, bool)> = found
            .iter()
            .map(|m| (m.name.as_str(), m.instruct))
            .collect();
        assert_eq!(
            names,
            [
                ("qwen2.5-coder-7b-instruct-q4_k_m.gguf", true),
                ("Breeze-7B-Base-v1_0.Q3_K_M.gguf", false)
            ]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn configured_server_must_exist() {
        assert_eq!(find_llama_server("/no/such/llama-server"), None);
    }
}
