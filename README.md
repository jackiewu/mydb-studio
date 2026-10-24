# MyDB Studio

MySQL / MariaDB 桌面用戶端，用 Rust（Tauri 2）開發。目標功能：

- 連線管理，支援 SSL/TLS 與 SSH 通道；首頁連線卡片，最愛與最近使用排前面 ✅
- 閒置斷線後自動重連（含 SSH 通道），保留 `USE` 選的資料庫 ✅
- 側邊欄資料庫／資料表清單，點資料表預覽資料 ✅
- SQL 編輯器與結果表格 ✅
- 執行紀錄：打開「📝 紀錄」後，每次執行的 SQL 與統計（執行時間、影響／回傳筆數、錯誤、結果預覽）都會存下來，可搜尋、篩選、加備註、帶回編輯器 ✅
- 報表：勾選紀錄，產生 Markdown、HTML 或 CSV 報表 ✅
- 錯誤修正建議：內建 MySQL/MariaDB 錯誤碼字典，必要時再交給 AI 分析（規劃中）
- 伺服器資訊、使用者與權限管理（規劃中）
- Snippet、MCP server（之後）

## 專案結構

```
crates/mydb-core/   連線、SSH 通道、執行查詢、SQL 分句、本機 SQLite（不含 UI，之後的 MCP server 也會共用）
src-tauri/          Tauri 後端，把 core 包成前端可呼叫的指令
src/                React + TypeScript 前端（CodeMirror 編輯器）
```

## 開發

需要 Rust（stable）、Node 22，以及 [Tauri 的系統套件](https://v2.tauri.app/start/prerequisites/)。

```bash
npm install
npm run tauri dev      # 開發模式
npm run tauri build    # 打包安裝檔
```

## 測試

```bash
cargo test --workspace
```

`crates/mydb-core/tests/live.rs` 會連真的資料庫，沒設定 `MYDB_TEST_HOST` 時自動略過。用 Docker 起一個測試用 MariaDB：

```bash
eval "$(./scripts/test-env.sh)"
cargo test --workspace
```

另外設定 `MYDB_TEST_CA` 會跑 TLS 測試，設定 `MYDB_TEST_SSH_HOST`、`MYDB_TEST_SSH_PORT`、`MYDB_TEST_SSH_USER`、`MYDB_TEST_SSH_KEY`（或 `MYDB_TEST_SSH_PASSWORD`）會跑 SSH 通道測試。

## 連線設定說明

- **SSL 模式**對應 mysql CLI 的 `--ssl-mode`：不加密、加密不驗證、驗證 CA、驗證 CA 與主機名稱。
- **SSH 通道**的「主機／埠號」填的是從跳板機看出去的資料庫位址。第一次連線會記住跳板機的金鑰指紋，之後指紋不同會拒絕連線。
- 密碼目前存在本機的 app 資料庫（SQLite）中，之後會改存到作業系統的鑰匙圈。
