# 场景识别功能实施计划（v2.33）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在首次 archive 时从对话内容识别场景（Coding/Writing/Research 等 7 类），写入 session 元数据，后续该 session 的 archive 读取元数据应用识别场景，解决 Trae/Cursor 等 Agent 里写非 coding 任务时 5 维配置错配的痛点。

**Architecture:** 关键词规则 + LLM 兜底的 HybridScenarioDetector，置信度 < 0.6 触发 LLM；识别结果通过 Storage trait 的新方法 `write_session_meta` / `read_session_meta` 持久化到 `sessions/{sid}/meta.json`；编排函数 `resolve_effective_scenario` 实现 4 级优先级链（用户显式 > session_meta > 识别 > Agent 默认）；archive handler 内部调用，对 LLM 透明。

**Tech Stack:** Rust + async-trait + tokio + serde + reqwest（复用 hippocampus-llm 的 `LlmDetectorConfig`）；TDD（每任务先写失败测试再实现）。

**Spec:** [docs/superpowers/specs/2026-07-06-scenario-auto-detect-design.md](../specs/2026-07-06-scenario-auto-detect-design.md)

---

## 文件结构

| 文件 | 改动类型 | 责任 |
|------|---------|------|
| `crates/hippocampus-core/src/storage.rs` | 修改 | 新增 `SessionMeta` struct + Storage trait 2 个新方法（默认实现）+ LocalStorage 实现 |
| `crates/hippocampus-core/src/sqlite.rs` | 修改 | SqliteStorage 实现：新增 `session_meta` 表 + 2 个方法 |
| `crates/hippocampus-core/src/cache.rs` | 修改 | CachedStorage 透传实现（避免默认实现使包装失效） |
| `crates/hippocampus-presets/src/builder.rs` | 修改 | 新增 `scenario_to_str` + 扩展 `scenario_from_str` 支持 `custom:` 前缀 |
| `crates/hippocampus-presets/src/scenario_detect.rs` | 新增 | `KeywordScenarioDetector` + `HttpScenarioDetector` + `HybridScenarioDetector` + `resolve_effective_scenario` |
| `crates/hippocampus-presets/src/lib.rs` | 修改 | 导出新模块 + 重导出 API |
| `crates/hippocampus-presets/Cargo.toml` | 修改 | 新增 hippocampus-llm 依赖（HttpScenarioDetector 用） |
| `crates/hippocampus-mcp/src/lib.rs` | 修改 | HippocampusMcp 新增 `scenario_detector` 字段 + `with_scenario_detector` 链式方法 + archive handler 调用 `resolve_effective_scenario` |
| `crates/hippocampus-mcp/src/main.rs` | 修改 | 新增 `build_scenario_detector` 函数 + 注入 HippocampusMcp |

**不修改的部分**：
- `crates/hippocampus-scenarios/*` — 场景数据 crate 保持纯数据
- `crates/hippocampus-core/src/archive.rs` — Archiver 本身不改
- 现有 `detect_agent_client` / `resolve_scenario_name` — 作为降级 fallback 保留

---

## Task 1: SessionMeta struct + Storage trait 扩展（默认实现）

**Files:**
- Modify: `crates/hippocampus-core/src/storage.rs`（在 `list_sessions` 方法后、trait 闭合 `}` 前插入）

- [ ] **Step 1: 在 storage.rs 顶部导入 chrono::DateTime**

文件头部已有 `use chrono::{Datelike, NaiveDateTime};`（line 43），需扩展为 `use chrono::{DateTime, Datelike, NaiveDateTime, Utc};`。

```rust
// crates/hippocampus-core/src/storage.rs line 43
use chrono::{DateTime, Datelike, NaiveDateTime, Utc};
```

- [ ] **Step 2: 新增 SessionMeta struct**

在 `Storage` trait 定义之前（约 line 49 处，`/// 存储后端 trait` 注释前）插入：

```rust
/// Session 元数据（v2.33 新增）
///
/// 首次 archive 时由 `HybridScenarioDetector` 识别生成，持久化到
/// `sessions/{session_id}/meta.json`（LocalStorage）或 `session_meta` 表（SqliteStorage）。
/// 后续该 session 的 archive 直接读取此元数据应用场景，跳过重复识别。
///
/// ## 字段
///
/// - `scenario`：稳定的场景字符串（如 "coding" / "writing" / "custom:xxx"），
///   由 `scenario_to_str` 生成，可用 `scenario_from_str` 反解析
/// - `confidence`：置信度 0.0-1.0（关键词规则按 top/(top+second) 计算，LLM 默认 0.8）
/// - `method`：识别方法（"keyword" / "llm" / "agent_default"）
/// - `detected_at`：识别时间（UTC）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionMeta {
    /// 识别的场景标签（与 `scenario_to_str` 输出一致）
    pub scenario: String,
    /// 置信度 0.0-1.0
    pub confidence: f32,
    /// 识别方法："keyword" / "llm" / "agent_default"
    pub method: String,
    /// 识别时间（UTC）
    pub detected_at: DateTime<Utc>,
}
```

- [ ] **Step 3: 在 Storage trait 末尾新增 2 个方法（默认实现）**

在 `list_sessions` 方法后、trait 闭合 `}` 前（约 line 418）插入：

```rust
    // ========================================================================
    // session 元数据（v2.33 新增，场景识别结果持久化）
    // ========================================================================

    /// 写入 session 元数据（v2.33 新增）
    ///
    /// 覆盖写入（若已存在则替换）。由 `resolve_effective_scenario` 在首次识别后调用，
    /// 失败不应阻塞 archive 主流程（调用方应忽略错误并日志 warn）。
    ///
    /// ## 默认实现
    ///
    /// 默认返回 `Ok(())`（no-op，旧后端不支持 session 元数据）。
    async fn write_session_meta(
        &self,
        _session_id: &str,
        _meta: &SessionMeta,
    ) -> crate::Result<()> {
        Ok(())
    }

    /// 读取 session 元数据（v2.33 新增）
    ///
    /// 未识别时返回 `Ok(None)`（首次 archive 前）。
    /// 由 `resolve_effective_scenario` 在每次 archive 时调用，命中则跳过识别。
    ///
    /// ## 默认实现
    ///
    /// 默认返回 `Ok(None)`（旧后端不支持 session 元数据）。
    async fn read_session_meta(
        &self,
        _session_id: &str,
    ) -> crate::Result<Option<SessionMeta>> {
        Ok(None)
    }
```

- [ ] **Step 4: 编译验证 trait 扩展不破坏现有实现**

Run: `cargo build -p hippocampus-core`
Expected: 编译通过（默认实现让 LocalStorage/SqliteStorage/CachedStorage 无需立即实现）

- [ ] **Step 5: 提交**

```bash
git add crates/hippocampus-core/src/storage.rs
git commit -m "feat(core): 新增 SessionMeta struct + Storage trait 的 write_session_meta / read_session_meta 默认实现 (v2.33)"
```

---

## Task 2: LocalStorage 实现 session_meta

**Files:**
- Modify: `crates/hippocampus-core/src/storage.rs`（在 LocalStorage 的 `impl Storage for LocalStorage` 块中，紧接 `write_project_memory` 方法后）
- Test: `crates/hippocampus-core/tests/session_meta_local.rs`（新增）

- [ ] **Step 1: 写失败测试**

新建 `crates/hippocampus-core/tests/session_meta_local.rs`：

```rust
//! LocalStorage session_meta 读写测试（v2.33）

use hippocampus_core::storage::{LocalStorage, SessionMeta, Storage};
use chrono::Utc;
use tempfile::TempDir;

fn make_meta(scenario: &str, confidence: f32, method: &str) -> SessionMeta {
    SessionMeta {
        scenario: scenario.to_string(),
        confidence,
        method: method.to_string(),
        detected_at: Utc::now(),
    }
}

#[tokio::test]
async fn test_write_then_read_session_meta() {
    let tmp = TempDir::new().unwrap();
    let storage = LocalStorage::new(tmp.path().to_path_buf());
    let sid = "test-session-1";

    let meta = make_meta("coding", 0.85, "keyword");
    storage.write_session_meta(sid, &meta).await.unwrap();

    let read = storage.read_session_meta(sid).await.unwrap();
    assert!(read.is_some(), "读取应命中已写入的 meta");
    let read = read.unwrap();
    assert_eq!(read.scenario, "coding");
    assert!((read.confidence - 0.85).abs() < 1e-6);
    assert_eq!(read.method, "keyword");
}

#[tokio::test]
async fn test_read_session_meta_returns_none_when_absent() {
    let tmp = TempDir::new().unwrap();
    let storage = LocalStorage::new(tmp.path().to_path_buf());

    let read = storage.read_session_meta("never-archived-session").await.unwrap();
    assert!(read.is_none(), "未写入的 session 应返回 None");
}

#[tokio::test]
async fn test_write_session_meta_overwrites_existing() {
    let tmp = TempDir::new().unwrap();
    let storage = LocalStorage::new(tmp.path().to_path_buf());
    let sid = "test-session-2";

    let meta1 = make_meta("coding", 0.7, "keyword");
    storage.write_session_meta(sid, &meta1).await.unwrap();

    let meta2 = make_meta("writing", 0.9, "llm");
    storage.write_session_meta(sid, &meta2).await.unwrap();

    let read = storage.read_session_meta(sid).await.unwrap().unwrap();
    assert_eq!(read.scenario, "writing", "覆盖写入应保留最新值");
    assert!((read.confidence - 0.9).abs() < 1e-6);
    assert_eq!(read.method, "llm");
}

#[tokio::test]
async fn test_session_meta_persists_custom_scenario() {
    let tmp = TempDir::new().unwrap();
    let storage = LocalStorage::new(tmp.path().to_path_buf());
    let sid = "test-session-3";

    let meta = make_meta("custom:medical", 0.65, "llm");
    storage.write_session_meta(sid, &meta).await.unwrap();

    let read = storage.read_session_meta(sid).await.unwrap().unwrap();
    assert_eq!(read.scenario, "custom:medical");
}

#[tokio::test]
async fn test_session_meta_isolation_between_sessions() {
    let tmp = TempDir::new().unwrap();
    let storage = LocalStorage::new(tmp.path().to_path_buf());

    let meta_a = make_meta("coding", 0.8, "keyword");
    let meta_b = make_meta("writing", 0.75, "llm");
    storage.write_session_meta("session-a", &meta_a).await.unwrap();
    storage.write_session_meta("session-b", &meta_b).await.unwrap();

    let read_a = storage.read_session_meta("session-a").await.unwrap().unwrap();
    let read_b = storage.read_session_meta("session-b").await.unwrap().unwrap();
    assert_eq!(read_a.scenario, "coding");
    assert_eq!(read_b.scenario, "writing");
}
```

- [ ] **Step 2: 验证测试失败（方法尚未实现，走默认 Ok(None)/Ok(())）**

Run: `cargo test -p hippocampus-core --test session_meta_local`
Expected: FAIL — `test_write_then_read_session_meta` 失败（写入后读取仍返回 None，因为默认实现是 no-op）

- [ ] **Step 3: 在 LocalStorage 的 `impl Storage` 块中实现 2 个方法**

在 `write_project_memory` 方法后（约 line 995 附近，最后一个 `}` 前）插入：

