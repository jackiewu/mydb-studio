//! Local AI model support: finding llama.cpp and model files, running
//! `llama-server` on demand, and chatting with it.
//!
//! The model only ever runs on this computer, through llama.cpp's
//! `llama-server`. Nothing here is required for the rest of the app.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};

use crate::error::{CoreError, Result};

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

// ---------------------------------------------------------------------------
// What the model is told

/// One table's structure, sent to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableDdl {
    pub name: String,
    pub ddl: String,
}

/// What the AI is told about the user's situation. Never holds row data:
/// only SQL, errors, query plans and table structure.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiContext {
    /// Server product and version.
    pub server: String,
    pub database: Option<String>,
    pub sql: String,
    pub error: Option<String>,
    /// EXPLAIN output as text.
    pub explain: Option<String>,
    pub tables: Vec<TableDdl>,
    /// Other table names of the current database, for open questions.
    pub other_tables: Vec<String>,
    /// The app's own pre-run check results.
    pub findings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// `system`, `user` or `assistant`.
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn new(role: &str, content: impl Into<String>) -> Self {
        Self {
            role: role.to_string(),
            content: content.into(),
        }
    }
}

/// A saved AI conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiChat {
    /// 0 for one not saved yet.
    #[serde(default)]
    pub id: i64,
    pub title: String,
    #[serde(default)]
    pub connection_id: Option<String>,
    #[serde(default)]
    pub connection_name: String,
    #[serde(default)]
    pub context: AiContext,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub updated_at: String,
}

/// A row of the conversation history list.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AiChatSummary {
    pub id: i64,
    pub title: String,
    pub connection_name: String,
    pub updated_at: String,
    pub message_count: usize,
}

const SYSTEM_PROMPT: &str = "你是資深的 MySQL / MariaDB 資料庫顧問，在一個資料庫管理工具裡協助使用者。\
用繁體中文回答，簡潔直接，先講結論。\
指出問題時說明原因；需要改寫 SQL 時，把完整、可以直接執行的 SQL 放在 ```sql 區塊裡。\
只根據下面提供的資料表結構推論，不要捏造欄位、索引或資料表；資訊不夠就直接說還需要什麼。\
會修改或刪除資料、或會鎖表很久的建議，要提醒風險並建議先備份或在測試環境試。";

/// Cuts `s` to at most `max` characters.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}\n…（以下省略）", s.chars().take(max).collect::<String>())
    }
}

/// The system message: instructions plus everything in `ctx`.
pub fn system_message(ctx: &AiContext) -> ChatMessage {
    let mut text = String::from(SYSTEM_PROMPT);
    text.push_str("\n\n## 環境\n");
    if !ctx.server.is_empty() {
        text.push_str(&format!("伺服器：{}\n", ctx.server));
    }
    text.push_str(&format!(
        "目前資料庫：{}\n",
        ctx.database.as_deref().unwrap_or("（未選擇）")
    ));
    if !ctx.sql.trim().is_empty() {
        text.push_str(&format!(
            "\n## 使用者的 SQL\n```sql\n{}\n```\n",
            clip(ctx.sql.trim(), 6000)
        ));
    }
    if let Some(e) = ctx.error.as_deref().filter(|e| !e.is_empty()) {
        text.push_str(&format!("\n## 執行錯誤\n{}\n", clip(e, 1000)));
    }
    if let Some(p) = ctx.explain.as_deref().filter(|p| !p.is_empty()) {
        text.push_str(&format!("\n## EXPLAIN\n```\n{}\n```\n", clip(p, 3000)));
    }
    if !ctx.findings.is_empty() {
        text.push_str("\n## 工具的規則檢查結果\n");
        for f in &ctx.findings {
            text.push_str(&format!("- {f}\n"));
        }
    }
    if !ctx.tables.is_empty() {
        text.push_str("\n## 相關資料表結構\n");
        for t in &ctx.tables {
            text.push_str(&format!("```sql\n{}\n```\n", clip(&t.ddl, 3000)));
        }
    }
    if !ctx.other_tables.is_empty() {
        let names = ctx.other_tables.join(", ");
        text.push_str(&format!(
            "\n## 目前資料庫的資料表\n{}\n",
            clip(&names, 2000)
        ));
    }
    ChatMessage::new("system", text)
}

