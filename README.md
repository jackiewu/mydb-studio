# MyDB Studio

MySQL / MariaDB 與 Redis 桌面用戶端，用 Rust（Tauri 2）開發。目標功能：

- 連線管理，支援 SSL/TLS 與 SSH 通道；首頁連線卡片，最愛與最近使用排前面 ✅
- 閒置斷線後自動重連（含 SSH 通道），保留 `USE` 選的資料庫 ✅
- 側邊欄資料庫／資料表清單，點資料表預覽資料；雙擊資料庫切換目前資料庫（`USE`） ✅
- SQL 編輯器：多句依序執行、只執行選取或游標所在那句、Explain 並標出全表掃描等問題；自動完成資料庫、資料表、欄位名稱 ✅
- 多分頁：每個分頁有自己的 SQL、結果與紀錄開關；📌 釘選的分頁會保留，下次開同一個連線時還原，預設開啟紀錄 ✅
- 結果表格：選取儲存格／列／欄，複製或存成 TSV、CSV、JSON、INSERT SQL、Markdown ✅
- 執行中的查詢可以按「■ 停止」（⌘／Ctrl + .）中止；超過畫面筆數的結果可以「匯出全部」，直接串流寫進檔案 ✅
- Snippet：常用 SQL 範本，打縮寫展開，`${欄位}` 用 Tab 依序填 ✅
- 設定：編輯器主題與字型、預設回傳筆數、是否預設記錄、是否顯示系統資料庫 ✅
- 執行紀錄：打開「📝 紀錄」後，每次執行的 SQL 與統計（執行時間、影響／回傳筆數、錯誤、結果預覽）都會存下來，可搜尋、篩選、加備註、帶回編輯器 ✅
- 報表：勾選紀錄，產生 Markdown、HTML 或 CSV 報表 ✅
- 錯誤修正建議：內建 MySQL/MariaDB 錯誤碼字典，必要時再交給 AI 分析（規劃中）
- 🖥 伺服器：版本、運行時間、連線數、快取命中率等重要指標；狀態與系統變數搜尋；執行中的查詢（可中止）；使用者新增、改密碼、鎖定、刪除；全域／資料庫／資料表權限勾選與預設組合，套用前先預覽 GRANT／REVOKE ✅
- MCP server：讓 AI 工具（例如 Claude）用存好的 MySQL／MariaDB 連線查詢 ✅
- Redis（第一版）：連線支援 TLS（含 AWS ElastiCache、GCP Memorystore）與 SSH 通道；左側 db0～db15 的 key 依冒號前綴分組，只用 `SCAN` 分批載入；依型別唯讀檢視值（大型集合分頁）；指令列一行一個指令；`KEYS`、`FLUSHALL` 等危險指令先確認；指令寫進執行紀錄，`AUTH` 的密碼一律遮蔽 ✅

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

`crates/mydb-core/tests/live.rs` 會連真的資料庫，沒設定 `MYDB_TEST_HOST` 時自動略過；`crates/mydb-core/tests/redis_live.rs` 連 Redis，沒設定 `MYDB_TEST_REDIS`（`host:port`）時自動略過。用 Docker 起測試用的 MariaDB 和 Redis：

```bash
eval "$(./scripts/test-env.sh)"
cargo test --workspace
```

前端的單元測試（key 前綴分組）用 `npm test` 執行。

另外設定 `MYDB_TEST_CA` 會跑 TLS 測試，設定 `MYDB_TEST_SSH_HOST`、`MYDB_TEST_SSH_PORT`、`MYDB_TEST_SSH_USER`、`MYDB_TEST_SSH_KEY`（或 `MYDB_TEST_SSH_PASSWORD`）會跑 SSH 通道測試。

Redis 的 TLS 測試要設定 `MYDB_TEST_REDIS_TLS`（TLS 埠的 `host:port`，憑證上要有這個主機名稱）和 `MYDB_TEST_REDIS_CA`；SSH 通道測試沿用上面的 `MYDB_TEST_SSH_*`。

## 連線設定說明

- **SSL 模式**對應 mysql CLI 的 `--ssl-mode`：不加密、加密不驗證、驗證 CA、驗證 CA 與主機名稱。
- **SSH 通道**的「主機／埠號」填的是從跳板機看出去的資料庫位址。第一次連線會記住跳板機的金鑰指紋，之後指紋不同會拒絕連線。
- 密碼目前存在本機的 app 資料庫（SQLite）中，之後會改存到作業系統的鑰匙圈。
- **Redis 的 TLS** 一樣分四種模式。透過 SSH 通道時，app 連到本機的通道埠，但憑證仍用「主機」欄位的名稱驗證，所以 ElastiCache 請填 endpoint 而不是 IP。
- **Redis 指令列**每行一個指令，`#` 開頭是註解；⌘Enter 執行游標所在那一行，有選取時依序執行選取的多行，遇到錯誤就停。SQL 專用的功能（分句、Explain、自動加 LIMIT、執行前檢查）不會套用。