```rust
    /// 写入 session 元数据（v2.33 新增）
    ///
    /// 覆盖写入 `sessions/{session_id}/meta.json`。
    /// 不加 session 写锁（与 session_state.json 同理，无并发冲突风险）。
    async fn write_session_meta(
        &self,
        session_id: &str,
        meta: &SessionMeta,
    ) -> crate::Result<()> {
        let relative = self.session_scope_dir(session_id).join("meta.json");
        let abs = self.abs_path(&relative);
        self.ensure_parent_dir(&abs).await?;

        let json = serde_json::to_vec_pretty(meta)
            .map_err(|e| crate::Error::Serialize(format!("序列化 SessionMeta 失败: {}", e)))?;

        self.atomic_write(&abs, &json).await?;

        tracing::debug!(
            session_id = %session_id,
            scenario = %meta.scenario,
            confidence = meta.confidence,
            method = %meta.method,
            "session meta 已写入"
        );

        Ok(())
    }

    /// 读取 session 元数据（v2.33 新增）
    ///
    /// 从 `sessions/{session_id}/meta.json` 读取。
    /// 文件不存在时返回 `Ok(None)`（首次 archive 前）。
    async fn read_session_meta(
        &self,
        session_id: &str,
    ) -> crate::Result<Option<SessionMeta>> {
        let relative = self.session_scope_dir(session_id).join("meta.json");
        let abs = self.abs_path(&relative);

        match tokio::fs::read(&abs).await {
            Ok(content) => {
                let meta: SessionMeta = serde_json::from_slice(&content)
                    .map_err(|e| crate::Error::Serialize(format!(
                        "反序列化 SessionMeta 失败: {}", e
                    )))?;
                Ok(Some(meta))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(crate::Error::Storage(format!(
                "读取 meta.json 失败 {:?}: {}",
                path_display(&relative), e
            ))),
        }
    }
```

- [ ] **Step 4: 验证测试通过**

Run: `cargo test -p hippocampus-core --test session_meta_local`
Expected: PASS — 全部 5 个测试通过

- [ ] **Step 5: 提交**

```bash
git add crates/hippocampus-core/src/storage.rs crates/hippocampus-core/tests/session_meta_local.rs
git commit -m "feat(core): LocalStorage 实现 write_session_meta / read_session_meta (v2.33)"
```

---

## Task 3: SqliteStorage 实现 session_meta

**Files:**
- Modify: `crates/hippocampus-core/src/sqlite.rs`（INIT_SQL 新增表 + impl Storage 新增 2 方法）
- Test: `crates/hippocampus-core/tests/session_meta_sqlite.rs`（新增）

- [ ] **Step 1: 写失败测试**

新建 `crates/hippocampus-core/tests/session_meta_sqlite.rs`：

```rust
//! SqliteStorage session_meta 读写测试（v2.33）

use hippocampus_core::serialization::SerializationFormat;
use hippocampus_core::sqlite::SqliteStorage;
use hippocampus_core::storage::{SessionMeta, Storage};
use chrono::Utc;
use tempfile::TempDir;

fn make_meta(scenario: &str, confidence: f32, method: &str) -> SessionMeta {
    SessionMeta {
        scenario: scenario.to_string(),
        confidence,
        method: method.to_string(),
        detected_at: Utc::now(),
    }
}

fn make_storage(tmp: &TempDir) -> SqliteStorage {
    SqliteStorage::with_format(
        tmp.path().to_path_buf(),
        None,
        SerializationFormat::Json,
    )
    .unwrap()
}

#[tokio::test]
async fn test_sqlite_write_then_read_session_meta() {
    let tmp = TempDir::new().unwrap();
    let storage = make_storage(&tmp);
    let sid = "sqlite-session-1";

    let meta = make_meta("coding", 0.85, "keyword");
    storage.write_session_meta(sid, &meta).await.unwrap();

    let read = storage.read_session_meta(sid).await.unwrap();
    assert!(read.is_some());
    let read = read.unwrap();
    assert_eq!(read.scenario, "coding");
    assert!((read.confidence - 0.85).abs() < 1e-6);
    assert_eq!(read.method, "keyword");
}

#[tokio::test]
async fn test_sqlite_read_returns_none_when_absent() {
    let tmp = TempDir::new().unwrap();
    let storage = make_storage(&tmp);

    let read = storage.read_session_meta("never-existed").await.unwrap();
    assert!(read.is_none());
}

#[tokio::test]
async fn test_sqlite_write_overwrites_existing() {
    let tmp = TempDir::new().unwrap();
    let storage = make_storage(&tmp);
    let sid = "sqlite-session-2";

    let meta1 = make_meta("coding", 0.7, "keyword");
    storage.write_session_meta(sid, &meta1).await.unwrap();

    let meta2 = make_meta("writing", 0.9, "llm");
    storage.write_session_meta(sid, &meta2).await.unwrap();

    let read = storage.read_session_meta(sid).await.unwrap().unwrap();
    assert_eq!(read.scenario, "writing");
    assert_eq!(read.method, "llm");
}
```

- [ ] **Step 2: 验证测试失败**

Run: `cargo test -p hippocampus-core --test session_meta_sqlite`
Expected: FAIL — `test_sqlite_write_then_read_session_meta` 失败（默认实现 Ok(None)）

- [ ] **Step 3: 在 INIT_SQL 末尾新增 session_meta 表**

在 `crates/hippocampus-core/src/sqlite.rs` 的 `INIT_SQL` 常量末尾（约 line 113，`CREATE INDEX IF NOT EXISTS idx_hooks_memory ...` 后，闭合 `"#;` 前）追加：

```sql
CREATE TABLE IF NOT EXISTS session_meta (
    session_id  TEXT PRIMARY KEY,
    scenario    TEXT NOT NULL,
    confidence  REAL NOT NULL,
    method      TEXT NOT NULL,
    detected_at TEXT NOT NULL
);
```

完整上下文：

```rust
const INIT_SQL: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA busy_timeout = 5000;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS memories (
    memory_id   TEXT PRIMARY KEY,
    session_id  TEXT NOT NULL,
    project_id  TEXT,
    period      TEXT NOT NULL,
    archived_at TEXT NOT NULL,
    content     BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_memories_session ON memories(session_id, period);
CREATE INDEX IF NOT EXISTS idx_memories_project ON memories(project_id, period);

CREATE TABLE IF NOT EXISTS hooks (
    hook_id        TEXT NOT NULL,
    memory_id      TEXT NOT NULL,
    session_id     TEXT NOT NULL,
    project_id     TEXT,
    period         TEXT NOT NULL,
    summary_title  TEXT NOT NULL,
    tags           TEXT NOT NULL,
    archived_at    TEXT NOT NULL,
    token_count    INTEGER NOT NULL,
    scope          TEXT NOT NULL,
    PRIMARY KEY (hook_id, scope),
    FOREIGN KEY (memory_id) REFERENCES memories(memory_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_hooks_session ON hooks(session_id, period, scope);
CREATE INDEX IF NOT EXISTS idx_hooks_project ON hooks(project_id, period, scope);
CREATE INDEX IF NOT EXISTS idx_hooks_memory ON hooks(memory_id);

CREATE TABLE IF NOT EXISTS session_meta (
    session_id  TEXT PRIMARY KEY,
    scenario    TEXT NOT NULL,
    confidence  REAL NOT NULL,
    method      TEXT NOT NULL,
    detected_at TEXT NOT NULL
);
"#;
```

- [ ] **Step 4: 在 sqlite.rs 顶部导入 SessionMeta**

修改 `crates/hippocampus-core/src/sqlite.rs` line 35 的导入：

```rust
use crate::model::{ArchivePeriod, IndexDocument, IndexHook, MemoryFile, MemoryUpdate};
use crate::serialization::SerializationFormat;
use crate::storage::{SessionMeta, Storage};
```

- [ ] **Step 5: 在 `impl Storage for SqliteStorage` 块末尾新增 2 方法**

在 sqlite.rs 的 `impl Storage for SqliteStorage` 块中（最后一个方法后、闭合 `}` 前）追加。先找到 impl 块的结束位置（用 Grep 搜索 `impl Storage for SqliteStorage`）。

```rust
    /// 写入 session 元数据（v2.33 新增）
    async fn write_session_meta(
        &self,
        session_id: &str,
        meta: &SessionMeta,
    ) -> crate::Result<()> {
        let pool = self.pool.clone();
        let sid = session_id.to_string();
        let scenario = meta.scenario.clone();
        let confidence = meta.confidence;
        let method = meta.method.clone();
        let detected_at = meta.detected_at.to_rfc3339();

        tokio::task::spawn_blocking(move || {
            let conn = pool.get()
                .map_err(|e| crate::Error::Storage(format!("获取连接失败: {}", e)))?;
            conn.execute(
                "INSERT OR REPLACE INTO session_meta (session_id, scenario, confidence, method, detected_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![sid, scenario, confidence, method, detected_at],
            ).map_err(|e| crate::Error::Storage(format!("写入 session_meta 失败: {}", e)))?;
            Ok(())
        })
        .await
        .map_err(|e| crate::Error::Storage(format!("任务调度失败: {}", e)))??;

        tracing::debug!(
            session_id = %session_id,
            scenario = %meta.scenario,
            "session_meta 已写入 SQLite"
        );
        Ok(())
    }

    /// 读取 session 元数据（v2.33 新增）
    async fn read_session_meta(
        &self,
        session_id: &str,
    ) -> crate::Result<Option<SessionMeta>> {
        let pool = self.pool.clone();
        let sid = session_id.to_string();

        let result = tokio::task::spawn_blocking(move || {
            let conn = pool.get()
                .map_err(|e| crate::Error::Storage(format!("获取连接失败: {}", e)))?;
            let mut stmt = conn.prepare(
                "SELECT scenario, confidence, method, detected_at FROM session_meta WHERE session_id = ?1"
            ).map_err(|e| crate::Error::Storage(format!("prepare 失败: {}", e)))?;

            let row_result: rusqlite::Result<Option<(String, f64, String, String)>> =
                stmt.query_row(rusqlite::params![sid], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                });

            match row_result {
                Ok(Some((scenario, confidence, method, detected_at))) => {
                    let dt = chrono::DateTime::parse_from_rfc3339(&detected_at)
                        .map_err(|e| crate::Error::Serialize(format!(
                            "解析 detected_at 失败: {}", e
                        )))?
                        .with_timezone(&chrono::Utc);
                    Ok(Some(SessionMeta {
                        scenario,
                        confidence: confidence as f32,
                        method,
                        detected_at: dt,
                    }))
                }
                Ok(None) => Ok(None),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(crate::Error::Storage(format!(
                    "查询 session_meta 失败: {}", e
                ))),
            }
        })
        .await
        .map_err(|e| crate::Error::Storage(format!("任务调度失败: {}", e)))?;

        result
    }
```

**注意**：`query_row` 在无行时返回 `Err(QueryReturnedNoRows)`，需在 match 中转换为 `Ok(None)`。但 `query_row` 的返回类型是 `Result<T>`（不是 `Result<Option<T>>`），上面的代码使用 `Ok(Some(...))` / `Ok(None)` 是错的，正确写法是先 query_row 返回 `Result<(String, f64, String, String)>`，然后 match：

修正后的代码（替换上面的 `let row_result` 块）：