/// Rough budget for the conversation, in characters, so it fits the
/// model's context window with room for the answer.
const HISTORY_BUDGET: usize = 12_000;

/// The messages sent to the model: the system message, then as much of
/// `history` (newest kept) as fits.
pub fn build_messages(ctx: &AiContext, history: &[ChatMessage]) -> Vec<ChatMessage> {
    let mut kept = Vec::new();
    let mut used = 0;
    for m in history.iter().rev() {
        used += m.content.chars().count();
        if used > HISTORY_BUDGET && !kept.is_empty() {
            break;
        }
        kept.push(ChatMessage::new(&m.role, clip(&m.content, HISTORY_BUDGET)));
    }
    kept.reverse();
    // Chat templates expect the conversation to start with the user.
    while kept.first().is_some_and(|m| m.role != "user") {
        kept.remove(0);
    }
    let mut out = vec![system_message(ctx)];
    out.extend(kept);
    out
}

// ---------------------------------------------------------------------------
// Running llama-server

/// How to reach the model.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerConfig {
    pub llama_server: String,
    pub model_path: String,
    pub threads: usize,
    pub context: u32,
    /// Use a server the user already runs instead of starting one.
    pub external_url: Option<String>,
}

fn config_err(message: impl Into<String>) -> CoreError {
    CoreError::Config {
        message: message.into(),
    }
}

struct Running {
    child: Child,
    base_url: String,
    config: ServerConfig,
}

/// Starts `llama-server` when first needed and stops it when idle, so the
/// model only takes memory while the user is asking it things.
#[derive(Default)]
pub struct AiRuntime {
    /// Where the model's process id is kept while it runs, so one left
    /// behind by a crash can be stopped on the next start.
    pid_file: Option<PathBuf>,
    running: tokio::sync::Mutex<Option<Running>>,
    last_used: Mutex<Option<Instant>>,
    busy: AtomicUsize,
    cancel: AtomicU64,
}

impl AiRuntime {
    pub fn new(pid_file: PathBuf) -> Self {
        Self {
            pid_file: Some(pid_file),
            ..Default::default()
        }
    }

    /// Stops a model left running by an app that crashed (only if that
    /// process is still a llama-server).
    pub fn stop_leftover(&self) {
        let Some(file) = &self.pid_file else { return };
        let Ok(pid) = std::fs::read_to_string(file) else {
            return;
        };
        let _ = std::fs::remove_file(file);
        let pid = pid.trim();
        if cfg!(unix) && !pid.is_empty() && pid.chars().all(|c| c.is_ascii_digit()) {
            let command = std::process::Command::new("ps")
                .args(["-p", pid, "-o", "command="])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default();
            if command.contains("llama-server") {
                let _ = std::process::Command::new("kill").arg(pid).status();
            }
        }
    }

    fn forget_pid(&self) {
        if let Some(file) = &self.pid_file {
            let _ = std::fs::remove_file(file);
        }
    }

    fn touch(&self) {
        *self.last_used.lock().unwrap() = Some(Instant::now());
    }

    /// Whether a request with `cfg` can be answered without starting a model.
    pub async fn is_ready(&self, cfg: &ServerConfig) -> bool {
        if cfg.external_url.is_some() {
            return true;
        }
        let mut guard = self.running.lock().await;
        guard
            .as_mut()
            .is_some_and(|r| r.config == *cfg && matches!(r.child.try_wait(), Ok(None)))
    }

