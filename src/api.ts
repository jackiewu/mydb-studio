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

export interface ConnectionProfile {
  id: string;
  name: string;
  host: string;
  port: number;
  user: string;
  password?: string | null;
  database?: string | null;
  ssl: SslConfig;
  ssh?: SshConfig | null;
  favorite?: boolean;
  last_used_at?: string | null;
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
  info: SessionInfo;
  current_database?: string | null;
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
  | { kind: "driver" | "connection" | "ssh" | "io" | "store" | "config"; message: string }
  | { kind: "not_connected"; id: string };

export interface ExecutionOutcome {
  started_at: string;
  duration_ms: number;
  results: StatementResult[];
  error?: CoreError | null;
  failed_statement?: string | null;
  reconnected: boolean;
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

export const DEFAULT_SETTINGS: AppSettings = {
  editor_theme: "auto",
  editor_font_size: 13,
  max_rows: 1000,
  record_by_default: false,
  show_system_databases: false,
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

export function newProfile(): ConnectionProfile {
  return {
    id: "",
    name: "",
    host: "127.0.0.1",
    port: 3306,
    user: "root",
    password: "",
    database: "",
    ssl: { mode: "disabled" },
    ssh: null,
  };
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
  readTextFile: (path: string) => invoke<string>("read_text_file", { path }),
  takeCrashInfo: () => invoke<CrashInfo | null>("take_crash_info"),
  bugReportBody: (what: string, details: string) => invoke<string>("bug_report_body", { what, details }),
  openBugReport: (title: string, body: string) => invoke<void>("open_bug_report", { title, body }),
};

/** Quotes an identifier with backticks. */
export function quoteIdent(name: string): string {
  return "`" + name.replace(/`/g, "``") + "`";
}
