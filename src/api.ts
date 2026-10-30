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
  /** Whether editor runs are saved to the records (default auto: follow the app setting). */
  record_mode?: RecordMode;
  /** Redis: database 0–15 selected after connecting. */
  redis_db?: number;
}

export type McpAccess = "off" | "read_only" | "read_write";

/**
 * auto: new tabs follow the app setting; on / off: new tabs start recording
 * or not; forced: every run is recorded and it can't be turned off.
 */
export type RecordMode = "auto" | "on" | "off" | "forced";

/** Whether new tabs on a connection start out recording. */
export function recordsByDefault(mode: RecordMode | undefined, appDefault: boolean) {
  return mode === "on" || mode === "forced" || (mode !== "off" && appDefault);
}

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
  /** Show the run-all button in the toolbar (⌘⇧↵ works either way). */
  show_run_all_button: boolean;
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

export interface Flavor {
  mariadb: boolean;
  version: [number, number, number];
}

export interface CharsetInfo {
  name: string;
  description: string;
  default_collation: string;
  collations: string[];
}

export interface SchemaMeta {
  flavor: Flavor;
  charsets: CharsetInfo[];
  /** Usable engines, the default first. */
  engines: string[];
  server_charset: string;
  server_collation: string;
}

export interface DatabaseOptions {
  name: string;
  charset: string;
  collation: string;
}

export type ObjectKind = "view" | "procedure" | "function" | "trigger" | "event";

export interface SchemaObject {
  kind: ObjectKind;
  name: string;
  detail: string;
  definer: string;
  created: string | null;
  modified: string | null;
  comment: string;
  /** Events only. */
  enabled: boolean | null;
}

export interface ObjectList {
  objects: SchemaObject[];
  /** ON, OFF or DISABLED; null if it could not be read. */
  event_scheduler: string | null;
}

export interface ObjectDefinition {
  definition: string;
  sql_mode: string | null;
}

export interface ColumnDesign {
  /** Name on the server; null for a column being added. */
  original: string | null;
  name: string;
  column_type: string;
  charset: string | null;
  collation: string | null;
  nullable: boolean;
  /** As SQL: 'abc', 0, CURRENT_TIMESTAMP, (uuid()); null for none. */
  default: string | null;
  on_update: string | null;
  auto_increment: boolean;
  /** Expression of a generated column. */
  generated: string | null;
  stored: boolean;
  invisible: boolean;
  comment: string;
}

export type IndexKind = "primary" | "unique" | "index" | "fulltext" | "spatial";

export interface IndexPart {
  column: string;
  length: number | null;
  descending: boolean;
  /** Functional key part (MySQL 8). */
  expression: string | null;
}

export interface IndexDesign {
  original: string | null;
  name: string;
  kind: IndexKind;
  parts: IndexPart[];
  using: string | null;
  comment: string;
}

export interface ForeignKeyDesign {
  original: string | null;
  name: string;
  columns: string[];
  /** null for the table's own database. */
  ref_database: string | null;
  ref_table: string;
  ref_columns: string[];
  on_delete: string;
  on_update: string;
}

export interface TableOptions {
  engine: string | null;
  charset: string | null;
  collation: string | null;
  comment: string;
  auto_increment: number | null;
}

export interface TableDesign {
  name: string;
  columns: ColumnDesign[];
  indexes: IndexDesign[];
  foreign_keys: ForeignKeyDesign[];
  options: TableOptions;
}

export type SchemaChange =
  | { kind: "create_database"; name: string; charset: string | null; collation: string | null }
  | { kind: "alter_database"; name: string; charset: string | null; collation: string | null; convert_tables: boolean }
  | { kind: "drop_database"; name: string }
  | { kind: "create_table"; database: string; design: TableDesign }
  | { kind: "alter_table"; database: string; original: TableDesign; design: TableDesign; convert_charset: boolean }
  | { kind: "drop_table"; database: string; table: string }
  | { kind: "save_object"; database: string; object_kind: ObjectKind; original: string | null; definition: string }
  | { kind: "drop_object"; database: string; object_kind: ObjectKind; name: string }
  | { kind: "set_event_enabled"; database: string; name: string; enabled: boolean }
  | { kind: "set_event_scheduler"; on: boolean };

export interface DdlPlan {
  statements: string[];
  /** What to know before running it. */
  notes: string[];
}

export interface SchemaApplied {
  statements: string[];
  error: CoreError | null;
  record_id: number | null;
  current_database: string | null;
}

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
  show_run_all_button: false,
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

/** The overview numbers of the Redis server page, from INFO. */
export interface RedisSummary {
  version: string;
  mode: string | null;
  uptime_seconds: number;
  connected_clients: number;
  maxclients: number | null;
  blocked_clients: number;
  used_memory: number;
  used_memory_peak: number;
  /** 0 means no limit. */
  maxmemory: number;
  maxmemory_policy: string | null;
  ops_per_sec: number;
  keyspace_hits: number;
  keyspace_misses: number;
  hit_ratio: number | null;
  expired_keys: number;
  evicted_keys: number;
  role: string;
  connected_slaves: number;
  rdb_last_save_time: number | null;
  rdb_last_bgsave_status: string | null;
  rdb_changes_since_last_save: number;
  aof_enabled: boolean;
}