    /// The base URL of a ready server for `cfg`, starting one if needed.
    pub async fn ensure(&self, cfg: &ServerConfig) -> Result<String> {
        self.touch();
        if let Some(url) = &cfg.external_url {
            self.stop().await;
            return Ok(url.trim().trim_end_matches('/').to_string());
        }
        let mut guard = self.running.lock().await;
        if let Some(r) = guard.as_mut() {
            if r.config == *cfg && matches!(r.child.try_wait(), Ok(None)) {
                return Ok(r.base_url.clone());
            }
            let _ = r.child.kill().await;
            self.forget_pid();
            *guard = None;
        }

        let server = find_llama_server(&cfg.llama_server).ok_or_else(|| {
            config_err("找不到 llama-server。請先安裝 llama.cpp（brew install llama.cpp），或在設定裡指定它的位置。")
        })?;
        let model = cfg.model_path.trim();
        if model.is_empty() {
            return Err(config_err(
                "還沒選擇模型檔，請到設定的「AI 建議」選一個 .gguf 模型。",
            ));
        }
        if !Path::new(model).is_file() {
            return Err(config_err(format!("找不到模型檔：{model}")));
        }
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map_err(|e| config_err(format!("找不到可用的連接埠：{e}")))?
            .port();

        // Lower priority so typing and other apps stay responsive.
        let nice = Path::new("/usr/bin/nice");
        let mut cmd = if cfg!(unix) && nice.is_file() {
            let mut c = Command::new(nice);
            c.args(["-n", "10"]).arg(&server);
            c
        } else {
            Command::new(&server)
        };
        cmd.arg("-m")
            .arg(model)
            .args(["--host", "127.0.0.1", "--port", &port.to_string()])
            .args(["-c", &cfg.context.to_string()])
            .args(["-t", &cfg.threads.max(1).to_string()])
            .args(["-np", "1", "-ngl", "99"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| config_err(format!("無法啟動 llama-server：{e}")))?;

        // Keep the end of its log for error messages, and keep the pipe drained.
        let log = Arc::new(Mutex::new(VecDeque::<String>::new()));
        if let Some(stderr) = child.stderr.take() {
            let log = log.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut log = log.lock().unwrap();
                    if log.len() == 12 {
                        log.pop_front();
                    }
                    log.push_back(line);
                }
            });
        }
        let tail = |log: &Mutex<VecDeque<String>>| {
            log.lock()
                .unwrap()
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        };

        if let (Some(file), Some(pid)) = (&self.pid_file, child.id()) {
            let _ = std::fs::write(file, pid.to_string());
        }
        let base_url = format!("http://127.0.0.1:{port}");
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                return Err(config_err(format!(
                    "模型啟動失敗（{status}）。\n{}",
                    tail(&log)
                )));
            }
            if let Ok((200, _)) = http_request(&base_url, "GET", "/health", None).await {
                break;
            }
            if Instant::now() > deadline {
                let _ = child.kill().await;
                return Err(config_err("模型啟動逾時（超過 3 分鐘）。"));
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
        *guard = Some(Running {
            child,
            base_url: base_url.clone(),
            config: cfg.clone(),
        });
        self.touch();
        Ok(base_url)
    }

    /// Stops the model if nothing used it for `idle`.
    pub async fn stop_if_idle(&self, idle: Duration) {
        if self.busy.load(Ordering::SeqCst) > 0 {
            return;
        }
        let stale = self
            .last_used
            .lock()
            .unwrap()
            .is_some_and(|t| t.elapsed() >= idle);
        if stale {
            self.stop().await;
        }
    }

    /// Whether the app's own model process is running now.
    pub async fn is_running(&self) -> bool {
        self.running
            .lock()
            .await
            .as_mut()
            .is_some_and(|r| matches!(r.child.try_wait(), Ok(None)))
    }

    pub async fn stop(&self) {
        if let Some(mut r) = self.running.lock().await.take() {
            let _ = r.child.kill().await;
            self.forget_pid();
        }
    }

    /// For app exit, outside async code: asks the model process to stop.
    pub fn kill_now(&self) {
        if let Ok(mut guard) = self.running.try_lock()
            && let Some(r) = guard.as_mut()
        {
            let _ = r.child.start_kill();
        }
        self.forget_pid();
    }

    /// Stops answers in progress.
    pub fn cancel(&self) {
        self.cancel.fetch_add(1, Ordering::SeqCst);
    }

    /// Sends `messages` to the server at `base_url`, calling `on_token` with
    /// each piece of the answer as it arrives. Returns the whole answer.
    pub async fn chat(
        &self,
        base_url: &str,
        messages: &[ChatMessage],
        max_tokens: u32,
        mut on_token: impl FnMut(&str),
    ) -> Result<String> {
        struct Busy<'a>(&'a AiRuntime);
        impl Drop for Busy<'_> {
            fn drop(&mut self) {
                self.0.busy.fetch_sub(1, Ordering::SeqCst);
                self.0.touch();
            }
        }
        self.busy.fetch_add(1, Ordering::SeqCst);
        let _busy = Busy(self);
        let epoch = self.cancel.load(Ordering::SeqCst);

        let body = serde_json::json!({
            "model": "local",
            "messages": messages,
            "temperature": 0.2,
            "max_tokens": max_tokens,
            "stream": true,
        })
        .to_string();
        // A model that answers at all sends its headers (and each piece)
        // well within this.
        let wait = Duration::from_secs(300);
        let mut resp = patient(
            wait,
            HttpResponse::send(base_url, "POST", "/v1/chat/completions", Some(&body)),
        )
        .await
        .map_err(|e| config_err(format!("連不上 AI 模型（{base_url}）：{e}")))?;
        if resp.status != 200 {
            let text = patient(wait, resp.read_all()).await.unwrap_or_default();
            return Err(config_err(format!(
                "AI 模型回應錯誤 {}：{}",
                resp.status,
                clip(&String::from_utf8_lossy(&text), 500)
            )));
        }

        let mut answer = String::new();
        if !resp.event_stream {
            // The server ignored `stream`: one JSON answer.
            let text = patient(wait, resp.read_all())
                .await
                .map_err(|e| config_err(e.to_string()))?;
            let v: serde_json::Value = serde_json::from_slice(&text)
                .map_err(|e| config_err(format!("看不懂 AI 模型的回應：{e}")))?;
            let content = v["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or_default();
            on_token(content);
            return Ok(content.to_string());
        }

        let started = std::time::Instant::now();
        let mut pending = Vec::<u8>::new();
        loop {
            if self.cancel.load(Ordering::SeqCst) != epoch {
                return Ok(answer);
            }
            if started.elapsed() > MAX_ANSWER_TIME {
                return Err(config_err("AI 回答太久，已停止。"));
            }
            let chunk = tokio::time::timeout(wait, resp.next_chunk())
                .await
                .map_err(|_| config_err("AI 模型太久沒有回應。"))?
                .map_err(|e| config_err(format!("讀取 AI 回應失敗：{e}")))?;
            let Some(chunk) = chunk else { break };
            pending.extend_from_slice(&chunk);
            if pending.len() > MAX_BODY {
                return Err(config_err("AI 回應的一行太長。"));
            }
            while let Some(nl) = pending.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = pending.drain(..=nl).collect();
                let line = String::from_utf8_lossy(&line);
                let Some(data) = line.trim().strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    return Ok(answer);
                }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                    if let Some(msg) = v["error"]["message"].as_str() {
                        return Err(config_err(format!("AI 模型回應錯誤：{msg}")));
                    }
                    if let Some(piece) = v["choices"][0]["delta"]["content"].as_str()
                        && !piece.is_empty()
                    {
                        if answer.len() + piece.len() > MAX_BODY {
                            return Err(config_err("AI 回應太長。"));
                        }
                        answer.push_str(piece);
                        on_token(piece);
                    }
                }
            }
        }
        Ok(answer)
    }
}