```rust
            let row_result: rusqlite::Result<(String, f64, String, String)> =
                stmt.query_row(rusqlite::params![sid], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                });

            match row_result {
                Ok((scenario, confidence, method, detected_at)) => {
                    let dt = chrono::DateTime::parse_from_rfc3339(&detected_at)
                        .map_err(|e| crate::Error::Serialize(format!(
                            "解析 detected_at 失败: {}", e
                        )))?
                        .with_timezone(&chrono::Utc);
                    Ok(Some(SessionMeta {
                        scenario,
                        confidence: confidence as f32,
                        method,
                        detected_at: dt,
                    }))
                }
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(crate::Error::Storage(format!(
                    "查询 session_meta 失败: {}", e
                ))),
            }
```

- [ ] **Step 6: 确认 sqlite.rs 中 `use chrono` 已存在**

Run: `cargo build -p hippocampus-core`
Expected: 编译通过。若提示 `chrono::Utc` / `chrono::DateTime` 未导入，在 sqlite.rs 顶部追加 `use chrono::Utc;`（line 38 `use chrono::{DateTime, Utc};` 应已存在，确认即可）。

- [ ] **Step 7: 验证测试通过**

Run: `cargo test -p hippocampus-core --test session_meta_sqlite`
Expected: PASS — 全部 3 个测试通过

- [ ] **Step 8: 提交**

```bash
git add crates/hippocampus-core/src/sqlite.rs crates/hippocampus-core/tests/session_meta_sqlite.rs
git commit -m "feat(core): SqliteStorage 实现 session_meta 表 + write/read 方法 (v2.33)"
```

---

## Task 4: CachedStorage 透传 session_meta

**Files:**
- Modify: `crates/hippocampus-core/src/cache.rs`（impl Storage 块中新增 2 方法）

**理由**：CachedStorage 装饰任意 Storage 后端，若走 trait 默认实现（Ok(None)/Ok(())），包装 LocalStorage/SqliteStorage 时 session_meta 会失效。必须透传到 inner。

- [ ] **Step 1: 写失败测试**

新建 `crates/hippocampus-core/tests/session_meta_cached.rs`：

```rust
//! CachedStorage session_meta 透传测试（v2.33）

use hippocampus_core::cache::CachedStorage;
use hippocampus_core::storage::{LocalStorage, SessionMeta, Storage};
use chrono::Utc;
use tempfile::TempDir;

fn make_meta(scenario: &str) -> SessionMeta {
    SessionMeta {
        scenario: scenario.to_string(),
        confidence: 0.8,
        method: "keyword".to_string(),
        detected_at: Utc::now(),
    }
}

#[tokio::test]
async fn test_cached_storage_passes_through_write_and_read() {
    let tmp = TempDir::new().unwrap();
    let inner = LocalStorage::new(tmp.path().to_path_buf());
    let cached = CachedStorage::new(inner);

    let meta = make_meta("coding");
    cached.write_session_meta("sess-cached", &meta).await.unwrap();

    // 通过 CachedStorage 读取，应命中 inner 的写入
    let read = cached.read_session_meta("sess-cached").await.unwrap();
    assert!(read.is_some(), "CachedStorage 应透传到 inner");
    assert_eq!(read.unwrap().scenario, "coding");
}

#[tokio::test]
async fn test_cached_storage_read_none_when_inner_absent() {
    let tmp = TempDir::new().unwrap();
    let inner = LocalStorage::new(tmp.path().to_path_buf());
    let cached = CachedStorage::new(inner);

    let read = cached.read_session_meta("never-existed").await.unwrap();
    assert!(read.is_none());
}
```

- [ ] **Step 2: 验证测试失败**

Run: `cargo test -p hippocampus-core --test session_meta_cached`
Expected: FAIL — `test_cached_storage_passes_through_write_and_read` 失败（默认 Ok(None)）

- [ ] **Step 3: 在 cache.rs 顶部导入 SessionMeta**

修改 `crates/hippocampus-core/src/cache.rs` line 42：

```rust
use crate::model::{ArchivePeriod, IndexDocument, IndexHook, MemoryFile, MemoryUpdate};
use crate::storage::{SessionMeta, Storage};
```

- [ ] **Step 4: 在 `impl Storage for CachedStorage<T>` 块末尾新增 2 方法**

在 cache.rs 中找到 `impl<T: Storage> Storage for CachedStorage<T>` 块（用 Grep 搜索），在最后一个方法后、闭合 `}` 前追加：

```rust
    /// 透传 session 元数据写入到 inner（v2.33 新增）
    ///
    /// CachedStorage 不单独缓存 session_meta（读取频率低，每个 session 仅首次 archive 时读一次）。
    async fn write_session_meta(
        &self,
        session_id: &str,
        meta: &SessionMeta,
    ) -> crate::Result<()> {
        self.inner.write_session_meta(session_id, meta).await
    }

    /// 透传 session 元数据读取到 inner（v2.33 新增）
    async fn read_session_meta(
        &self,
        session_id: &str,
    ) -> crate::Result<Option<SessionMeta>> {
        self.inner.read_session_meta(session_id).await
    }
```

- [ ] **Step 5: 验证测试通过**

Run: `cargo test -p hippocampus-core --test session_meta_cached`
Expected: PASS — 全部 2 个测试通过

- [ ] **Step 6: 提交**

```bash
git add crates/hippocampus-core/src/cache.rs crates/hippocampus-core/tests/session_meta_cached.rs
git commit -m "feat(core): CachedStorage 透传 session_meta 到 inner (v2.33)"
```

---

## Task 5: scenario_to_str + scenario_from_str 扩展 custom: 前缀

**Files:**
- Modify: `crates/hippocampus-presets/src/builder.rs`（新增 scenario_to_str + 扩展 scenario_from_str）
- Test: 内联在 builder.rs 的 `#[cfg(test)] mod tests` 中

**动机**：`format!("{:?}", scenario).to_lowercase()` 对 `Custom("xxx")` 会生成 `custom("xxx")`，无法被 `scenario_from_str` 反解析。需新增稳定的字符串序列化函数。

- [ ] **Step 1: 在 builder.rs 测试模块中写失败测试**

在 `crates/hippocampus-presets/src/builder.rs` 的 `#[cfg(test)] mod tests` 块末尾（最后一个 `}` 前）追加：

```rust
    // ========================================================================
    // v2.33 新增：scenario_to_str / scenario_from_str 互逆性测试
    // ========================================================================

    #[test]
    fn test_scenario_to_str_builtin_scenarios() {
        use hippocampus_scenarios::Scenario;
        assert_eq!(scenario_to_str(&Scenario::Coding), "coding");
        assert_eq!(scenario_to_str(&Scenario::Writing), "writing");
        assert_eq!(scenario_to_str(&Scenario::Research), "research");
        assert_eq!(scenario_to_str(&Scenario::Daily), "daily");
        assert_eq!(scenario_to_str(&Scenario::Finance), "finance");
        assert_eq!(scenario_to_str(&Scenario::Design), "design");
        assert_eq!(scenario_to_str(&Scenario::OfficeWork), "officework");
    }

    #[test]
    fn test_scenario_to_str_custom_with_prefix() {
        use hippocampus_scenarios::Scenario;
        assert_eq!(scenario_to_str(&Scenario::Custom("medical".into())), "custom:medical");
        assert_eq!(scenario_to_str(&Scenario::Custom("".into())), "custom:");
    }

    #[test]
    fn test_scenario_roundtrip_builtin() {
        use hippocampus_scenarios::Scenario;
        for s in [
            Scenario::Coding,
            Scenario::Writing,
            Scenario::Research,
            Scenario::Daily,
            Scenario::Finance,
            Scenario::Design,
            Scenario::OfficeWork,
        ] {
            let s_str = scenario_to_str(&s);
            let back = scenario_from_str(&s_str);
            assert_eq!(s, back, "互逆失败: {} 往返后变 {:?}", s_str, back);
        }
    }

    #[test]
    fn test_scenario_roundtrip_custom() {
        use hippocampus_scenarios::Scenario;
        let original = Scenario::Custom("medical".into());
        let s_str = scenario_to_str(&original);
        let back = scenario_from_str(&s_str);
        assert_eq!(original, back, "Custom 互逆失败: {}", s_str);
    }

    #[test]
    fn test_scenario_from_str_custom_prefix() {
        use hippocampus_scenarios::Scenario;
        // 直接解析 "custom:xxx" 应得到 Custom("xxx")
        assert_eq!(scenario_from_str("custom:medical"), Scenario::Custom("medical".into()));
        // 大小写不敏感
        assert_eq!(scenario_from_str("CUSTOM:medical"), Scenario::Custom("medical".into()));
    }

    #[test]
    fn test_scenario_from_str_unknown_falls_back_to_custom() {
        use hippocampus_scenarios::Scenario;
        // 未知标签（无 custom: 前缀）兜底为 Custom(s)
        assert_eq!(scenario_from_str("unknown_tag"), Scenario::Custom("unknown_tag".into()));
    }
```

- [ ] **Step 2: 验证测试失败**

Run: `cargo test -p hippocampus-presets --lib scenario_to_str`
Expected: FAIL — `scenario_to_str` 函数未定义

- [ ] **Step 3: 在 builder.rs 中新增 scenario_to_str 函数**

在 `scenario_from_str` 函数前（约 line 316，`/// 字符串解析为 Scenario` 注释前）插入：

```rust
/// Scenario 枚举转稳定字符串（v2.33 新增）
///
/// 与 [`scenario_from_str`] 互逆，用于 session_meta 持久化。
///
/// ## 映射
///
/// - `Coding` → `"coding"`
/// - `Writing` → `"writing"`
/// - `Research` → `"research"`
/// - `Daily` → `"daily"`
/// - `Finance` → `"finance"`
/// - `Design` → `"design"`
/// - `OfficeWork` → `"officework"`
/// - `Custom(s)` → `"custom:{s}"`（带前缀避免与内置场景冲突）
///
/// ## 注意
///
/// 不用 `format!("{:?}", scenario).to_lowercase()`，因为对 `Custom("xxx")`
/// 会生成 `custom("xxx")`，无法被 `scenario_from_str` 反解析。
pub fn scenario_to_str(scenario: &hippocampus_scenarios::Scenario) -> String {
    use hippocampus_scenarios::Scenario;
    match scenario {
        Scenario::Coding => "coding".to_string(),
        Scenario::Writing => "writing".to_string(),
        Scenario::Research => "research".to_string(),
        Scenario::Daily => "daily".to_string(),
        Scenario::Finance => "finance".to_string(),
        Scenario::Design => "design".to_string(),
        Scenario::OfficeWork => "officework".to_string(),
        Scenario::Custom(s) => format!("custom:{}", s),
    }
}
```

- [ ] **Step 4: 扩展 scenario_from_str 支持 custom: 前缀**

将 `scenario_from_str` 函数（约 line 320-332）整体替换为：

```rust
/// 字符串解析为 Scenario（大小写不敏感，v2.33 扩展 custom: 前缀）
///
/// 支持的别名：
/// - `coding` / `writing` / `research` / `daily` / `finance` / `design`
/// - `officework` / `office` / `work`
/// - `custom:{s}`（v2.33 新增，与 `scenario_to_str` 互逆）
/// - 其他字符串：兜底返回 `Scenario::Custom(s)`（向后兼容）
pub fn scenario_from_str(s: &str) -> hippocampus_scenarios::Scenario {
    let lower = s.to_lowercase();
    if let Some(custom_val) = lower.strip_prefix("custom:") {
        return hippocampus_scenarios::Scenario::Custom(custom_val.to_string());
    }
    match lower.as_str() {
        "coding" => hippocampus_scenarios::Scenario::Coding,
        "writing" => hippocampus_scenarios::Scenario::Writing,
        "research" => hippocampus_scenarios::Scenario::Research,
        "daily" => hippocampus_scenarios::Scenario::Daily,
        "finance" => hippocampus_scenarios::Scenario::Finance,
        "design" => hippocampus_scenarios::Scenario::Design,
        "officework" | "office" | "work" => hippocampus_scenarios::Scenario::OfficeWork,
        _ => hippocampus_scenarios::Scenario::Custom(s.to_string()),
    }
}
```

