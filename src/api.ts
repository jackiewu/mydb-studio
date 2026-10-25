import { Channel, invoke } from "@tauri-apps/api/core";

// Mirrors of the Rust types in crates/mydb-core. Keep in sync.

export type SslMode = "disabled" | "required" | "verify_ca" | "verify_identity";

export interface SslConfig {
  mode: SslMode;
  ca_path?: string | null;
  cert_path?: string | null;
  key_path?: string | null;
  /** The cloud service the settings were filled in for. */
  cloud?: "aws" | "gcp" | "azure" | null;
}

export type SshAuth =
  | { method: "password"; password: string }
  | { method: "key"; key_path: string; passphrase?: string | null };

export interface SshConfig {
  host: string;
  port: number;
  user: string;
  auth: SshAuth;
  known_host_fingerprint?: string | null;
}

export type ConnectionKind = "mysql" | "redis";

export interface ConnectionProfile {
  id: string;
  name: string;
  /** Missing on profiles saved before Redis support: MySQL. */
  kind?: ConnectionKind;
  host: string;
  port: number;
  user: string;
  password?: string | null;
  database?: string | null;
  ssl: SslConfig;
  ssh?: SshConfig | null;
  favorite?: boolean;
  last_used_at?: string | null;
  /** What AI tools may do through the MCP server (default read_only). MySQL only. */
  mcp?: McpAccess;
  /** Redis: database 0–15 selected after connecting. */
  redis_db?: number;
}

export type McpAccess = "off" | "read_only" | "read_write";

export interface McpSetup {
  command: string;
  config_json: string;
  claude_code: string;
}

export interface SessionInfo {
  server_version: string;
  connection_id: number;
  tls: boolean;
  ssh_host_fingerprint?: string | null;
}

export interface OpenedSession {
  session_id: string;
  connection_id: string;
  kind: ConnectionKind;
  info: SessionInfo;
  current_database?: string | null;
  /** Redis: the database selected. */
  redis_db?: number | null;
}

export interface ColumnInfo {
  name: string;
  table: string;
  type_name: string;
}

export type Cell = string | number | null;

export interface StatementResult {
  sql: string;
  duration_ms: number;
  columns: ColumnInfo[];
  rows: Cell[][];
  total_rows: number;
  truncated: boolean;
  affected_rows: number;
  last_insert_id?: number | null;
  warnings: number;
  info: string;
  /** The LIMIT the app added; `sql` is the statement as written. */
  limit_added?: number | null;
}

export type CoreError =
  | { kind: "server"; code: number; state: string; message: string }
  | { kind: "host_key_mismatch"; host: string; expected: string; actual: string }
  | { kind: "driver" | "connection" | "ssh" | "io" | "store" | "config" | "redis"; message: string }
  | { kind: "not_connected"; id: string }
  | { kind: "cancelled" };

export interface ExecutionOutcome {
  started_at: string;
  duration_ms: number;
  results: StatementResult[];
  error?: CoreError | null;
  failed_statement?: string | null;
  reconnected: boolean;
  /** The user pressed 停止; `error` is where it stopped. */
  cancelled?: boolean;
  /** Set when recording was on for this run. */
  record_id?: number | null;
  /** The database in use after the run (a USE may have changed it). */
  current_database?: string | null;
  /** What the error means and how to fix it, when known. */
  advice?: Advice | null;
  /** Pre-run check results for the SQL that ran. */
  findings?: Finding[];
}

export interface Advice {
  title: string;
  explanation: string;
  fixes: string[];
}

export interface Finding {
  /** Index of the statement within the SQL. */
  statement: number;
  /** The statement's first line. */
  sql: string;
  level: "danger" | "warn" | "info";
  rule: string;
  message: string;
  suggestion: string;
}

export interface RecordedStatement {
  sql: string;
  duration_ms: number;
  affected_rows: number;
  returned_rows?: number | null;
  warnings: number;
  info: string;
  columns: string[];
  preview: Cell[][];
}

export interface ExecutionRecord {
  id: number;
  connection_id?: string | null;
  connection_name: string;
  connection_kind?: ConnectionKind;
  server: string;
  server_version: string;
  database?: string | null;
  started_at: string;
  duration_ms: number;
  sql: string;
  /** Empty in list results; filled by getRecord. */
  statements: RecordedStatement[];
  statement_count: number;
  affected_rows: number;
  returned_rows: number;
  error_code?: number | null;
  error_message?: string | null;
  failed_statement?: string | null;
  note: string;
}