// ---------------------------------------------------------------------------
// A minimal HTTP/1.1 client for the local server

struct Target {
    host: String,
    port: u16,
    prefix: String,
}

fn parse_url(url: &str) -> std::io::Result<Target> {
    let bad = |m: &str| std::io::Error::new(std::io::ErrorKind::InvalidInput, m.to_string());
    let rest = url
        .trim()
        .strip_prefix("http://")
        .ok_or_else(|| bad("只支援 http:// 開頭的本機位址"))?;
    let (host_port, path) = match rest.split_once('/') {
        Some((h, p)) => (h, format!("/{p}")),
        None => (rest, String::new()),
    };
    let mut prefix = path.trim_end_matches('/').to_string();
    // People often paste the OpenAI-style base URL.
    if prefix.ends_with("/v1") {
        prefix.truncate(prefix.len() - 3);
    }
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().map_err(|_| bad("連接埠不正確"))?),
        None => (host_port, 80),
    };
    Ok(Target {
        host: host.trim_matches(['[', ']']).to_string(),
        port,
        prefix,
    })
}

/// Largest chunk, whole body and header line accepted from the local model
/// server.
const MAX_CHUNK: usize = 1 << 20;
const MAX_BODY: usize = 32 << 20;
const MAX_LINE: u64 = 16 << 10;
/// The longest one answer may stream for.
const MAX_ANSWER_TIME: Duration = Duration::from_secs(15 * 60);

