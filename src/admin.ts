import type { Grant, GrantTarget, PrivilegeEdit } from "./api";

export type Level = "global" | "database" | "table";

export const ALL = "ALL PRIVILEGES";

export interface PrivilegeDef {
  name: string;
  label: string;
  group: "資料" | "結構" | "管理";
  levels: Level[];
  mariadbOnly?: boolean;
}

const G: Level[] = ["global"];
const GD: Level[] = ["global", "database"];
const GDT: Level[] = ["global", "database", "table"];

/** The static privileges of MySQL and MariaDB, in display order. */
export const PRIVILEGES: PrivilegeDef[] = [
  { name: "SELECT", label: "讀取資料", group: "資料", levels: GDT },
  { name: "INSERT", label: "新增資料", group: "資料", levels: GDT },
  { name: "UPDATE", label: "修改資料", group: "資料", levels: GDT },
  { name: "DELETE", label: "刪除資料", group: "資料", levels: GDT },
  {
    name: "DELETE HISTORY",
    label: "刪除系統版本表的歷史資料",
    group: "資料",
    levels: GDT,
    mariadbOnly: true,
  },
  { name: "SHOW VIEW", label: "查看 view 定義", group: "資料", levels: GDT },
  { name: "EXECUTE", label: "執行預存程序與函式", group: "資料", levels: GD },
  { name: "LOCK TABLES", label: "鎖定資料表", group: "資料", levels: GD },
  {
    name: "CREATE TEMPORARY TABLES",
    label: "建立暫存表",
    group: "資料",
    levels: GD,
  },
  { name: "CREATE", label: "建立資料庫與資料表", group: "結構", levels: GDT },
  { name: "ALTER", label: "修改資料表結構", group: "結構", levels: GDT },
  {
    name: "DROP",
    label: "刪除資料庫、資料表、view",
    group: "結構",
    levels: GDT,
  },
  { name: "INDEX", label: "建立與刪除索引", group: "結構", levels: GDT },
  { name: "REFERENCES", label: "建立外鍵", group: "結構", levels: GDT },
  { name: "CREATE VIEW", label: "建立 view", group: "結構", levels: GDT },
  { name: "TRIGGER", label: "建立與刪除 trigger", group: "結構", levels: GDT },
  {
    name: "CREATE ROUTINE",
    label: "建立預存程序與函式",
    group: "結構",
    levels: GD,
  },
  {
    name: "ALTER ROUTINE",
    label: "修改與刪除預存程序",
    group: "結構",
    levels: GD,
  },
  { name: "EVENT", label: "排程事件", group: "結構", levels: GD },
  { name: "PROCESS", label: "看所有人執行中的查詢", group: "管理", levels: G },
  { name: "SHOW DATABASES", label: "列出所有資料庫", group: "管理", levels: G },
  { name: "RELOAD", label: "FLUSH 等重新載入", group: "管理", levels: G },
  { name: "SHUTDOWN", label: "關閉伺服器", group: "管理", levels: G },
  { name: "FILE", label: "讀寫伺服器上的檔案", group: "管理", levels: G },
  {
    name: "SUPER",
    label: "超級管理（KILL 他人、SET GLOBAL…）",
    group: "管理",
    levels: G,
  },
  { name: "CREATE USER", label: "建立與管理使用者", group: "管理", levels: G },
  {
    name: "REPLICATION CLIENT",
    label: "查看複寫狀態",
    group: "管理",
    levels: G,
  },
  {
    name: "REPLICATION SLAVE",
    label: "作為複寫的從庫",
    group: "管理",
    levels: G,
  },
  {
    name: "CREATE TABLESPACE",
    label: "管理 tablespace",
    group: "管理",
    levels: G,
  },
];

export function privilegesFor(level: Level, mariadb: boolean) {
  return PRIVILEGES.filter((p) => p.levels.includes(level) && (mariadb || !p.mariadbOnly));
}

export interface Preset {
  name: string;
  hint: string;
  privileges: string[];
}