export interface RecordFilter {
  connection_id?: string | null;
  connection_kind?: ConnectionKind | null;
  text?: string | null;
  from?: string | null;
  to?: string | null;
  only_errors?: boolean;
  limit?: number;
  offset?: number;
}

export type ReportFormat = "markdown" | "html" | "csv";

export interface ReportOptions {
  format: ReportFormat;
  title?: string | null;
  include_results: boolean;
  max_result_rows: number;
  /** Leave out hosts, accounts and IP addresses. */
  hide_connection: boolean;
}

export interface DatabaseInfo {
  name: string;
  table_count: number;
}

export interface TableInfo {
  name: string;
  kind: string;
  engine?: string | null;
  rows_estimate?: number | null;
  size_bytes?: number | null;
  comment: string;
}

export interface ColumnDef {
  table: string;
  name: string;
  column_type: string;
  /** PRI, UNI, MUL or empty. */
  key: string;
  nullable: boolean;
  comment: string;
}

export interface AppSettings {
  /** Editor theme id from EDITOR_THEMES; "auto" follows the system. */
  editor_theme: string;
  editor_font_size: number;
  max_rows: number;
  record_by_default: boolean;
  show_system_databases: boolean;
  /** Home page: click selects, double-click connects; off, a click connects. */
  connect_on_double_click: boolean;
  /** Open the problem report at startup after a crash; off, only the status bar shows ⚠. */
  report_crash_on_start: boolean;
  /** App lock: "off" or "system" (Touch ID / the login password). */
  lock_mode: string;
  /** Lock after this many idle minutes; 0 never. */
  lock_idle_minutes: number;
  lock_on_sleep: boolean;
  lock_on_start: boolean;
  lock_disconnect: boolean;
  schema_autocomplete: boolean;
  /** Ask before running statements that can destroy data. */
  confirm_dangerous: boolean;
  /** LIMIT added to queries that have none; 0 turns it off. */
  auto_limit: number;
  ai_enabled: boolean;
  /** Empty finds llama-server on PATH or in Homebrew. */
  ai_llama_server: string;
  ai_model_path: string;
  /** Use an already running OpenAI-compatible server instead of starting one. */
  ai_use_external: boolean;
  ai_server_url: string;
  ai_idle_minutes: number;
  /** 0 picks half the CPU threads. */
  ai_threads: number;
}

export interface ModelFile {
  path: string;
  name: string;
  size_bytes: number;
  /** Instruct/chat models follow questions; base models don't. */
  instruct: boolean;
}

export interface TableDdl {
  name: string;
  ddl: string;
}

/** What the AI is told: SQL, errors, plans and table structure, never row data. */
export interface AiContext {
  server: string;
  database: string | null;
  sql: string;
  error: string | null;
  explain: string | null;
  tables: TableDdl[];
  other_tables: string[];
  findings: string[];
}

export interface ChatMessage {
  role: "user" | "assistant";
  content: string;
}

export interface AiChat {
  id: number;
  title: string;
  connection_id: string | null;
  connection_name: string;
  context: AiContext;
  messages: ChatMessage[];
  updated_at: string;
}

export interface AiChatSummary {
  id: number;
  title: string;
  connection_name: string;
  updated_at: string;
  message_count: number;
}

export interface CrashInfo {
  unclean_exit: boolean;
  panic: string | null;
}

export type AiEvent = { kind: "starting" } | { kind: "thinking" } | { kind: "token"; text: string };

export interface AiStatus {
  llama_server: string | null;
  model: ModelFile | null;
  found_models: ModelFile[];
  models_dir: string;
  cpu_threads: number;
}

export interface ExportOptions {
  format: "tsv" | "csv" | "json" | "insert" | "insert_batch" | "markdown";
  header: boolean;
  table?: string | null;
}

export interface ServerOverview {
  version: string;
  current_user: string;
  status: Record<string, string>;
  variables: Record<string, string>;
}

export interface ProcessInfo {
  id: number;
  user: string;
  host: string;
  db: string | null;
  command: string;
  time: number;
  state: string | null;
  info: string | null;
}

export interface UserAccount {
  user: string;
  host: string;
  plugin: string;
  locked: boolean;
}

/** `*.*` when both are null, `db.*` or `db.table`. */
export interface GrantTarget {
  database: string | null;
  table: string | null;
}

export interface Grant {
  privileges: string[];
  target: GrantTarget | null;
  grant_option: boolean;
  text: string;
  editable: boolean;
}