export interface RedisOverview {
  summary: RedisSummary;
  keyspace: DbInfo[];
  info: Record<string, string>;
}

export interface RedisClient {
  id: number;
  addr: string;
  name: string;
  user: string;
  age: number;
  idle: number;
  flags: string;
  db: number;
  cmd: string;
  /** This app's own browse connection. */
  own: boolean;
}

export interface SlowEntry {
  id: number;
  time: number;
  duration_us: number;
  /** Passwords masked. */
  command: string;
  client: string;
  client_name: string;
}

export interface CommandStat {
  name: string;
  calls: number;
  usec: number;
  usec_per_call: number;
  rejected_calls: number;
  failed_calls: number;
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
  /** Without passwords; {@link api.getConnection} has them. */
  listConnections: () => invoke<ConnectionProfile[]>("list_connections"),
  getConnection: (id: string) => invoke<ConnectionProfile>("get_connection", { id }),
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
  /** Opens another app window; with `connectionId` it connects to that. */
  newWindow: (connectionId?: string) => invoke<void>("new_window", { connectionId: connectionId ?? null }),
  /** Brings forward another window that has the connection open; false if none has. */
  focusConnectionWindow: (connectionId: string) =>
    invoke<boolean>("focus_connection_window", { connectionId }),
  execute: (sessionId: string, sql: string, record = false, maxRows?: number | null, autoLimit?: number | null) =>
    invoke<ExecutionOutcome>("execute", { sessionId, sql, maxRows, autoLimit, record }),
  aiStatus: () => invoke<AiStatus>("ai_status"),
  aiRunning: () => invoke<boolean>("ai_running"),
  lockStatus: () => invoke<{ locked: boolean; available: boolean }>("lock_status"),
  lockApp: () => invoke<void>("lock_app"),
  unlockApp: () => invoke<void>("unlock_app"),
  verifyOwner: (reason: string) => invoke<void>("verify_owner", { reason }),
  ownerVerified: () => invoke<boolean>("owner_verified"),
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
  /** Round trip in ms; null while the connection is busy. Never reconnects. */
  sessionPing: (sessionId: string) => invoke<number | null>("session_ping", { sessionId }),
  /** MySQL's wait_timeout in seconds; null for Redis. */
  idleTimeout: (sessionId: string) => invoke<number | null>("idle_timeout", { sessionId }),
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
  schemaMeta: (sessionId: string) => invoke<SchemaMeta>("schema_meta", { sessionId }),
  databaseOptions: (sessionId: string, database: string) =>
    invoke<DatabaseOptions>("database_options", { sessionId, database }),
  tableDesign: (sessionId: string, database: string, table: string) =>
    invoke<TableDesign>("table_design", { sessionId, database, table }),
  showCreateTable: (sessionId: string, database: string, table: string) =>
    invoke<string>("show_create_table", { sessionId, database, table }),
  schemaObjects: (sessionId: string, database: string) =>
    invoke<ObjectList>("schema_objects", { sessionId, database }),
  objectDefinition: (sessionId: string, database: string, kind: ObjectKind, name: string) =>
    invoke<ObjectDefinition>("object_definition", { sessionId, database, kind, name }),
  /** The statements a schema change would run. */
  schemaPlan: (sessionId: string, change: SchemaChange) => invoke<DdlPlan>("schema_plan", { sessionId, change }),
  /** Runs a schema change; it is always kept in the records. */
  schemaApply: (sessionId: string, change: SchemaChange) =>
    invoke<SchemaApplied>("schema_apply", { sessionId, change }),
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
  redisServerOverview: (sessionId: string) => invoke<RedisOverview>("redis_server_overview", { sessionId }),
  redisClients: (sessionId: string) => invoke<RedisClient[]>("redis_clients", { sessionId }),
  redisSlowlog: (sessionId: string) => invoke<SlowEntry[]>("redis_slowlog", { sessionId }),
  redisCommandStats: (sessionId: string) => invoke<CommandStat[]>("redis_command_stats", { sessionId }),
  /** CONFIG GET *, passwords as ****. */
  redisConfig: (sessionId: string) => invoke<[string, string][]>("redis_config", { sessionId }),
  /** CLIENT KILL ID, after the user confirmed it. */
  redisKillClient: (sessionId: string, id: number) => invoke<boolean>("redis_kill_client", { sessionId, id }),
  redisCheck: (text: string) => invoke<Finding[]>("redis_check", { text }),
  redisExecute: (sessionId: string, text: string, record: boolean) =>
    invoke<RedisOutcome>("redis_execute", { sessionId, text, record }),
  readTextFile: (path: string) => invoke<string>("read_text_file", { path }),
  crashInfo: () => invoke<CrashInfo | null>("crash_info"),
  clearCrashInfo: () => invoke<void>("clear_crash_info"),
  bugReportBody: (what: string, details: string) => invoke<string>("bug_report_body", { what, details }),
  openBugReport: (title: string, body: string) => invoke<void>("open_bug_report", { title, body }),
};

/** Quotes an identifier with backticks. */
export function quoteIdent(name: string): string {
  return "`" + name.replace(/`/g, "``") + "`";
}