const READ = ["SELECT", "SHOW VIEW"];
const WRITE = [...READ, "INSERT", "UPDATE", "DELETE", "EXECUTE", "LOCK TABLES", "CREATE TEMPORARY TABLES"];
const DEV = [
  ...WRITE,
  "CREATE",
  "ALTER",
  "DROP",
  "INDEX",
  "REFERENCES",
  "CREATE VIEW",
  "TRIGGER",
  "CREATE ROUTINE",
  "ALTER ROUTINE",
  "EVENT",
];

export const PRESETS: Preset[] = [
  { name: "唯讀", hint: "只能查詢資料", privileges: READ },
  { name: "讀寫", hint: "查詢與增修刪資料，不能改結構", privileges: WRITE },
  {
    name: "開發",
    hint: "讀寫加上建表、改結構、索引、view、預存程序",
    privileges: DEV,
  },
  { name: "全部", hint: "ALL PRIVILEGES", privileges: [ALL] },
];

/** A preset's privileges that exist at `level`. */
export function presetFor(preset: Preset, level: Level, mariadb: boolean) {
  if (preset.privileges.includes(ALL)) return [ALL];
  const known = new Set(privilegesFor(level, mariadb).map((p) => p.name));
  return preset.privileges.filter((p) => known.has(p));
}

export function levelOf(t: GrantTarget): Level {
  return t.database == null ? "global" : t.table == null ? "database" : "table";
}

export function targetKey(t: GrantTarget) {
  return JSON.stringify([t.database, t.table]);
}

const q = (n: string) => "`" + n.replace(/`/g, "``") + "`";

export function targetLabel(t: GrantTarget) {
  if (t.database == null) return "*.*（整台伺服器）";
  return t.table == null ? `${q(t.database)}.*` : `${q(t.database)}.${q(t.table)}`;
}

export function accountLabel(user: string, host: string) {
  return `'${user}'@'${host}'`;
}

/** Privileges of one target, as loaded and as edited. */
export interface TargetState {
  target: GrantTarget;
  before: string[];
  beforeGo: boolean;
  after: string[];
  afterGo: boolean;
}

/** Editable grants by target; the global one is always there. */
export function targetsFrom(grants: Grant[]): TargetState[] {
  const map = new Map<string, TargetState>();
  const add = (target: GrantTarget, privileges: string[], go: boolean) => {
    const k = targetKey(target);
    const cur = map.get(k) ?? {
      target,
      before: [],
      beforeGo: false,
      after: [],
      afterGo: false,
    };
    const merged = [...new Set([...cur.before, ...privileges.filter((p) => p !== "USAGE")])];
    map.set(k, {
      ...cur,
      before: merged,
      after: merged,
      beforeGo: cur.beforeGo || go,
      afterGo: cur.afterGo || go,
    });
  };
  add({ database: null, table: null }, [], false);
  for (const g of grants) if (g.editable && g.target) add(g.target, g.privileges, g.grant_option);
  return [...map.values()].sort((a, b) => {
    const order = { global: 0, database: 1, table: 2 };
    const la = levelOf(a.target);
    const lb = levelOf(b.target);
    if (la !== lb) return order[la] - order[lb];
    return targetKey(a.target).localeCompare(targetKey(b.target));
  });
}

function sameSet(a: string[], b: string[]) {
  return a.length === b.length && a.every((x) => b.includes(x));
}

export function isChanged(t: TargetState) {
  return !sameSet(t.before, t.after) || t.beforeGo !== t.afterGo;
}

export function editsOf(targets: TargetState[]): PrivilegeEdit[] {
  return targets.filter(isChanged).map((t) => ({
    target: t.target,
    before: t.before,
    before_grant_option: t.beforeGo,
    after: t.after,
    after_grant_option: t.afterGo,
  }));
}

/** A random password: letters and digits only, so it survives any client. */
export function randomPassword(length = 20) {
  const chars = "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
  const bytes = crypto.getRandomValues(new Uint32Array(length));
  return Array.from(bytes, (b) => chars[b % chars.length]).join("");
}