export interface PrivilegeEdit {
  target: GrantTarget;
  before: string[];
  before_grant_option: boolean;
  after: string[];
  after_grant_option: boolean;
}

export type AdminChange =
  | { kind: "create_user"; user: string; host: string; password: string; privileges: PrivilegeEdit[] }
  | { kind: "set_password"; user: string; host: string; password: string }
  | { kind: "set_locked"; user: string; host: string; locked: boolean }
  | { kind: "drop_user"; user: string; host: string }
  | { kind: "privileges"; user: string; host: string; edits: PrivilegeEdit[] }
  | { kind: "kill"; id: number; query_only: boolean };

export const DEFAULT_SETTINGS: AppSettings = {
  editor_theme: "auto",
  editor_font_size: 13,
  max_rows: 1000,
  record_by_default: false,
  show_system_databases: false,
  connect_on_double_click: true,
  report_crash_on_start: false,
  lock_mode: "off",
  lock_idle_minutes: 15,
  lock_on_sleep: true,
  lock_on_start: true,
  lock_disconnect: false,
  schema_autocomplete: true,
  confirm_dangerous: true,
  auto_limit: 1000,
  ai_enabled: false,
  ai_llama_server: "",
  ai_model_path: "",
  ai_use_external: false,
  ai_server_url: "http://127.0.0.1:39282",
  ai_idle_minutes: 10,
  ai_threads: 0,
};

export interface Snippet {
  /** 0 until saved. */
  id: number;
  name: string;
  prefix: string;
  /** SQL; ${name} marks a field to fill in. */
  body: string;
}

export interface PinnedTab {
  /** 0 until saved. */
  id: number;
  connection_id: string;
  title: string;
  sql: string;
  position: number;
  /** False for a draft kept only in case the app closes unexpectedly. */
  pinned: boolean;
  file_path: string | null;
}

export function newProfile(kind: ConnectionKind = "mysql"): ConnectionProfile {
  if (kind === "redis")
    return {
      id: "",
      name: "",
      kind: "redis",
      host: "127.0.0.1",
      port: 6379,
      user: "",
      password: "",
      database: null,
      ssl: { mode: "disabled" },
      ssh: null,
      mcp: "off",
      redis_db: 0,
    };
  return {
    id: "",
    name: "",
    kind: "mysql",
    host: "127.0.0.1",
    port: 3306,
    user: "root",
    password: "",
    database: "",
    ssl: { mode: "disabled" },
    ssh: null,
    mcp: "read_only",
  };
}

/** `user@host:port`, or `host:port` for Redis without an ACL user. */
export function serverLabel(p: ConnectionProfile) {
  return p.kind === "redis" && !p.user ? `${p.host}:${p.port}` : `${p.user}@${p.host}:${p.port}`;
}

export const KIND_LABEL: Record<ConnectionKind, string> = { mysql: "MySQL", redis: "Redis" };

// Redis, mirroring crates/mydb-core/src/redis.

export interface KeyName {
  key: string;
  /** `key` is hex: the key isn't UTF-8. */
  binary?: boolean;
}

export interface KeyEntry extends KeyName {
  kind: string;
  /** Seconds; -1 never expires. */
  ttl: number;
}

export interface ScanPage {
  /** "0" when every key was seen. */
  cursor: string;
  keys: KeyEntry[];
}

export interface DbInfo {
  index: number;
  keys: number;
  expires: number;
}

export interface KeyInfo {
  kind: string;
  ttl: number;
  encoding?: string | null;
  memory_bytes?: number | null;
  length?: number | null;
}

export interface ValuePage {
  kind: string;
  text?: string | null;
  binary: boolean;
  truncated: boolean;
  columns: string[];
  rows: Cell[][];
  /** Start of the next page; null at the end. */
  next?: string | null;
}

export type Reply =
  | { type: "nil" }
  | { type: "integer"; value: number }
  | { type: "text"; value: string; binary: boolean }
  | { type: "status"; value: string }
  | { type: "double"; value: number }
  | { type: "bool"; value: boolean }
  | { type: "array"; items: Reply[] }
  | { type: "map"; entries: [Reply, Reply][] }
  | { type: "error"; message: string };

export interface CommandResult {
  line: number;
  /** Passwords masked. */
  command: string;
  duration_ms: number;
  reply: Reply;
}

export interface RedisOutcome {
  started_at: string;
  duration_ms: number;
  results: CommandResult[];
  error?: CoreError | null;
  failed_command?: string | null;
  reconnected: boolean;
  cancelled: boolean;
  db: number;
  record_id?: number | null;
}

