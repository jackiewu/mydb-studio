import { useCallback, useEffect, useMemo, useState } from "react";
import { api, describeError, type AdminChange, type Grant, type UserAccount } from "../../api";
import {
  accountLabel,
  ALL,
  editsOf,
  isChanged,
  levelOf,
  PRESETS,
  presetFor,
  privilegesFor,
  randomPassword,
  targetKey,
  targetLabel,
  targetsFrom,
  type Level,
  type TargetState,
} from "../../admin";
import { copyText } from "../../copy";
import { PlanDialog } from "./PlanDialog";

interface Props {
  sessionId: string;
  mariadb: boolean;
}

interface Pending {
  title: string;
  change: AdminChange;
  warning?: string;
  danger?: boolean;
  /** Account to select once it ran; null clears the selection. */
  select?: UserAccount | null;
}

const LEVEL_NAME: Record<Level, string> = {
  global: "全域",
  database: "資料庫",
  table: "資料表",
};

export function UsersPanel({ sessionId, mariadb }: Props) {
  const [users, setUsers] = useState<UserAccount[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [selected, setSelected] = useState<UserAccount | null>(null);
  const [me, setMe] = useState("");
  const [pending, setPending] = useState<Pending | null>(null);
  const [creating, setCreating] = useState(false);
  const [passwordFor, setPasswordFor] = useState<UserAccount | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  // Bumped to reload the selected account's grants.
  const [grantsVersion, setGrantsVersion] = useState(0);

  const load = useCallback(() => {
    api.serverUsers(sessionId).then(
      (u) => {
        setUsers(u);
        setError(null);
        setSelected((cur) => (cur ? (u.find((x) => x.user === cur.user && x.host === cur.host) ?? null) : cur));
      },
      (e) => setError(describeError(e)),
    );
  }, [sessionId]);

  useEffect(() => {
    load();
    api.serverOverview(sessionId).then(
      (o) => setMe(o.current_user),
      () => {},
    );
  }, [load, sessionId]);

  const isMe = (u: UserAccount) => me === `${u.user}@${u.host}`;
  const f = filter.trim().toLowerCase();
  const shown = (users ?? []).filter((u) => !f || `${u.user}@${u.host}`.toLowerCase().includes(f));

  return (
    <div className="users-panel">
      <div className="users-list">
        <div className="users-list-head">
          <input placeholder="搜尋使用者" value={filter} onChange={(e) => setFilter(e.target.value)} />
          <button className="primary" onClick={() => setCreating(true)} title="新增使用者">
            ＋
          </button>
        </div>
        {error && (
          <div className="error-box">
            {error}
            <p className="hint">列出使用者需要讀取 mysql 資料庫的權限。</p>
          </div>
        )}
        <ul>
          {shown.map((u) => (
            <li
              key={`${u.user}@${u.host}`}
              className={selected?.user === u.user && selected.host === u.host ? "current" : undefined}
              onClick={() => (setSelected(u), setMessage(null))}
            >
              <span className="user-name">{u.user || <em className="muted">（匿名）</em>}</span>
              <span className="muted">@{u.host}</span>
              {u.locked && <span title="已鎖定，無法登入">🔒</span>}
              {isMe(u) && <span className="badge">你</span>}
            </li>
          ))}
        </ul>
        {users && shown.length === 0 && <p className="empty-state">沒有使用者</p>}
      </div>

      <div className="user-detail">
        {message && (
          <div className="notice ok" onClick={() => setMessage(null)}>
            {message}
          </div>
        )}
        {selected ? (
          <UserDetail
            key={`${selected.user}@${selected.host}:${grantsVersion}`}
            sessionId={sessionId}
            mariadb={mariadb}
            account={selected}
            isMe={isMe(selected)}
            onChangePassword={() => setPasswordFor(selected)}
            onPlan={setPending}
          />
        ) : (
          <p className="empty-state">選一個使用者查看與修改權限，或按 ＋ 新增使用者。</p>
        )}
      </div>

      {creating && (
        <CreateUserDialog
          sessionId={sessionId}
          mariadb={mariadb}
          onCancel={() => setCreating(false)}
          onNext={(p) => {
            setCreating(false);
            setPending(p);
          }}
        />
      )}
      {passwordFor && (
        <PasswordDialog
          account={passwordFor}
          onCancel={() => setPasswordFor(null)}
          onNext={(password) => {
            setPasswordFor(null);
            setPending({
              title: "變更密碼",
              change: {
                kind: "set_password",
                user: passwordFor.user,
                host: passwordFor.host,
                password,
              },
              warning: isMe(passwordFor)
                ? "這是你目前登入的帳號。改完後，連線設定裡記住的密碼也要一起更新，否則下次會連不上。"
                : undefined,
            });
          }}
        />
      )}
      {pending && (
        <PlanDialog
          sessionId={sessionId}
          title={pending.title}
          change={pending.change}
          warning={pending.warning}
          danger={pending.danger}
          onCancel={() => setPending(null)}
          onDone={() => {
            const done = pending;
            setPending(null);
            setMessage(`已完成：${done.title}`);
            if (done.select !== undefined) setSelected(done.select);
            setGrantsVersion((v) => v + 1);
            load();
          }}
        />
      )}
    </div>
  );
}

function UserDetail({
  sessionId,
  mariadb,
  account,
  isMe,
  onChangePassword,
  onPlan,
}: {
  sessionId: string;
  mariadb: boolean;
  account: UserAccount;
  isMe: boolean;
  onChangePassword: () => void;
  onPlan: (p: Pending) => void;
}) {
  const [grants, setGrants] = useState<Grant[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [targets, setTargets] = useState<TargetState[]>([]);
  const [current, setCurrent] = useState<string>(targetKey({ database: null, table: null }));
  const [adding, setAdding] = useState(false);
  const { user, host } = account;
  const label = accountLabel(user, host);

  useEffect(() => {
    api.userGrants(sessionId, user, host).then(
      (g) => {
        setGrants(g);
        const t = targetsFrom(g);
        setTargets(t);
        // Open the most specific grant, which is usually the one that matters.
        setCurrent(targetKey((t.length > 1 ? t[1] : t[0]).target));
      },
      (e) => setError(describeError(e)),
    );
  }, [sessionId, user, host]);

  const edits = useMemo(() => editsOf(targets), [targets]);
  const active = targets.find((t) => targetKey(t.target) === current) ?? targets[0];
  const readOnly = (grants ?? []).filter((g) => !g.editable);

  const update = (key: string, patch: Partial<TargetState>) =>
    setTargets((ts) => ts.map((t) => (targetKey(t.target) === key ? { ...t, ...patch } : t)));

  const losesOwnAccess = isMe && edits.some((e) => e.before.length > e.after.length || e.before_grant_option);

  return (
    <div className="user-detail-body">
      <div className="user-head">
        <h2>{label}</h2>
        {account.locked && <span className="badge bad-badge">已鎖定</span>}
        {account.plugin && (
          <span className="badge" title="驗證方式">
            {account.plugin}
          </span>
        )}
        <span className="spacer" />
        <button onClick={onChangePassword}>改密碼</button>
        <button
          onClick={() =>
            onPlan({
              title: account.locked ? "解除鎖定" : "鎖定帳號",
              change: {
                kind: "set_locked",
                user,
                host,
                locked: !account.locked,
              },
              warning: account.locked
                ? `${label} 解鎖後就能再登入。`
                : `鎖定後 ${label} 無法登入，已連上的連線不受影響。${isMe ? "\n⚠️ 這是你目前登入的帳號，鎖定後下次會連不上。" : ""}`,
              danger: !account.locked && isMe,
            })
          }
        >
          {account.locked ? "解除鎖定" : "鎖定"}
        </button>
        <button
          className="danger"
          onClick={() =>
            onPlan({
              title: "刪除使用者",
              change: { kind: "drop_user", user, host },
              warning: `刪除 ${label} 以及它的所有權限，無法復原。${isMe ? "\n⚠️ 這是你目前登入的帳號！" : ""}`,
              danger: true,
              select: null,
            })
          }
        >
          刪除
        </button>
      </div>
      {error && <div className="error-box">{error}</div>}

      {grants && (
        <div className="priv-editor">
          <div className="priv-targets">
            <div className="priv-targets-title">權限範圍</div>
            <ul>
              {targets.map((t) => {
                const k = targetKey(t.target);
                const n = t.after.includes(ALL) ? "全部" : `${t.after.length} 項`;
                return (
                  <li key={k} className={k === current ? "current" : undefined} onClick={() => setCurrent(k)}>
                    <span className="priv-level">{LEVEL_NAME[levelOf(t.target)]}</span>
                    <span className="priv-target-name">{targetLabel(t.target)}</span>
                    <span className="muted small-text">{t.after.length || t.afterGo ? n : "無"}</span>
                    {isChanged(t) && (
                      <span className="tab-dirty" title="有未套用的變更">
                        •
                      </span>
                    )}
                  </li>
                );
              })}
            </ul>
            {adding ? (
              <AddTarget
                sessionId={sessionId}
                existing={targets.map((t) => targetKey(t.target))}
                onCancel={() => setAdding(false)}
                onAdd={(target) => {
                  setAdding(false);
                  setTargets((ts) => [
                    ...ts,
                    {
                      target,
                      before: [],
                      beforeGo: false,
                      after: [],
                      afterGo: false,
                    },
                  ]);
                  setCurrent(targetKey(target));
                }}
              />
            ) : (
              <button className="link" onClick={() => setAdding(true)}>
                ＋ 加入資料庫或資料表
              </button>
            )}
          </div>
          {active && (
            <PrivilegeChecks
              key={targetKey(active.target)}
              state={active}
              mariadb={mariadb}
              onChange={(patch) => update(targetKey(active.target), patch)}
            />
          )}
        </div>
      )}

      {grants && (
        <div className="priv-footer">
          <span className="muted small-text">
            {edits.length ? `${edits.length} 個範圍有變更` : "勾選後按「預覽並套用」，會先顯示要執行的 GRANT／REVOKE。"}
          </span>
          <span className="spacer" />
          <button disabled={!edits.length} onClick={() => setTargets(targetsFrom(grants))}>
            還原
          </button>
          <button
            className="primary"
            disabled={!edits.length}
            onClick={() =>
              onPlan({
                title: "套用權限變更",
                change: { kind: "privileges", user, host, edits },
                warning: losesOwnAccess
                  ? "⚠️ 這是你目前登入的帳號，拿掉權限後這個 app 可能就無法再管理它。"
                  : undefined,
                danger: losesOwnAccess,
              })
            }
          >
            預覽並套用
          </button>
        </div>
      )}

      {grants && (
        <details className="grant-lines" open={readOnly.length > 0}>
          <summary>
            SHOW GRANTS（{grants.length} 行{readOnly.length ? `，${readOnly.length} 行無法在上方編輯` : ""}）
          </summary>
          <ul>
            {grants.map((g, i) => (
              <li key={i} className={g.editable ? undefined : "readonly"}>
                <code>{g.text}</code>
                {!g.editable && (
                  <span className="muted small-text">
                    {g.target ? "　欄位層級權限" : "　預存程序／角色／PROXY 權限"}
                    ，請用 SQL 修改
                  </span>
                )}
              </li>
            ))}
          </ul>
          <button className="small" onClick={() => copyText(grants.map((g) => g.text + ";").join("\n"))}>
            複製全部
          </button>
        </details>
      )}
    </div>
  );
}

function PrivilegeChecks({
  state,
  mariadb,
  onChange,
}: {
  state: TargetState;
  mariadb: boolean;
  onChange: (patch: Partial<TargetState>) => void;
}) {
  const level = levelOf(state.target);
  const defs = privilegesFor(level, mariadb);
  const all = state.after.includes(ALL);
  const known = new Set(defs.map((d) => d.name));
  // Privileges the server reported that this list does not know (newer versions add some).
  const others = [...new Set([...state.before, ...state.after])].filter((p) => p !== ALL && !known.has(p));

  const toggle = (name: string, on: boolean) => {
    if (name === ALL) {
      // Unticking ALL leaves every single privilege ticked, to untick one by one.
      onChange({ after: on ? [ALL] : defs.map((d) => d.name) });
      return;
    }
    const next = new Set(state.after);
    if (on) next.add(name);
    else next.delete(name);
    onChange({ after: [...next] });
  };

  const groups = ["資料", "結構", "管理"] as const;
  return (
    <div className="priv-checks">
      <div className="priv-presets">
        <span className="muted small-text">快速套用：</span>
        {PRESETS.map((p) => (
          <button
            key={p.name}
            className="small"
            title={p.hint}
            onClick={() => onChange({ after: presetFor(p, level, mariadb) })}
          >
            {p.name}
          </button>
        ))}
        <button
          className="small"
          onClick={() => onChange({ after: [], afterGo: false })}
          title="拿掉這個範圍的所有權限"
        >
          清除
        </button>
      </div>
      <label className="priv-all">
        <input type="checkbox" checked={all} onChange={(e) => toggle(ALL, e.target.checked)} />
        <strong>ALL PRIVILEGES</strong>
        <span className="muted small-text">
          {level === "global" ? "整台伺服器的所有權限（等同管理員）" : "這個範圍的所有權限"}
        </span>
      </label>
      <div className="priv-groups">
        {groups.map((g) => {
          const items = defs.filter((d) => d.group === g);
          if (!items.length) return null;
          return (
            <fieldset key={g} disabled={all}>
              <legend>{g}</legend>
              {items.map((d) => (
                <label key={d.name} title={d.label} className={changedClass(state, d.name)}>
                  <input
                    type="checkbox"
                    checked={all || state.after.includes(d.name)}
                    onChange={(e) => toggle(d.name, e.target.checked)}
                  />
                  <span className="priv-name">{d.name}</span>
                  <span className="muted small-text">{d.label}</span>
                </label>
              ))}
            </fieldset>
          );
        })}
        {others.length > 0 && (
          <fieldset disabled={all}>
            <legend>其他</legend>
            {others.map((name) => (
              <label key={name} className={changedClass(state, name)}>
                <input
                  type="checkbox"
                  checked={all || state.after.includes(name)}
                  onChange={(e) => toggle(name, e.target.checked)}
                />
                <span className="priv-name">{name}</span>
              </label>
            ))}
          </fieldset>
        )}
      </div>
      <label className={`priv-all ${state.beforeGo !== state.afterGo ? "changed" : ""}`}>
        <input type="checkbox" checked={state.afterGo} onChange={(e) => onChange({ afterGo: e.target.checked })} />
        <strong>WITH GRANT OPTION</strong>
        <span className="muted small-text">可以把自己有的權限再授予別人</span>
      </label>
    </div>
  );
}

function changedClass(state: TargetState, name: string) {
  const had = state.before.includes(name) || state.before.includes(ALL);
  const has = state.after.includes(name) || state.after.includes(ALL);
  return had === has ? undefined : has ? "changed added" : "changed removed";
}

function AddTarget({
  sessionId,
  existing,
  onAdd,
  onCancel,
}: {
  sessionId: string;
  existing: string[];
  onAdd: (t: { database: string; table: string | null }) => void;
  onCancel: () => void;
}) {
  const [dbs, setDbs] = useState<string[]>([]);
  const [tables, setTables] = useState<string[]>([]);
  const [db, setDb] = useState("");
  const [table, setTable] = useState("");

  useEffect(() => {
    api.listDatabases(sessionId).then(
      (d) => setDbs(d.map((x) => x.name)),
      () => {},
    );
  }, [sessionId]);
  useEffect(() => {
    setTable("");
    setTables([]);
    if (db && dbs.includes(db))
      api.listTables(sessionId, db).then(
        (t) => setTables(t.map((x) => x.name)),
        () => {},
      );
  }, [sessionId, db, dbs]);

  const target = { database: db.trim(), table: table.trim() || null };
  const dup = existing.includes(targetKey(target));
  return (
    <div className="add-target">
      <label>
        資料庫
        <input
          list="admin-dbs"
          value={db}
          onChange={(e) => setDb(e.target.value)}
          placeholder="名稱，可用 % 萬用字元"
        />
        <datalist id="admin-dbs">
          {dbs.map((d) => (
            <option key={d} value={d} />
          ))}
        </datalist>
      </label>
      <label>
        資料表
        <select value={table} onChange={(e) => setTable(e.target.value)} disabled={!tables.length}>
          <option value="">（整個資料庫）</option>
          {tables.map((t) => (
            <option key={t} value={t}>
              {t}
            </option>
          ))}
        </select>
      </label>
      {dup && <p className="hint">這個範圍已經在清單裡。</p>}
      <div className="actions">
        <button className="small" onClick={onCancel}>
          取消
        </button>
        <button className="small primary" disabled={!target.database || dup} onClick={() => onAdd(target)}>
          加入
        </button>
      </div>
    </div>
  );
}

function PasswordFields({
  password,
  setPassword,
  required,
  onValid,
}: {
  password: string;
  setPassword: (p: string) => void;
  required: boolean;
  /** Whether the confirmation matches (always, while the password is shown). */
  onValid: (ok: boolean) => void;
}) {
  const [confirm, setConfirm] = useState("");
  const [visible, setVisible] = useState(false);
  const mismatch = !visible && confirm !== password;
  useEffect(() => onValid(!mismatch), [mismatch, onValid]);
  return (
    <>
      <label>
        <span>密碼{!required && <span className="muted">（可留空）</span>}</span>
        <div className="input-with-button">
          <input
            type={visible ? "text" : "password"}
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            autoComplete="new-password"
          />
          <button
            type="button"
            onClick={() => {
              const p = randomPassword();
              setPassword(p);
              setConfirm(p);
              setVisible(true);
            }}
            title="產生 20 字元的隨機密碼"
          >
            產生
          </button>
          <button type="button" onClick={() => setVisible(!visible)}>
            {visible ? "隱藏" : "顯示"}
          </button>
          {visible && password && (
            <button type="button" onClick={() => copyText(password)}>
              複製
            </button>
          )}
        </div>
      </label>
      {!visible && (
        <label>
          再輸入一次
          <input
            type="password"
            value={confirm}
            onChange={(e) => setConfirm(e.target.value)}
            autoComplete="new-password"
          />
        </label>
      )}
      {visible && password && <p className="hint">這個密碼之後看不到，請先複製保存。</p>}
      {mismatch && confirm && <p className="hint bad-text">兩次輸入的密碼不一樣</p>}
    </>
  );
}

function PasswordDialog({
  account,
  onNext,
  onCancel,
}: {
  account: UserAccount;
  onNext: (password: string) => void;
  onCancel: () => void;
}) {
  const [password, setPassword] = useState("");
  const [valid, setValid] = useState(true);
  return (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onCancel()}>
      <form
        className="conn-form admin-form"
        onSubmit={(e) => {
          e.preventDefault();
          if (password && valid) onNext(password);
        }}
      >
        <h2>變更 {accountLabel(account.user, account.host)} 的密碼</h2>
        <PasswordFields password={password} setPassword={setPassword} required onValid={setValid} />
        <div className="actions">
          <span className="spacer" />
          <button type="button" onClick={onCancel}>
            取消
          </button>
          <button className="primary" type="submit" disabled={!password || !valid}>
            下一步：預覽
          </button>
        </div>
      </form>
    </div>
  );
}

const HOSTS: { value: string; label: string }[] = [
  { value: "%", label: "任何地方" },
  { value: "localhost", label: "只限伺服器本機" },
  { value: "127.0.0.1", label: "本機（TCP）" },
];

function CreateUserDialog({
  sessionId,
  mariadb,
  onNext,
  onCancel,
}: {
  sessionId: string;
  mariadb: boolean;
  onNext: (p: Pending) => void;
  onCancel: () => void;
}) {
  const [user, setUser] = useState("");
  const [host, setHost] = useState("%");
  const [password, setPassword] = useState("");
  const [valid, setValid] = useState(true);
  const [preset, setPreset] = useState(0);
  const [scope, setScope] = useState("");
  const [dbs, setDbs] = useState<string[]>([]);

  useEffect(() => {
    api.listDatabases(sessionId).then(
      (d) => setDbs(d.map((x) => x.name)),
      () => {},
    );
  }, [sessionId]);

  const submit = () => {
    const chosen = PRESETS[preset - 1];
    const target = { database: scope || null, table: null };
    const privileges = chosen
      ? [
          {
            target,
            before: [],
            before_grant_option: false,
            after: presetFor(chosen, levelOf(target), mariadb),
            after_grant_option: false,
          },
        ]
      : [];
    onNext({
      title: "新增使用者",
      change: {
        kind: "create_user",
        user: user.trim(),
        host: host.trim() || "%",
        password,
        privileges,
      },
      warning:
        chosen?.privileges.includes(ALL) && !scope
          ? "⚠️ 會授予整台伺服器的所有權限，等同管理員。"
          : password
            ? undefined
            : "沒有設定密碼：任何人只要知道帳號就能登入。",
      danger: !password || (!!chosen?.privileges.includes(ALL) && !scope),
      select: {
        user: user.trim(),
        host: host.trim() || "%",
        plugin: "",
        locked: false,
      },
    });
  };

  return (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onCancel()}>
      <form
        className="conn-form admin-form"
        onSubmit={(e) => {
          e.preventDefault();
          if (user.trim() && valid) submit();
        }}
      >
        <h2>新增使用者</h2>
        <label>
          使用者名稱
          <input autoFocus value={user} onChange={(e) => setUser(e.target.value)} />
        </label>
        <label>
          可從哪裡登入（host）
          <input value={host} onChange={(e) => setHost(e.target.value)} />
        </label>
        <div className="host-chips">
          {HOSTS.map((h) => (
            <button
              type="button"
              key={h.value}
              className={host === h.value ? "small active" : "small"}
              onClick={() => setHost(h.value)}
            >
              {h.value}　<span className="muted">{h.label}</span>
            </button>
          ))}
        </div>
        <p className="hint">也可以填 IP 或網段，例如 10.0.0.% 只允許內網。</p>
        <PasswordFields password={password} setPassword={setPassword} required={false} onValid={setValid} />
        <div className="row">
          <label>
            初始權限
            <select value={preset} onChange={(e) => setPreset(Number(e.target.value))}>
              <option value={0}>先不給（之後再設定）</option>
              {PRESETS.map((p, i) => (
                <option key={p.name} value={i + 1}>
                  {p.name}：{p.hint}
                </option>
              ))}
            </select>
          </label>
          <label>
            範圍
            <select value={scope} onChange={(e) => setScope(e.target.value)} disabled={!preset}>
              <option value="">所有資料庫（*.*）</option>
              {dbs.map((d) => (
                <option key={d} value={d}>
                  {d}
                </option>
              ))}
            </select>
          </label>
        </div>
        <div className="actions">
          <span className="spacer" />
          <button type="button" onClick={onCancel}>
            取消
          </button>
          <button className="primary" type="submit" disabled={!user.trim() || !valid}>
            下一步：預覽
          </button>
        </div>
      </form>
    </div>
  );
}