- [ ] **Step 5: 验证测试通过**

Run: `cargo test -p hippocampus-presets --lib scenario_`
Expected: PASS — 全部 6 个新测试通过，原有测试不受影响

- [ ] **Step 6: 提交**

```bash
git add crates/hippocampus-presets/src/builder.rs
git commit -m "feat(presets): scenario_to_str + scenario_from_str 支持 custom: 前缀 (v2.33)"
```

---

## Task 6: KeywordScenarioDetector + 关键词字典

**Files:**
- Create: `crates/hippocampus-presets/src/scenario_detect.rs`
- Modify: `crates/hippocampus-presets/Cargo.toml`（新增 hippocampus-llm 依赖，本任务先加 hippocampus-core 依赖）

- [ ] **Step 1: 在 Cargo.toml 中新增依赖**

修改 `crates/hippocampus-presets/Cargo.toml`，在 `[dependencies]` 末尾追加：

```toml
hippocampus-llm = { path = "../hippocampus-llm", version = "0.1.0" }
```

完整 `[dependencies]` 段：

```toml
[dependencies]
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
hippocampus-core = { path = "../hippocampus-core", version = "0.1.0" }
hippocampus-models = { path = "../hippocampus-models", version = "0.1.0" }
hippocampus-scenarios = { path = "../hippocampus-scenarios", version = "0.1.0" }
hippocampus-windows = { path = "../hippocampus-windows", version = "0.1.0" }
hippocampus-agents = { path = "../hippocampus-agents", version = "0.1.0" }
hippocampus-skills = { path = "../hippocampus-skills", version = "0.1.0" }
hippocampus-llm = { path = "../hippocampus-llm", version = "0.1.0" }
```

- [ ] **Step 2: 创建 scenario_detect.rs 文件骨架**

创建 `crates/hippocampus-presets/src/scenario_detect.rs`：

```rust
//! # 场景识别模块（v2.33）
//!
//! 从对话内容自动推断 Scenario（Coding/Writing/Research 等 7 类），
//! 解决 Trae/Cursor 等 Agent 里写非 coding 任务时 5 维配置错配的痛点。
//!
//! ## 架构
//!
//! ```text
//! KeywordScenarioDetector  ──┐   HttpScenarioDetector ──┐
//!  (纯算法，零依赖)          │   (LLM 推断)            │
//!  7 场景 × ~15 关键词        │   复用 LlmDetectorConfig │
//!  返回 (Scenario, f32)       │                         │
//! └─────┬───────────────────┘ └────────┬────────────────┘
//!       │                                │
//!       └────────────┬───────────────────┘
//!                    ▼
//!      HybridScenarioDetector
//!       (串联关键词 + LLM 兜底)
//!       置信度 < 0.6 时调 LLM
//!                    │
//!                    ▼ return DetectionResult
//!      resolve_effective_scenario (编排函数)
//!       1. 用户显式 > 2. session_meta > 3. 识别 > 4. Agent 默认
//! ```
//!
//! ## 设计要点
//!
//! - **首次识别**：仅在首次 archive 时调用，后续读取 session_meta 跳过
//! - **失败降级**：识别失败永不阻塞 archive，降级到 Agent 默认场景
//! - **跨进程持久**：识别结果写入 `sessions/{sid}/meta.json`

use hippocampus_core::model::MessageTurn;
use hippocampus_scenarios::Scenario;
use std::sync::Arc;

// ============================================================================
// 关键词字典
// ============================================================================

/// 关键词字典：7 场景 × ~15 关键词
///
/// 子串匹配（大小写不敏感），统计每个场景命中数。
fn keyword_dict() -> Vec<(Scenario, Vec<&'static str>)> {
    vec![
        (Scenario::Coding, vec![
            "fn ", "class ", "def ", "function", "bug", "compile", "commit", "refactor",
            "api", "函数", "编译", "重构", "报错", "调试", "架构",
        ]),
        (Scenario::Writing, vec![
            "文章", "论点", "论据", "素材", "风格", "段落", "开头", "结尾", "修辞",
            "article", "essay", "draft", "outline", "narrative", "tone",
        ]),
        (Scenario::Research, vec![
            "假设", "方法", "数据", "结论", "引用", "文献", "实验", "样本", "论文",
            "hypothesis", "methodology", "conclusion", "citation", "abstract",
        ]),
        (Scenario::Daily, vec![
            "今天", "昨天", "吃饭", "天气", "心情", "朋友", "周末", "电影", "购物",
            "约会", "family", "dinner", "weather", "mood", "weekend",
        ]),
        (Scenario::Finance, vec![
            "交易", "金额", "收益", "风险", "投资", "股票", "基金", "利率", "止损",
            "portfolio", "stock", "bond", "dividend", "volatility", "hedge",
        ]),
        (Scenario::Design, vec![
            "设计", "原型", "用户", "界面", "交互", "迭代", "视觉", "反馈",
            "mockup", "wireframe", "ui", "ux", "persona", "iteration",
        ]),
        (Scenario::OfficeWork, vec![
            "会议", "待办", "文档", "决议", "项目", "截止", "参会", "纪要",
            "meeting", "todo", "memo", "deadline", "agenda", "minutes",
        ]),
    ]
}

// ============================================================================
// DetectionResult
// ============================================================================

/// 识别结果
#[derive(Debug, Clone)]
pub struct DetectionResult {
    /// 识别的场景（None 表示识别失败，调用方应降级）
    pub scenario: Option<Scenario>,
    /// 置信度 0.0-1.0（关键词规则按 top/(top+second) 计算，LLM 默认 0.8）
    pub confidence: f32,
    /// 识别方法："keyword" / "llm" / "failed"
    pub method: &'static str,
}

impl DetectionResult {
    /// 识别失败
    pub fn failed() -> Self {
        Self {
            scenario: None,
            confidence: 0.0,
            method: "failed",
        }
    }

    /// 是否识别失败
    pub fn is_failed(&self) -> bool {
        self.scenario.is_none()
    }
}

// ============================================================================
// KeywordScenarioDetector
// ============================================================================

/// 关键词规则场景识别器（纯算法，零依赖）
///
/// 子串匹配（大小写不敏感）+ 置信度计算：
/// - `confidence = top / (top + second)`
/// - `>= 0.6` 算高置信，直接采用
/// - `< 0.6` 触发 LLM 兜底
/// - 全部零命中 → 返回 None
pub struct KeywordScenarioDetector;

impl KeywordScenarioDetector {
    pub fn new() -> Self {
        Self
    }

    /// 从对话轮次提取文本（拼接 user_message.text + llm_message.text）
    fn extract_text(turns: &[MessageTurn]) -> String {
        let mut text = String::new();
        for turn in turns {
            if let Some(t) = &turn.user_message.text {
                text.push_str(t);
                text.push(' ');
            }
            if let Some(t) = &turn.llm_message.text {
                text.push_str(t);
                text.push(' ');
            }
        }
        text.to_lowercase()
    }

    /// 关键词匹配，返回 (场景, 命中数) 列表，按命中数降序
    fn count_hits(text: &str) -> Vec<(Scenario, usize)> {
        let dict = keyword_dict();
        let mut hits: Vec<(Scenario, usize)> = dict
            .into_iter()
            .map(|(scenario, keywords)| {
                let count = keywords
                    .iter()
                    .filter(|kw| text.contains(*kw))
                    .count();
                (scenario, count)
            })
            .filter(|(_, c)| *c > 0)
            .collect();
        hits.sort_by(|a, b| b.1.cmp(&a.1));
        hits
    }

    /// 识别场景
    ///
    /// 返回 `Some((Scenario, confidence))` 或 `None`（零命中）。
    pub fn detect(&self, turns: &[MessageTurn]) -> Option<(Scenario, f32)> {
        let text = Self::extract_text(turns);
        let hits = Self::count_hits(&text);

        if hits.is_empty() {
            return None;
        }

        let top = hits[0];
        let top_count = top.1;

        let confidence = if hits.len() >= 2 {
            let second_count = hits[1].1;
            top_count as f32 / (top_count + second_count) as f32
        } else {
            // 只有一个场景命中 → 高置信
            1.0
        };

        Some((top.0, confidence))
    }
}

impl Default for KeywordScenarioDetector {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use hippocampus_core::model::{MessageContent, MessageTurn};
    use chrono::Utc;
    use uuid::Uuid;

    fn make_turn(user: &str, llm: &str) -> MessageTurn {
        MessageTurn {
            id: Uuid::new_v4(),
            user_message: MessageContent {
                text: Some(user.to_string()),
                attachments: vec![],
                tool_calls: vec![],
                thinking: None,
            },
            llm_message: MessageContent {
                text: Some(llm.to_string()),
                attachments: vec![],
                tool_calls: vec![],
                thinking: None,
            },
            tags: vec![],
            timestamp: Utc::now(),
            token_count: 100,
        }
    }

    // ========================================================================
    // KeywordScenarioDetector 测试
    // ========================================================================

    #[test]
    fn test_keyword_detect_coding() {
        let turns = vec![
            make_turn("帮我写一个 Rust 函数", "好的，fn 主体如下..."),
            make_turn("这里报错了", "调试一下，可能是架构问题。"),
        ];
        let detector = KeywordScenarioDetector::new();
        let result = detector.detect(&turns);
        assert!(result.is_some(), "应识别到 Coding");
        let (scenario, conf) = result.unwrap();
        assert_eq!(scenario, Scenario::Coding);
        assert!(conf >= 0.6, "Coding 单场景命中应高置信: {}", conf);
    }

    #[test]
    fn test_keyword_detect_writing() {
        let turns = vec![
            make_turn("帮我写一篇文章", "好的，先列大纲。论点是什么？"),
            make_turn("论据需要充实", "段落开头可以用修辞。"),
        ];
        let detector = KeywordScenarioDetector::new();
        let (scenario, _) = detector.detect(&turns).unwrap();
        assert_eq!(scenario, Scenario::Writing);
    }

    #[test]
    fn test_keyword_detect_research() {
        let turns = vec![
            make_turn("假设是什么", "根据数据，假设是..."),
            make_turn("引用哪篇文献", "结论在论文的 abstract。"),
        ];
        let detector = KeywordScenarioDetector::new();
        let (scenario, _) = detector.detect(&turns).unwrap();
        assert_eq!(scenario, Scenario::Research);
    }

    #[test]
    fn test_keyword_detect_daily() {
        let turns = vec![
            make_turn("今天天气怎么样", "周末适合看电影。"),
            make_turn("和朋友吃饭", "好的，心情不错。"),
        ];
        let detector = KeywordScenarioDetector::new();
        let (scenario, _) = detector.detect(&turns).unwrap();
        assert_eq!(scenario, Scenario::Daily);
    }

    #[test]
    fn test_keyword_detect_finance() {
        let turns = vec![
            make_turn("这只股票怎么样", "投资有风险，建议止损。"),
            make_turn("基金收益如何", "portfolio 配置需要分散。"),
        ];
        let detector = KeywordScenarioDetector::new();
        let (scenario, _) = detector.detect(&turns).unwrap();
        assert_eq!(scenario, Scenario::Finance);
    }