/** Plain words for the TLS failures people run into with cloud servers. */
function tlsHint(message: string) {
  if (/UnknownIssuer/i.test(message))
    return "\n伺服器的憑證不是由指定的 CA 簽發。請確認 SSL/TLS 頁的 CA 檔和「雲端服務」選項是否正確。";
  if (/NotValidForName|CertNotValidForName/i.test(message))
    return "\n憑證上的主機名稱和你填的「主機」不同。請改填憑證上的名稱（例如 RDS endpoint），或把模式改成「加密，驗證 CA」。";
  if (/Expired/i.test(message)) return "\n伺服器的憑證已過期。";
  return "";
}

/** `p` without its database password, SSH password or key passphrase. */
export function withoutSecrets(p: ConnectionProfile): ConnectionProfile {
  const ssh = p.ssh && {
    ...p.ssh,
    auth:
      p.ssh.auth.method === "password"
        ? { method: "password" as const, password: "" }
        : { ...p.ssh.auth, passphrase: null },
  };
  return { ...p, password: null, ssh };
}

export function describeError(e: unknown): string {
  if (typeof e === "string") return e;
  const err = e as CoreError;
  switch (err?.kind) {
    case "server":
      return `錯誤 ${err.code} (${err.state})：${err.message}`;
    case "host_key_mismatch":
      return `SSH 主機金鑰已變更（${err.host}）。記錄的是 ${err.expected}，這次收到 ${err.actual}。如果確定主機有換金鑰，請在連線設定中清除指紋後再連。`;
    case "connection":
      return `連線中斷：${err.message}${tlsHint(err.message)}`;
    case "not_connected":
      return "連線已關閉，請重新連線。";
    case "cancelled":
      return "已停止。";
    case undefined:
      return String(e);
    default:
      return err.message;
  }
}