/// Reads one line of at most [`MAX_LINE`] bytes; a longer one is an error.
async fn read_line(reader: &mut BufReader<TcpStream>, line: &mut String) -> std::io::Result<usize> {
    let n = (&mut *reader).take(MAX_LINE).read_line(line).await?;
    if n as u64 == MAX_LINE && !line.ends_with('\n') {
        return Err(std::io::Error::other("回應的標頭太長"));
    }
    Ok(n)
}

struct HttpResponse {
    status: u16,
    chunked: bool,
    event_stream: bool,
    reader: BufReader<TcpStream>,
    done: bool,
}

impl HttpResponse {
    async fn send(
        base: &str,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> std::io::Result<Self> {
        let t = parse_url(base)?;
        let stream = tokio::time::timeout(
            Duration::from_secs(5),
            TcpStream::connect((t.host.as_str(), t.port)),
        )
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "連線逾時"))??;
        let mut reader = BufReader::new(stream);
        let body = body.unwrap_or("");
        let head = format!(
            "{method} {}{path} HTTP/1.1\r\nHost: {}:{}\r\nContent-Type: application/json\r\n\
             Accept: text/event-stream, application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            t.prefix,
            t.host,
            t.port,
            body.len()
        );
        let s = reader.get_mut();
        s.write_all(head.as_bytes()).await?;
        s.write_all(body.as_bytes()).await?;
        s.flush().await?;

        let mut line = String::new();
        read_line(&mut reader, &mut line).await?;
        let status = line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| std::io::Error::other("不是 HTTP 回應"))?;
        let (mut chunked, mut event_stream) = (false, false);
        loop {
            line.clear();
            if read_line(&mut reader, &mut line).await? == 0 || line.trim().is_empty() {
                break;
            }
            let lower = line.to_ascii_lowercase();
            if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
                chunked = true;
            }
            if lower.starts_with("content-type:") && lower.contains("text/event-stream") {
                event_stream = true;
            }
        }
        Ok(Self {
            status,
            chunked,
            event_stream,
            reader,
            done: false,
        })
    }

    /// The next piece of the body, or `None` at its end.
    async fn next_chunk(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        if self.done {
            return Ok(None);
        }
        if !self.chunked {
            let mut buf = vec![0; 8192];
            let n = self.reader.read(&mut buf).await?;
            if n == 0 {
                self.done = true;
                return Ok(None);
            }
            buf.truncate(n);
            return Ok(Some(buf));
        }
        let mut line = String::new();
        read_line(&mut self.reader, &mut line).await?;
        let size = usize::from_str_radix(line.trim().split(';').next().unwrap_or(""), 16)
            .map_err(|_| std::io::Error::other("分塊格式不正確"))?;
        if size == 0 {
            self.done = true;
            return Ok(None);
        }
        // The size comes from the other end: don't let it pick the allocation.
        if size > MAX_CHUNK {
            return Err(std::io::Error::other("回應的分塊太大"));
        }
        let mut buf = vec![0; size];
        self.reader.read_exact(&mut buf).await?;
        line.clear();
        read_line(&mut self.reader, &mut line).await?;
        Ok(Some(buf))
    }

    async fn read_all(&mut self) -> std::io::Result<Vec<u8>> {
        let mut all = Vec::new();
        while let Some(c) = self.next_chunk().await? {
            all.extend(c);
            if all.len() > MAX_BODY {
                return Err(std::io::Error::other("回應太大"));
            }
        }
        Ok(all)
    }
}