    #[test]
    fn test_keyword_detect_design() {
        let turns = vec![
            make_turn("UI 设计问题", "wireframe 先画一下"),
            make_turn("用户体验如何", "persona 和交互迭代。"),
        ];
        let detector = KeywordScenarioDetector::new();
        let (scenario, _) = detector.detect(&turns).unwrap();
        assert_eq!(scenario, Scenario::Design);
    }

    #[test]
    fn test_keyword_detect_officework() {
        let turns = vec![
            make_turn("帮我写会议纪要", "agenda 如下，参会人..."),
            make_turn("项目截止日期", "deadline 是下周，待办事项..."),
        ];
        let detector = KeywordScenarioDetector::new();
        let (scenario, _) = detector.detect(&turns).unwrap();
        assert_eq!(scenario, Scenario::OfficeWork);
    }

    #[test]
    fn test_keyword_detect_empty_turns_returns_none() {
        let detector = KeywordScenarioDetector::new();
        assert!(detector.detect(&[]).is_none());
    }

    #[test]
    fn test_keyword_detect_zero_hits_returns_none() {
        let turns = vec![
            make_turn("啊啊啊", "嗯嗯嗯"),
            make_turn("哦哦哦", "呃呃呃"),
        ];
        let detector = KeywordScenarioDetector::new();
        assert!(detector.detect(&turns).is_none(), "零命中应返回 None");
    }

    #[test]
    fn test_keyword_detect_single_scenario_high_confidence() {
        // 只命中一个场景 → confidence = 1.0
        let turns = vec![make_turn("fn compile refactor", "好的")];
        let detector = KeywordScenarioDetector::new();
        let (scenario, conf) = detector.detect(&turns).unwrap();
        assert_eq!(scenario, Scenario::Coding);
        assert_eq!(conf, 1.0);
    }

    #[test]
    fn test_keyword_detect_mixed_scenarios_lower_confidence() {
        // 同时命中 Coding 和 Writing → confidence < 1.0
        let turns = vec![make_turn(
            "fn function 文章 论点 段落 调试",
            "compile 重构"
        )];
        let detector = KeywordScenarioDetector::new();
        let (scenario, conf) = detector.detect(&turns).unwrap();
        // 哪个场景命中数多就选哪个
        let _ = (scenario, conf);
        // 关键是置信度 < 1.0（说明触发了 LLM 兜底条件）
        assert!(conf < 1.0, "混合场景置信度应 < 1.0: {}", conf);
    }
}
```

- [ ] **Step 3: 在 lib.rs 中导出 scenario_detect 模块**

修改 `crates/hippocampus-presets/src/lib.rs`（line 79-86），在 `pub mod linkage;` 后追加 `pub mod scenario_detect;`：

```rust
pub mod builder;
pub mod combined;
pub mod detect;
pub mod linkage;
pub mod scenario_detect;

pub use builder::{build_from_strings, scenario_from_str, scenario_to_str, PresetBuilder};
pub use combined::{CombinedProfile, TriggerRule, UsageProtocol};
pub use detect::{detect_agent_client, default_scenario_for_agent, resolve_scenario_name, DetectedAgent, DetectionSource};
pub use linkage::derive_window_from_agent;
pub use scenario_detect::{DetectionResult, KeywordScenarioDetector};
```

- [ ] **Step 4: 验证 KeywordScenarioDetector 测试通过**

Run: `cargo test -p hippocampus-presets --lib scenario_detect::tests::keyword_detect`
Expected: PASS — 全部 11 个 KeywordScenarioDetector 测试通过

- [ ] **Step 5: 提交**

```bash
git add crates/hippocampus-presets/Cargo.toml crates/hippocampus-presets/src/scenario_detect.rs crates/hippocampus-presets/src/lib.rs
git commit -m "feat(presets): 新增 KeywordScenarioDetector + 关键词字典 (v2.33)"
```

---

## Task 7: HttpScenarioDetector（LLM 推断器）

**Files:**
- Modify: `crates/hippocampus-presets/src/scenario_detect.rs`（在 KeywordScenarioDetector 后追加）

- [ ] **Step 1: 在 scenario_detect.rs 顶部新增 use 语句**

修改 `crates/hippocampus-presets/src/scenario_detect.rs` 的 use 段（line 50 附近）：

```rust
use hippocampus_core::model::MessageTurn;
use hippocampus_llm::LlmDetectorConfig;
use hippocampus_scenarios::Scenario;
use std::sync::Arc;
```

- [ ] **Step 2: 在 scenario_detect.rs 测试模块前追加 HttpScenarioDetector**

在 `#[cfg(test)] mod tests` 之前（KeywordScenarioDetector 的 `impl Default` 块之后）插入：

```rust
// ============================================================================
// HttpScenarioDetector
// ============================================================================

/// HTTP LLM 场景识别器
///
/// 复用 `LlmDetectorConfig`（同 `HIPPOCAMPUS_DETECTOR_*` 环境变量前缀），
/// 调用 OpenAI 兼容 API 推断对话场景。
///
/// ## Prompt 策略
///
/// 要求 LLM 严格返回 JSON：`{"scenario": "coding", "reason": "..."}`
///
/// ## 降级策略
///
/// - 未配置 API URL（config.api_url 为空）：返回 None
/// - 网络错误 / 超时 / API 错误：返回 None
/// - JSON 解析失败：返回 None
/// - 场景标签不在 7 个内置场景中：视为 `Custom(s)`
pub struct HttpScenarioDetector {
    config: LlmDetectorConfig,
    client: reqwest::Client,
}

impl HttpScenarioDetector {
    /// 创建新的 LLM 场景识别器
    pub fn new(config: LlmDetectorConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { config, client }
    }

    /// 从对话轮次提取文本（前 N 轮，默认 10 轮）
    fn build_conversation_summary(turns: &[MessageTurn], max_turns: usize) -> String {
        let take = turns.len().min(max_turns);
        let mut summary = String::new();
        for (i, turn) in turns.iter().take(take).enumerate() {
            if let Some(t) = &turn.user_message.text {
                summary.push_str(&format!("轮次 {} 用户: {}\n", i + 1, truncate(t, 200)));
            }
            if let Some(t) = &turn.llm_message.text {
                summary.push_str(&format!("轮次 {} 助手: {}\n", i + 1, truncate(t, 200)));
            }
        }
        summary
    }

    /// 构造 LLM prompt
    fn build_prompt(conversation_summary: &str) -> String {
        format!(
            r#"你是一个场景识别器。请分析以下对话内容，判断属于哪个场景。

## 可选场景标签

- coding: 编码场景（编程/调试/架构设计/code review）
- writing: 写作场景（文章/文档/创意写作）
- research: 科研场景（论文/实验/数据分析）
- daily: 日常场景（闲聊/咨询/生活）
- finance: 金融场景（交易/投资/风险分析）
- design: 设计场景（UI/UX/视觉/产品设计）
- officework: 工作场景（会议/文档/项目协作）

## 对话摘要（前 10 轮）

{conversation_summary}

## 输出要求

请只返回 JSON，不要包含任何解释或 markdown 标记。格式如下：

{{"scenario": "coding", "reason": "对话涉及 Rust 代码实现"}}

若无法判断，返回：{{"scenario": "daily", "reason": "无明显场景特征"}}"#,
            conversation_summary = conversation_summary
        )
    }

    /// 解析 LLM 返回的 JSON，提取场景标签
    fn parse_scenario(raw: &str) -> Option<Scenario> {
        // 尝试直接解析
        let value: serde_json::Value = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(_) => {
                // 尝试从 markdown 代码块中提取
                let trimmed = Self::extract_json_from_markdown(raw);
                match serde_json::from_str(&trimmed) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(error = %e, raw = %raw, "LLM 场景识别响应 JSON 解析失败");
                        return None;
                    }
                }
            }
        };

        let scenario_str = value
            .get("scenario")
            .and_then(|s| s.as_str())
            .unwrap_or("");

        if scenario_str.is_empty() {
            tracing::warn!(raw = %raw, "LLM 响应缺少 scenario 字段");
            return None;
        }

        Some(scenario_from_str(scenario_str))
    }

    /// 从 markdown 代码块中提取 JSON
    fn extract_json_from_markdown(raw: &str) -> String {
        let trimmed = raw.trim();
        if let Some(start) = trimmed.find("```") {
            let after = &trimmed[start + 3..];
            let after = after.strip_prefix("json").unwrap_or(after);
            if let Some(end) = after.find("```") {
                return after[..end].trim().to_string();
            }
        }
        trimmed.to_string()
    }

    /// 识别场景
    ///
    /// 返回 `Some(Scenario)` 或 `None`（失败时调用方应降级到 Agent 默认场景）。
    pub async fn detect(&self, turns: &[MessageTurn]) -> Option<Scenario> {
        if self.config.api_url.is_empty() {
            tracing::debug!("HttpScenarioDetector 未配置 api_url，跳过");
            return None;
        }

        let summary = Self::build_conversation_summary(turns, 10);
        let prompt = Self::build_prompt(&summary);

        let request_body = serde_json::json!({
            "model": self.config.model,
            "messages": [
                {"role": "user", "content": prompt}
            ],
            "max_tokens": self.config.max_tokens,
            "temperature": 0.0,
            "thinking": {"type": "disabled"},
        });

        let resp = match self
            .client
            .post(&self.config.api_url)
            .bearer_auth(&self.config.api_key)
            .json(&request_body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "LLM 场景识别 API 请求失败");
                return None;
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(status = %status, body = %body, "LLM 场景识别 API 返回错误状态");
            return None;
        }

        let resp_json: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "LLM 场景识别响应解析失败");
                return None;
            }
        };

        let content = resp_json
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("");

        Self::parse_scenario(content)
    }
}

/// 截断文本到指定字符数（避免 prompt 过长）
fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        s.chars().take(max_chars).collect::<String>() + "..."
    }
}
```

- [ ] **Step 3: 在测试模块中追加 HttpScenarioDetector 测试**

在 `crates/hippocampus-presets/src/scenario_detect.rs` 的 `#[cfg(test)] mod tests` 块末尾（最后一个 `}` 前）追加：