export const api = {
  listConnections: () => invoke<ConnectionProfile[]>("list_connections"),
  saveConnection: (profile: ConnectionProfile) =>
    invoke<ConnectionProfile>("save_connection", { profile }),
  deleteConnection: (id: string) => invoke<void>("delete_connection", { id }),
  setFavorite: (id: string, favorite: boolean) => invoke<void>("set_favorite", { id, favorite }),
  listServerDatabases: (profile: ConnectionProfile) => invoke<string[]>("list_server_databases", { profile }),
  awsRdsCaPath: () => invoke<string>("aws_rds_ca_path"),
  testConnection: (profile: ConnectionProfile) =>
    invoke<SessionInfo>("test_connection", { profile }),
  openSession: (connectionId: string) =>
    invoke<OpenedSession>("open_session", { connectionId }),
  closeSession: (sessionId: string) => invoke<void>("close_session", { sessionId }),
  execute: (sessionId: string, sql: string, record = false, maxRows?: number | null, autoLimit?: number | null) =>
    invoke<ExecutionOutcome>("execute", { sessionId, sql, maxRows, autoLimit, record }),
  aiStatus: () => invoke<AiStatus>("ai_status"),
  aiRunning: () => invoke<boolean>("ai_running"),
  lockStatus: () => invoke<{ locked: boolean; available: boolean }>("lock_status"),
  lockApp: () => invoke<void>("lock_app"),
  unlockApp: () => invoke<void>("unlock_app"),
  verifyOwner: (reason: string) => invoke<void>("verify_owner", { reason }),
  aiContext: (sessionId: string | null, sql: string, error: string | null, explain: string | null) =>
    invoke<AiContext>("ai_context", { sessionId, sql, error, explain }),
  aiChat: (context: AiContext, messages: ChatMessage[], onEvent: (e: AiEvent) => void) => {
    const channel = new Channel<AiEvent>();
    channel.onmessage = onEvent;
    return invoke<string>("ai_chat", { context, messages, onEvent: channel });
  },
  aiCancel: () => invoke<void>("ai_cancel"),
  saveAiChat: (chat: AiChat) => invoke<number>("save_ai_chat", { chat }),
  listAiChats: () => invoke<AiChatSummary[]>("list_ai_chats"),
  getAiChat: (id: number) => invoke<AiChat | null>("get_ai_chat", { id }),
  deleteAiChat: (id: number) => invoke<void>("delete_ai_chat", { id }),
  checkSql: (sql: string) => invoke<Finding[]>("check_sql", { sql }),
  mcpSetup: () => invoke<McpSetup>("mcp_setup"),
  cancelQuery: (sessionId: string) => invoke<boolean>("cancel_query", { sessionId }),
  /** Runs `sql` again and writes every row to `path`; returns the row count. */
  exportQuery: (sessionId: string, sql: string, options: ExportOptions, path: string) =>
    invoke<number>("export_query", { sessionId, sql, options, path }),
  serverOverview: (sessionId: string) => invoke<ServerOverview>("server_overview", { sessionId }),
  serverProcesses: (sessionId: string) => invoke<ProcessInfo[]>("server_processes", { sessionId }),
  serverUsers: (sessionId: string) => invoke<UserAccount[]>("server_users", { sessionId }),
  userGrants: (sessionId: string, user: string, host: string) =>
    invoke<Grant[]>("user_grants", { sessionId, user, host }),
  /** The statements a change would run, passwords masked. */
  adminPlan: (sessionId: string, change: AdminChange) => invoke<string[]>("admin_plan", { sessionId, change }),
  adminApply: (sessionId: string, change: AdminChange) => invoke<string[]>("admin_apply", { sessionId, change }),
  listDatabases: (sessionId: string) => invoke<DatabaseInfo[]>("list_databases", { sessionId }),
  listTables: (sessionId: string, database: string) =>
    invoke<TableInfo[]>("list_tables", { sessionId, database }),
  listRecords: (filter: RecordFilter) => invoke<ExecutionRecord[]>("list_records", { filter }),
  getRecord: (id: number) => invoke<ExecutionRecord | null>("get_record", { id }),
  setRecordNote: (id: number, note: string) => invoke<void>("set_record_note", { id, note }),
  deleteRecords: (ids: number[]) => invoke<void>("delete_records", { ids }),
  generateReport: (ids: number[], options: ReportOptions) =>
    invoke<string>("generate_report", { ids, options }),
  /** Opens an HTML report in a print window, where it can be saved as PDF. */
  printReport: (html: string) => invoke<void>("print_report", { html }),
  saveTextFile: (path: string, contents: string) => invoke<void>("save_text_file", { path, contents }),
  useDatabase: (sessionId: string, database: string) =>
    invoke<void>("use_database", { sessionId, database }),
  listAllTables: (sessionId: string) => invoke<[string, string][]>("list_all_tables", { sessionId }),
  listColumns: (sessionId: string, database: string) =>
    invoke<ColumnDef[]>("list_columns", { sessionId, database }),
  getSettings: () => invoke<AppSettings>("get_settings"),
  saveSettings: (settings: AppSettings) => invoke<void>("save_settings", { settings }),
  listSnippets: () => invoke<Snippet[]>("list_snippets"),
  saveSnippet: (snippet: Snippet) => invoke<Snippet>("save_snippet", { snippet }),
  deleteSnippet: (id: number) => invoke<void>("delete_snippet", { id }),
  listPinnedTabs: (connectionId: string) => invoke<PinnedTab[]>("list_pinned_tabs", { connectionId }),
  savePinnedTab: (tab: PinnedTab) => invoke<PinnedTab>("save_pinned_tab", { tab }),
  deletePinnedTab: (id: number) => invoke<void>("delete_pinned_tab", { id }),
  deleteDraftTabs: (connectionId: string) => invoke<void>("delete_draft_tabs", { connectionId }),
  redisDatabases: (sessionId: string) => invoke<DbInfo[]>("redis_databases", { sessionId }),
  redisScan: (sessionId: string, db: number, cursor: string, pattern: string) =>
    invoke<ScanPage>("redis_scan", { sessionId, db, cursor, pattern }),
  redisKeyInfo: (sessionId: string, db: number, key: KeyName) =>
    invoke<KeyInfo>("redis_key_info", { sessionId, db, key }),
  redisReadValue: (sessionId: string, db: number, key: KeyName, from: string) =>
    invoke<ValuePage>("redis_read_value", { sessionId, db, key, from }),
  redisCheck: (text: string) => invoke<Finding[]>("redis_check", { text }),
  redisExecute: (sessionId: string, text: string, record: boolean) =>
    invoke<RedisOutcome>("redis_execute", { sessionId, text, record }),
  readTextFile: (path: string) => invoke<string>("read_text_file", { path }),
  takeCrashInfo: () => invoke<CrashInfo | null>("take_crash_info"),
  bugReportBody: (what: string, details: string) => invoke<string>("bug_report_body", { what, details }),
  openBugReport: (title: string, body: string) => invoke<void>("open_bug_report", { title, body }),
};

/** Quotes an identifier with backticks. */
export function quoteIdent(name: string): string {
  return "`" + name.replace(/`/g, "``") + "`";
}