/// `work`, or a time-out error once `limit` passes.
async fn patient<T>(
    limit: Duration,
    work: impl std::future::Future<Output = std::io::Result<T>>,
) -> std::io::Result<T> {
    tokio::time::timeout(limit, work)
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "逾時"))?
}

/// Sends a request and returns its status and body.
async fn http_request(
    base: &str,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> std::io::Result<(u16, Vec<u8>)> {
    // The whole exchange, body included, has the one time limit.
    patient(Duration::from_secs(10), async {
        let mut resp = HttpResponse::send(base, method, path, body).await?;
        let body = resp.read_all().await?;
        Ok((resp.status, body))
    })
    .await
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

    #[test]
    fn urls_accept_openai_style_bases() {
        let t = parse_url("http://127.0.0.1:39282/v1/").unwrap();
        assert_eq!(
            (t.host.as_str(), t.port, t.prefix.as_str()),
            ("127.0.0.1", 39282, "")
        );
        let t = parse_url("http://localhost/llm").unwrap();
        assert_eq!(
            (t.host.as_str(), t.port, t.prefix.as_str()),
            ("localhost", 80, "/llm")
        );
        assert!(parse_url("https://api.example.com").is_err());
    }

    #[test]
    fn context_goes_into_the_system_message() {
        let ctx = AiContext {
            server: "MariaDB 11.4".into(),
            database: Some("shop".into()),
            sql: "SELECT * FROM orders".into(),
            error: Some("Unknown column".into()),
            tables: vec![TableDdl {
                name: "orders".into(),
                ddl: "CREATE TABLE `orders` (id INT)".into(),
            }],
            ..Default::default()
        };
        let history = vec![
            ChatMessage::new("assistant", "stray"),
            ChatMessage::new("user", "為什麼錯？"),
        ];
        let msgs = build_messages(&ctx, &history);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "system");
        for part in [
            "MariaDB 11.4",
            "shop",
            "SELECT * FROM orders",
            "Unknown column",
            "CREATE TABLE `orders`",
        ] {
            assert!(msgs[0].content.contains(part), "{part}");
        }
        assert_eq!(msgs[1].content, "為什麼錯？");
    }

    /// A stand-in for llama-server: answers one streamed chat request.
    async fn fake_server(chunked: bool) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0; 65536];
            let _ = sock.read(&mut buf).await.unwrap();
            let events = [
                r#"data: {"choices":[{"delta":{"content":"加上"}}]}"#,
                r#"data: {"choices":[{"delta":{"content":"索引"}}]}"#,
                "data: [DONE]",
            ];
            let body: String = events.iter().map(|e| format!("{e}\n\n")).collect();
            let resp = if chunked {
                // Split mid-event to test reassembly.
                let (a, b) = body.split_at(20);
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{a}\r\n{:x}\r\n{b}\r\n0\r\n\r\n",
                    a.len(),
                    b.len()
                )
            } else {
                format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n{body}")
            };
            sock.write_all(resp.as_bytes()).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn streams_answers() {
        for chunked in [true, false] {
            let url = fake_server(chunked).await;
            let rt = AiRuntime::default();
            let mut pieces = Vec::new();
            let answer = rt
                .chat(&url, &[ChatMessage::new("user", "hi")], 100, |p| {
                    pieces.push(p.to_string())
                })
                .await
                .unwrap();
            assert_eq!(answer, "加上索引");
            assert_eq!(pieces, ["加上", "索引"]);
        }
    }

    #[tokio::test]
    async fn missing_model_fails_softly() {
        let rt = AiRuntime::default();
        let cfg = ServerConfig {
            llama_server: "/no/such/llama-server".into(),
            model_path: String::new(),
            threads: 2,
            context: 4096,
            external_url: None,
        };
        let err = rt.ensure(&cfg).await.unwrap_err();
        assert!(err.to_string().contains("llama-server"));
    }
}