```rust
    // ========================================================================
    // HttpScenarioDetector 测试（不含真实网络调用，仅测 prompt 构造 + 解析）
    // ========================================================================

    #[test]
    fn test_http_build_prompt_contains_scenario_labels() {
        let summary = "轮次 1 用户: 写一个 Rust 函数\n轮次 1 助手: 好的\n";
        let prompt = HttpScenarioDetector::build_prompt(summary);
        assert!(prompt.contains("coding"));
        assert!(prompt.contains("writing"));
        assert!(prompt.contains("research"));
        assert!(prompt.contains("daily"));
        assert!(prompt.contains("finance"));
        assert!(prompt.contains("design"));
        assert!(prompt.contains("officework"));
        assert!(prompt.contains("轮次 1 用户: 写一个 Rust 函数"));
    }

    #[test]
    fn test_http_parse_scenario_valid_json() {
        let raw = r#"{"scenario": "coding", "reason": "Rust 代码"}"#;
        let scenario = HttpScenarioDetector::parse_scenario(raw);
        assert_eq!(scenario, Some(Scenario::Coding));
    }

    #[test]
    fn test_http_parse_scenario_markdown_wrapped() {
        let raw = "```json\n{\"scenario\": \"writing\", \"reason\": \"文章\"}\n```";
        let scenario = HttpScenarioDetector::parse_scenario(raw);
        assert_eq!(scenario, Some(Scenario::Writing));
    }

    #[test]
    fn test_http_parse_scenario_unknown_label_falls_back_to_custom() {
        let raw = r#"{"scenario": "medical", "reason": "医学对话"}"#;
        let scenario = HttpScenarioDetector::parse_scenario(raw);
        assert_eq!(scenario, Some(Scenario::Custom("medical".to_string())));
    }

    #[test]
    fn test_http_parse_scenario_missing_scenario_field() {
        let raw = r#"{"reason": "无 scenario 字段"}"#;
        let scenario = HttpScenarioDetector::parse_scenario(raw);
        assert_eq!(scenario, None);
    }

    #[test]
    fn test_http_parse_scenario_invalid_json() {
        let raw = "这不是 JSON";
        let scenario = HttpScenarioDetector::parse_scenario(raw);
        assert_eq!(scenario, None);
    }

    #[test]
    fn test_http_build_conversation_summary_truncates_long_text() {
        let long_text = "a".repeat(500);
        let turns = vec![make_turn(&long_text, &long_text)];
        let summary = HttpScenarioDetector::build_conversation_summary(&turns, 10);
        // 每段截断到 200 字符 + "..."
        assert!(summary.contains("..."));
        // 总长度应远小于原始 1000 字符
        assert!(summary.chars().count() < 600);
    }

    #[tokio::test]
    async fn test_http_detect_without_api_url_returns_none() {
        let config = LlmDetectorConfig::default(); // api_url 为空
        let detector = HttpScenarioDetector::new(config);
        let turns = vec![make_turn("test", "test")];
        assert_eq!(detector.detect(&turns).await, None);
    }

    #[test]
    fn test_http_extract_json_from_markdown_plain() {
        let raw = r#"{"scenario": "coding"}"#;
        let extracted = HttpScenarioDetector::extract_json_from_markdown(raw);
        assert_eq!(extracted, r#"{"scenario": "coding"}"#);
    }

    #[test]
    fn test_http_extract_json_from_markdown_block() {
        let raw = "```json\n{\"scenario\": \"coding\"}\n```";
        let extracted = HttpScenarioDetector::extract_json_from_markdown(raw);
        assert_eq!(extracted, r#"{"scenario": "coding"}"#);
    }
```

- [ ] **Step 4: 在测试模块顶部导入 LlmDetectorConfig**

修改 `crates/hippocampus-presets/src/scenario_detect.rs` 测试模块的 use 段（约 line 142）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use hippocampus_core::model::{MessageContent, MessageTurn};
    use hippocampus_llm::LlmDetectorConfig;
    use chrono::Utc;
    use uuid::Uuid;
```

- [ ] **Step 5: 验证测试通过**

Run: `cargo test -p hippocampus-presets --lib scenario_detect::tests::http_`
Expected: PASS — 全部 10 个 HttpScenarioDetector 测试通过

- [ ] **Step 6: 提交**

```bash
git add crates/hippocampus-presets/src/scenario_detect.rs
git commit -m "feat(presets): HttpScenarioDetector LLM 场景识别器 + prompt 构造/解析测试 (v2.33)"
```

---

## Task 8: HybridScenarioDetector + resolve_effective_scenario

**Files:**
- Modify: `crates/hippocampus-presets/src/scenario_detect.rs`（在 HttpScenarioDetector 后追加）

- [ ] **Step 1: 在 scenario_detect.rs 顶部追加 use 语句**

修改 use 段（line 50 附近）：

```rust
use hippocampus_agents::AgentFamily;
use hippocampus_core::model::MessageTurn;
use hippocampus_core::storage::{SessionMeta, Storage};
use hippocampus_llm::LlmDetectorConfig;
use hippocampus_scenarios::Scenario;
use std::sync::Arc;
```

- [ ] **Step 2: 在 scenario_detect.rs 的 truncate 函数后、`#[cfg(test)]` 前追加 HybridScenarioDetector**

```rust
// ============================================================================
// HybridScenarioDetector
// ============================================================================

/// 混合场景识别器（关键词 + LLM 兜底）
///
/// ## 串联策略
///
/// 1. 关键词规则优先
/// 2. 关键词置信度 `>= 0.6` → 直接采用，跳过 LLM
/// 3. 关键词置信度 `< 0.6` 或零命中 → 调 LLM
/// 4. LLM 失败 → 返回 `DetectionResult::failed()`
pub struct HybridScenarioDetector {
    keyword: KeywordScenarioDetector,
    llm: Option<Arc<HttpScenarioDetector>>,
}

impl HybridScenarioDetector {
    /// 创建混合识别器
    ///
    /// - `llm = None`：仅关键词模式（未配置 LLM API）
    /// - `llm = Some`：关键词 + LLM 兜底
    pub fn new(llm: Option<Arc<HttpScenarioDetector>>) -> Self {
        Self {
            keyword: KeywordScenarioDetector::new(),
            llm,
        }
    }

    /// 识别场景
    pub async fn detect(&self, turns: &[MessageTurn]) -> DetectionResult {
        // 1. 关键词规则优先
        if let Some((scenario, conf)) = self.keyword.detect(turns) {
            if conf >= 0.6 {
                tracing::debug!(
                    ?scenario,
                    confidence = conf,
                    "关键词高置信，跳过 LLM"
                );
                return DetectionResult {
                    scenario: Some(scenario),
                    confidence: conf,
                    method: "keyword",
                };
            }
            tracing::debug!(
                ?scenario,
                confidence = conf,
                "关键词低置信，触发 LLM 兜底"
            );
        } else {
            tracing::debug!("关键词零命中，触发 LLM 兜底");
        }

        // 2. LLM 兜底
        if let Some(llm) = &self.llm {
            if let Some(scenario) = llm.detect(turns).await {
                return DetectionResult {
                    scenario: Some(scenario),
                    confidence: 0.8,
                    method: "llm",
                };
            }
        }

        // 3. 全部失败
        DetectionResult::failed()
    }
}

// ============================================================================
// resolve_effective_scenario 编排函数
// ============================================================================

/// 解析生效的场景（v2.33 核心 API）
///
/// 4 级优先级链：
///
/// 1. **用户显式**（`user_explicit` 参数）最高
/// 2. **session 元数据**（已识别则跳过识别）
/// 3. **首次 archive**：调 `detector.detect(turns)` 识别 + 写入元数据
/// 4. **降级**：Agent 默认场景（[`crate::resolve_scenario_name`]）
///
/// ## 参数
///
/// - `storage`：存储 trait（读写 session_meta）
/// - `session_id`：会话 ID
/// - `user_explicit`：用户显式指定的场景（来自 preset.scenario）
/// - `agent_family`：Agent family（用于降级时推导默认场景）
/// - `detector`：场景识别器
/// - `turns`：对话内容（首次识别时用）
///
/// ## 失败容忍
///
/// - `read_session_meta` 失败：当作 None，触发重新识别
/// - `write_session_meta` 失败：日志 warn，不阻塞返回
/// - detector 识别失败：降级到 Agent 默认场景
pub async fn resolve_effective_scenario(
    storage: &dyn Storage,
    session_id: &str,
    user_explicit: Option<&str>,
    agent_family: &AgentFamily,
    detector: &HybridScenarioDetector,
    turns: &[MessageTurn],
) -> Scenario {
    // 1. 用户显式最高
    if let Some(s) = user_explicit {
        tracing::debug!(scenario = %s, "场景识别：用户显式指定");
        return scenario_from_str(s);
    }

    // 2. session 元数据（已识别）
    match storage.read_session_meta(session_id).await {
        Ok(Some(meta)) => {
            tracing::debug!(
                scenario = %meta.scenario,
                confidence = meta.confidence,
                method = %meta.method,
                "场景识别：命中 session 元数据"
            );
            return scenario_from_str(&meta.scenario);
        }
        Ok(None) => { /* 首次识别，继续 */ }
        Err(e) => {
            tracing::warn!(error = %e, "读取 session_meta 失败，触发重新识别");
        }
    }

    // 3. 首次识别
    let result = detector.detect(turns).await;
    if let Some(scenario) = result.scenario {
        let meta = SessionMeta {
            scenario: scenario_to_str(&scenario),
            confidence: result.confidence,
            method: result.method.to_string(),
            detected_at: chrono::Utc::now(),
        };
        // 写入元数据（失败不阻塞）
        if let Err(e) = storage.write_session_meta(session_id, &meta).await {
            tracing::warn!(error = %e, "写入 session_meta 失败（不阻塞 archive）");
        }
        tracing::info!(
            ?scenario,
            confidence = result.confidence,
            method = %result.method,
            "场景识别完成"
        );
        return scenario;
    }

    // 4. 降级：Agent 默认场景
    let default_str = crate::resolve_scenario_name(agent_family);
    let default = scenario_from_str(&default_str);
    tracing::info!(
        default = ?default,
        "场景识别失败，降级到 Agent 默认场景"
    );
    default
}
```

- [ ] **Step 3: 在测试模块中追加 HybridScenarioDetector + resolve_effective_scenario 测试**

在 `crates/hippocampus-presets/src/scenario_detect.rs` 测试模块末尾追加：

```rust
    // ========================================================================
    // HybridScenarioDetector 测试
    // ========================================================================

    #[tokio::test]
    async fn test_hybrid_keyword_high_confidence_skips_llm() {
        // 关键词命中明显，置信度 >= 0.6，应跳过 LLM
        let turns = vec![
            make_turn("fn compile refactor 调试", "好的，重构架构"),
        ];
        let detector = HybridScenarioDetector::new(None); // 无 LLM
        let result = detector.detect(&turns).await;
        assert!(result.scenario.is_some());
        assert_eq!(result.scenario.unwrap(), Scenario::Coding);
        assert!(result.confidence >= 0.6);
        assert_eq!(result.method, "keyword");
    }

    #[tokio::test]
    async fn test_hybrid_keyword_zero_hits_without_llm_returns_failed() {
        // 零命中 + 无 LLM → failed
        let turns = vec![make_turn("啊啊啊", "嗯嗯嗯")];
        let detector = HybridScenarioDetector::new(None);
        let result = detector.detect(&turns).await;
        assert!(result.is_failed());
    }

    #[tokio::test]
    async fn test_hybrid_keyword_zero_hits_with_llm_unconfigured_returns_failed() {
        // 零命中 + LLM 未配置 api_url → LLM 返回 None → failed
        let turns = vec![make_turn("啊啊啊", "嗯嗯嗯")];
        let config = LlmDetectorConfig::default(); // api_url 为空
        let llm = Arc::new(HttpScenarioDetector::new(config));
        let detector = HybridScenarioDetector::new(Some(llm));
        let result = detector.detect(&turns).await;
        assert!(result.is_failed());
    }

    // ========================================================================
    // resolve_effective_scenario 测试
    // ========================================================================

    use hippocampus_core::storage::LocalStorage;
    use tempfile::TempDir;

    fn make_storage() -> (TempDir, LocalStorage) {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path().to_path_buf());
        (tmp, storage)
    }

    #[tokio::test]
    async fn test_resolve_user_explicit_overrides_everything() {
        // 用户显式 > session_meta > 识别 > 默认
        let (_tmp, storage) = make_storage();
        let detector = HybridScenarioDetector::new(None);
        let family = AgentFamily::ClaudeCode;

        let result = resolve_effective_scenario(
            &storage,
            "sess-1",
            Some("writing"),
            &family,
            &detector,
            &[make_turn("fn compile", "好的")],
        ).await;
        assert_eq!(result, Scenario::Writing);
        // 用户显式不应写入 session_meta
        assert!(storage.read_session_meta("sess-1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_resolve_session_meta_hit_skips_detection() {
        // 已有 session_meta → 直接用，不调 detector
        let (_tmp, storage) = make_storage();
        let meta = SessionMeta {
            scenario: "research".to_string(),
            confidence: 0.85,
            method: "keyword".to_string(),
            detected_at: chrono::Utc::now(),
        };
        storage.write_session_meta("sess-2", &meta).await.unwrap();

        let detector = HybridScenarioDetector::new(None);
        let family = AgentFamily::ClaudeCode;

        let result = resolve_effective_scenario(
            &storage,
            "sess-2",
            None,
            &family,
            &detector,
            &[make_turn("fn compile", "好的")], // 即使对话是 coding，也用 meta 的 research
        ).await;
        assert_eq!(result, Scenario::Research);
    }

    #[tokio::test]
    async fn test_resolve_first_archive_writes_meta() {
        // 首次 archive：识别 + 写 meta
        let (_tmp, storage) = make_storage();
        let detector = HybridScenarioDetector::new(None);
        let family = AgentFamily::ClaudeCode;

        let result = resolve_effective_scenario(
            &storage,
            "sess-3",
            None,
            &family,
            &detector,
            &[make_turn("fn compile refactor 调试", "架构")],
        ).await;
        assert_eq!(result, Scenario::Coding);

        // 验证 meta 已写入
        let meta = storage.read_session_meta("sess-3").await.unwrap().unwrap();
        assert_eq!(meta.scenario, "coding");
        assert_eq!(meta.method, "keyword");
    }

    #[tokio::test]
    async fn test_resolve_detection_failure_falls_back_to_agent_default() {
        // 识别失败（零命中 + 无 LLM）→ Agent 默认场景
        let (_tmp, storage) = make_storage();
        let detector = HybridScenarioDetector::new(None);
        let family = AgentFamily::ClaudeCode; // 默认 Coding

        let result = resolve_effective_scenario(
            &storage,
            "sess-4",
            None,
            &family,
            &detector,
            &[make_turn("啊啊啊", "嗯嗯嗯")],
        ).await;
        assert_eq!(result, Scenario::Coding, "ClaudeCode 默认应降级到 Coding");
    }
```

- [ ] **Step 4: 在 Cargo.toml 中追加 tempfile dev-dependency**

修改 `crates/hippocampus-presets/Cargo.toml`，在文件末尾追加：

```toml
[dev-dependencies]
tempfile = { workspace = true }
```

- [ ] **Step 5: 验证测试通过**

Run: `cargo test -p hippocampus-presets --lib scenario_detect`
Expected: PASS — 全部场景识别测试通过（KeywordScenarioDetector + HttpScenarioDetector + HybridScenarioDetector + resolve_effective_scenario）

- [ ] **Step 6: 提交**

```bash
git add crates/hippocampus-presets/src/scenario_detect.rs crates/hippocampus-presets/Cargo.toml
git commit -m "feat(presets): HybridScenarioDetector + resolve_effective_scenario 编排函数 (v2.33)"
```

---

## Task 9: lib.rs 重导出 API

**Files:**
- Modify: `crates/hippocampus-presets/src/lib.rs`

- [ ] **Step 1: 更新 lib.rs 的导出**

修改 `crates/hippocampus-presets/src/lib.rs`（line 79-87），最终内容：

```rust
pub mod builder;
pub mod combined;
pub mod detect;
pub mod linkage;
pub mod scenario_detect;

pub use builder::{build_from_strings, scenario_from_str, scenario_to_str, PresetBuilder};
pub use combined::{CombinedProfile, TriggerRule, UsageProtocol};
pub use detect::{detect_agent_client, default_scenario_for_agent, resolve_scenario_name, DetectedAgent, DetectionSource};
pub use linkage::derive_window_from_agent;
pub use scenario_detect::{
    DetectionResult, HybridScenarioDetector, HttpScenarioDetector, KeywordScenarioDetector,
    resolve_effective_scenario,
};
```

- [ ] **Step 2: 验证 hippocampus-presets 编译**

Run: `cargo build -p hippocampus-presets`
Expected: 编译通过

- [ ] **Step 3: 提交**

```bash
git add crates/hippocampus-presets/src/lib.rs
git commit -m "feat(presets): lib.rs 重导出场景识别 API (v2.33)"
```

---

## Task 10: MCP build_scenario_detector + 注入 HippocampusMcp

**Files:**
- Modify: `crates/hippocampus-mcp/src/main.rs`（新增 `build_scenario_detector` 函数 + 注入 HippocampusMcp）
- Modify: `crates/hippocampus-mcp/src/lib.rs`（HippocampusMcp 新增 `scenario_detector` 字段 + `with_scenario_detector` 链式方法）

- [ ] **Step 1: 在 hippocampus-mcp/src/lib.rs 中新增 scenario_detector 字段**

修改 `crates/hippocampus-mcp/src/lib.rs` line 60-102 的 HippocampusMcp 结构体定义。在 `summary_generator` 字段后、`combined_profile` 字段前插入 `scenario_detector` 字段：

```rust
// crates/hippocampus-mcp/src/lib.rs line 60
use hippocampus_presets::CombinedProfile;
use hippocampus_presets::scenario_detect::HybridScenarioDetector;
```

修改 struct 定义（line 65-102）：

```rust
#[derive(Clone)]
pub struct HippocampusMcp {
    /// 存储根目录
    storage_root: PathBuf,
    /// 可注入的冲突检测器（v2.11）
    conflict_detector: Option<Arc<dyn ConflictDetector>>,
    /// 可注入的 Session 级语义检索路由器（v2.18）
    session_search: Option<Arc<SessionSearchRouter>>,
    /// 可注入的 LLM 摘要生成器（v2.21 批次 8c）
    summary_generator: Option<Arc<dyn SummaryGenerator>>,
    /// 可注入的场景识别器（v2.33 新增）
    ///
    /// - `Some`：`archive` 工具首次归档时调用，识别场景写入 session_meta
    /// - `None`：仅用 Agent 默认场景（向后兼容，与 v2.32 行为一致）
    scenario_detector: Option<Arc<HybridScenarioDetector>>,
    /// 启动时识别 + 注入的 CombinedProfile（v2.30 新增）
    combined_profile: Option<CombinedProfile>,
    /// 运行时降级状态快照（v2.32 新增）
    runtime_status: RuntimeStatus,
}
```

- [ ] **Step 2: 更新 `new()` 和 `with_conflict_detector()` 初始化 scenario_detector**

修改 `crates/hippocampus-mcp/src/lib.rs` 的 `new()` 方法（line 108-117）和 `with_conflict_detector()` 方法（line 128-140），都加入 `scenario_detector: None,`：

```rust
    pub fn new(storage_root: PathBuf) -> Self {
        Self {
            storage_root,
            conflict_detector: None,
            session_search: None,
            summary_generator: None,
            scenario_detector: None,
            combined_profile: None,
            runtime_status: RuntimeStatus::default(),
        }
    }

    pub fn with_conflict_detector(
        storage_root: PathBuf,
        conflict_detector: Option<Arc<dyn ConflictDetector>>,
    ) -> Self {
        Self {
            storage_root,
            conflict_detector,
            session_search: None,
            summary_generator: None,
            scenario_detector: None,
            combined_profile: None,
            runtime_status: RuntimeStatus::default(),
        }
    }
```

- [ ] **Step 3: 新增 with_scenario_detector 链式方法**

在 `with_summary_generator` 方法后（用 Grep 定位 `pub fn with_summary_generator`，在其闭合 `}` 后）追加：

```rust
    /// 链式注入场景识别器（v2.33 新增 builder 模式）
    ///
    /// 启用后 `archive` 工具首次归档时调用识别器，识别场景写入 session_meta。
    /// 未注入时使用 Agent 默认场景（向后兼容）。
    ///
    /// ## 使用示例
    ///
    /// ```rust,ignore
    /// let detector = Arc::new(HybridScenarioDetector::new(Some(llm)));
    /// let mcp = HippocampusMcp::with_conflict_detector(root, detector_conflict)
    ///     .with_scenario_detector(Some(detector));
    /// ```
    pub fn with_scenario_detector(
        mut self,
        scenario_detector: Option<Arc<HybridScenarioDetector>>,
    ) -> Self {
        self.scenario_detector = scenario_detector;
        self
    }
```

- [ ] **Step 4: 验证 hippocampus-mcp 编译**

Run: `cargo build -p hippocampus-mcp`
Expected: 编译通过（未在 archive handler 中使用 scenario_detector 字段，会有 unused 警告，但不影响编译）

- [ ] **Step 5: 在 main.rs 中新增 build_scenario_detector 函数**

修改 `crates/hippocampus-mcp/src/main.rs`，在 `build_summary_generator` 函数后（约 line 207 附近）追加：

```rust
/// 从环境变量构造场景识别器（v2.33 新增）
///
/// 复用 `HIPPOCAMPUS_DETECTOR_*` 环境变量（与冲突检测器、摘要生成器共享 LLM 配置）：
///
/// - 配置了 `HIPPOCAMPUS_DETECTOR_API_URL` + `API_KEY`：
///   返回 `Some(HybridScenarioDetector)`（关键词 + LLM 兜底）
/// - 未配置：返回 `Some(HybridScenarioDetector)` 仅关键词模式
///
/// ## 返回
///
/// 总是返回 `Some`（关键词模式无外部依赖，作为基线识别器）。
fn build_scenario_detector() -> Arc<hippocampus_presets::HybridScenarioDetector> {
    use hippocampus_llm::LlmDetectorConfig;
    use hippocampus_presets::scenario_detect::{HttpScenarioDetector, HybridScenarioDetector};

    let config = match LlmDetectorConfig::from_env() {
        Some(config) => {
            tracing::info!(
                api_url = %config.api_url,
                model = %config.model,
                "场景识别器：LLM API 已配置，启用关键词 + LLM 兜底"
            );
            Some(Arc::new(HttpScenarioDetector::new(config)))
        }
        None => {
            tracing::info!(
                "场景识别器：未配置 LLM API，仅用关键词规则识别（7 场景 × 15 关键词）"
            );
            None
        }
    };

    Arc::new(HybridScenarioDetector::new(config))
}
```

- [ ] **Step 6: 在 main.rs 顶部导入 HybridScenarioDetector**

修改 `crates/hippocampus-mcp/src/main.rs` line 67-70 的导入：

```rust
use hippocampus_presets::{
    detect_agent_client, resolve_scenario_name, scenario_from_str, CombinedProfile,
    PresetBuilder,
};
```

无需额外导入（`build_scenario_detector` 内部使用全限定路径）。

- [ ] **Step 7: 在 main() 中调用 build_scenario_detector 并注入 HippocampusMcp**

用 Grep 找到 main.rs 中 `HippocampusMcp::with_conflict_detector` 或 `HippocampusMcp::new` 的调用位置（通常在 main 函数中）。在调用链中追加 `.with_scenario_detector(Some(build_scenario_detector()))`：

```rust
// 假设原代码大致如下（根据实际 main() 调整）：
let mcp = HippocampusMcp::with_conflict_detector(storage_root.clone(), conflict_detector)
    .with_session_search(session_search)
    .with_summary_generator(summary_generator)
    .with_scenario_detector(Some(build_scenario_detector()));  // 新增此行
```

若 main.rs 用了 `combined_profile` 字段，可能还需要在 `with_combined_profile` 链中保留。先用 Grep 定位实际调用模式再修改。

- [ ] **Step 8: 验证编译**

Run: `cargo build -p hippocampus-mcp`
Expected: 编译通过

- [ ] **Step 9: 提交**

```bash
git add crates/hippocampus-mcp/src/lib.rs crates/hippocampus-mcp/src/main.rs
git commit -m "feat(mcp): HippocampusMcp 注入 scenario_detector + build_scenario_detector (v2.33)"
```

---

## Task 11: MCP archive handler 调用 resolve_effective_scenario

**Files:**
- Modify: `crates/hippocampus-mcp/src/lib.rs`（archive handler 中插入识别调用，约 line 728-750 附近）

- [ ] **Step 1: 在 archive handler 中插入场景识别调用**

定位到 `crates/hippocampus-mcp/src/lib.rs` 中 archive 方法（line 692）的 `let (archive_threshold, summary_template) = if let Some(preset_req) = &params.preset { ... }` 块（约 line 728-749）。

在该块之后（line 749 的 `};` 后）、`let storage = self.create_storage();`（line 751）前插入场景识别逻辑：

```rust
        // v2.33：场景识别（仅首次 archive 时识别，后续读 session_meta 跳过）
        // 优先级：用户显式 preset.scenario > session_meta > 识别 > Agent 默认
        // 识别失败不阻塞 archive，降级到 Agent 默认场景
        let effective_scenario_name: Option<String> = if let Some(detector) = &self.scenario_detector {
            // 推导 Agent family（用于降级 fallback）
            let family = self.combined_profile
                .as_ref()
                .and_then(|cp| cp.agent.as_ref())
                .map(|a| a.family.clone())
                .unwrap_or(hippocampus_agents::AgentFamily::Custom("unknown".to_string()));

            // 提取 preset.scenario 作为用户显式（若存在）
            let user_explicit = params.preset.as_ref()
                .and_then(|p| p.scenario.as_deref());

            let storage_for_detect = self.create_storage();
            let scenario = hippocampus_presets::resolve_effective_scenario(
                storage_for_detect.as_ref(),
                &params.session_id,
                user_explicit,
                &family,
                detector.as_ref(),
                &turns,
            ).await;

            Some(hippocampus_presets::scenario_to_str(&scenario))
        } else {
            // 未注入识别器：保留原行为（preset.scenario 或 None）
            params.preset.as_ref().and_then(|p| p.scenario.clone())
        };

        // v2.33：若识别到场景（且 preset 未显式指定），用识别的场景重新 build CombinedProfile
        // 以应用对应场景的 summary_template / archive_threshold / priority_tags 等
        let (archive_threshold, summary_template) = if let Some(preset_req) = &params.preset {
            // 用户传了 preset，按 preset build（识别结果仅作记录已写入 session_meta）
            let combined = hippocampus_presets::build_from_strings(
                preset_req.agent.as_deref(),
                preset_req.scenario.as_deref(),
                preset_req.model.as_deref(),
                preset_req.archive_threshold,
                preset_req.summary_template.as_deref(),
            )
            .map_err(|e| McpError::invalid_params(
                format!("预设构建失败: {e}"), None,
            ))?;
            (Some(combined.archive_threshold()), Some(combined.summary_template().to_string()))
        } else if let Some(scenario_name) = effective_scenario_name {
            // 无 preset 但识别到场景 → 用识别的场景 build
            let combined = hippocampus_presets::build_from_strings(
                None,
                Some(&scenario_name),
                None,
                None,
                None,
            ).map_err(|e| McpError::internal_error(
                format!("识别场景构建预设失败: {e}"), None,
            ))?;
            (Some(combined.archive_threshold()), Some(combined.summary_template().to_string()))
        } else {
            (None, None)
        };
```

**注意**：此段替换了原来的 `let (archive_threshold, summary_template) = if let Some(preset_req) = &params.preset { ... } else { (None, None) };` 块。

- [ ] **Step 2: 在 lib.rs 顶部导入 AgentFamily**

修改 `crates/hippocampus-mcp/src/lib.rs` 顶部导入（如有需要）：

```rust
// 若尚未导入 hippocampus_agents::AgentFamily，添加：
use hippocampus_agents::AgentFamily;
```

实际上 archive handler 中用 `hippocampus_agents::AgentFamily::Custom(...)` 全限定路径，可不导入。

- [ ] **Step 3: 验证编译**

Run: `cargo build -p hippocampus-mcp`
Expected: 编译通过

- [ ] **Step 4: 写集成测试（可选，验证 archive handler 调用识别）**

新建 `crates/hippocampus-mcp/tests/scenario_detect_integration.rs`：

```rust
//! 场景识别集成测试（v2.33）
//!
//! 验证 archive handler 调用 resolve_effective_scenario 的完整流程：
//! 1. 首次 archive 触发识别
//! 2. session_meta 写入
//! 3. 后续 archive 跳过识别（读 meta）

use hippocampus_core::storage::{LocalStorage, SessionMeta, Storage};
use hippocampus_mcp::HippocampusMcp;
use hippocampus_presets::scenario_detect::HybridScenarioDetector;
use std::sync::Arc;
use tempfile::TempDir;

#[tokio::test]
async fn test_first_archive_writes_session_meta() {
    let tmp = TempDir::new().unwrap();
    let storage_root = tmp.path().to_path_buf();

    // 直接通过 LocalStorage 验证元数据写入（不调用完整 archive，避免 MCP 工具调用复杂度）
    let storage = LocalStorage::new(storage_root.clone());
    let detector = HybridScenarioDetector::new(None);

    // 模拟 coding 对话
    let turns = vec![
        make_turn("帮我写 Rust 函数", "好的，fn 主体如下"),
        make_turn("编译报错了", "调试一下架构"),
    ];

    let family = hippocampus_agents::AgentFamily::ClaudeCode;
    let scenario = hippocampus_presets::resolve_effective_scenario(
        &storage,
        "integration-sess-1",
        None,
        &family,
        &detector,
        &turns,
    ).await;

    assert_eq!(scenario, hippocampus_scenarios::Scenario::Coding);

    // 验证 meta 已写入
    let meta = storage.read_session_meta("integration-sess-1").await.unwrap().unwrap();
    assert_eq!(meta.scenario, "coding");
    assert_eq!(meta.method, "keyword");
}

fn make_turn(user: &str, llm: &str) -> hippocampus_core::model::MessageTurn {
    use hippocampus_core::model::{MessageContent, MessageTurn};
    use chrono::Utc;
    use uuid::Uuid;
    MessageTurn {
        id: Uuid::new_v4(),
        user_message: MessageContent {
            text: Some(user.to_string()),
            attachments: vec![],
            tool_calls: vec![],
            thinking: None,
        },
        llm_message: MessageContent {
            text: Some(llm.to_string()),
            attachments: vec![],
            tool_calls: vec![],
            thinking: None,
        },
        tags: vec![],
        timestamp: Utc::now(),
        token_count: 100,
    }
}
```

- [ ] **Step 5: 验证测试通过**

Run: `cargo test -p hippocampus-mcp --test scenario_detect_integration`
Expected: PASS — 集成测试通过

- [ ] **Step 6: 完整工作区测试 + clippy**

Run: `cargo test --workspace`
Expected: 全部测试通过（无回归）

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: 无 warning（如有 unused 字段等，按提示修复）

- [ ] **Step 7: 提交**

```bash
git add crates/hippocampus-mcp/src/lib.rs crates/hippocampus-mcp/tests/scenario_detect_integration.rs
git commit -m "feat(mcp): archive handler 调用 resolve_effective_scenario + 集成测试 (v2.33)"
```

---

## Task 12: server 端 archive handler 同步（如适用）

**Files:**
- Modify: `crates/hippocampus-server/src/handlers.rs`（如 server 端有 archive handler）

**说明**：spec 提到 server 端 archive handler 也需同步调用 `resolve_effective_scenario`。如果 server 端的 archive 走的是与 MCP 完全独立的路径，需同样注入 HybridScenarioDetector；如果 server 端复用 hippocampus-mcp 的逻辑（如通过 HippocampusMcp 内部调用），则本任务可跳过。

- [ ] **Step 1: 检查 server 端是否有独立 archive handler**

Run: `cargo build -p hippocampus-server`（确认 server crate 存在）

用 Grep 搜索 `crates/hippocampus-server/src` 中的 `archive` handler：

```
pattern: "async fn archive|build_from_strings"
path: crates/hippocampus-server/src
```

- [ ] **Step 2: 若有独立 archive handler，按 Task 11 同样模式注入**

参考 Task 11 的 archive handler 修改模式，在 server 端 archive handler 中插入同样的识别逻辑。

- [ ] **Step 3: 若 server 端复用 MCP 逻辑，跳过本任务并标记完成**

如无独立 archive handler，本任务直接标记完成。

- [ ] **Step 4: 提交（如有改动）**

```bash
git add crates/hippocampus-server/
git commit -m "feat(server): archive handler 同步调用 resolve_effective_scenario (v2.33)"
```

---

## 完工验收

- [ ] **Step 1: 完整工作区编译 + 测试**

```bash
cargo build --workspace
cargo test --workspace
```

Expected: 全部通过

- [ ] **Step 2: clippy 无 warning**

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

- [ ] **Step 3: 手动验证（可选）**

启动 MCP server（配置 `HIPPOCAMPUS_DETECTOR_API_URL` 和 `HIPPOCAMPUS_DETECTOR_API_KEY`），通过 LLM 调用 archive 工具，传入一段写作类对话（如"帮我写一篇文章"），观察日志：
- 首次 archive：应看到 `场景识别完成 scenario=Writing method=keyword`
- 再次 archive 同一 session：应看到 `场景识别：命中 session 元数据`

- [ ] **Step 4: 更新 project_memory.md**

更新 `c:\Users\LINGTIAN303\.trae-cn\memory\projects\-d----AI-Hippocampus\project_memory.md` 的 `task_state` 章节：

```markdown
## 当前任务
- v2.33 场景识别功能已完成
- 包括：SessionMeta + Storage trait 扩展 + KeywordScenarioDetector + HttpScenarioDetector + HybridScenarioDetector + resolve_effective_scenario + MCP 注入
```

- [ ] **Step 5: 推送部署**

```bash
git push production main
```

push 会触发 post-receive hook → 自动构建 + 部署。

---

## 风险与回滚

| 风险 | 缓解 |
|------|------|
| Storage trait 扩展破坏第三方实现 | 默认实现返回 Ok(())/Ok(None)，向后兼容 |
| LLM 调用增加 archive 延迟 | 仅首次调用，后续命中 session_meta 跳过 |
| 关键词规则误识别 | 置信度阈值 + LLM 兜底 |
| 识别失败阻塞 archive | 永不阻塞，降级到 Agent 默认场景 |
| session_meta 文件并发写 | archive 串行化，无并发风险 |

**回滚**：若 v2.33 引入严重问题，回滚 commit 即可。session_meta 文件不影响旧版本运行（旧版本 Storage trait 没有此方法，忽略文件即可）。
