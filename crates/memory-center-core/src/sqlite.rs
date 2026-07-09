//! # SQLite 存储后端
//!
//! 基于 rusqlite + r2d2 连接池的 SQLite 存储后端，支持 WAL 模式并发读写。
//!
//! ## 设计
//!
//! - [`SqliteStorage`]：一个实例对应一个 project 的数据库
//! - 数据库文件路径：`{root}/projects/{project_id}/memories.db`
//! - 无 `project_id` 时使用 `_default.db`
//! - 内部用 [`r2d2`] 连接池管理 [`rusqlite`] 连接（WAL 模式支持并发读）
//! - 所有 async 方法通过 [`tokio::task::spawn_blocking`] 包装同步 rusqlite 调用
//!
//! ## WAL 模式
//!
//! 启动时执行：
//! ```sql
//! PRAGMA journal_mode = WAL;       -- WAL 模式，读写不互斥
//! PRAGMA synchronous = NORMAL;     -- WAL 下安全且更快
//! PRAGMA busy_timeout = 5000;      -- 5 秒等待锁
//! PRAGMA foreign_keys = ON;        -- 启用外键约束
//! ```
//!
//! ## 表结构
//!
//! - `memories`：记忆文件（按 UUID 主键，content 字段存序列化后的完整 MemoryFile）
//! - `hooks`：索引钩子（按 UUID 主键，scope 区分 session/project）
//!
//! ## 与 LocalStorage 共存
//!
//! v2.4 设计为「混合共存 + 手动迁移」：
//! - 旧数据：LocalStorage 文件树
//! - 新数据：SqliteStorage 数据库
//! - 迁移：通过 `migrator` 模块的迁移工具（批次 4 实现）

use crate::model::{ArchivePeriod, IndexDocument, IndexHook, MemoryFile, MemoryUpdate};
use crate::serialization::SerializationFormat;
use crate::storage::{SessionMeta, Storage};
use chrono::{DateTime, Utc};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use std::path::PathBuf;
use uuid::Uuid;

/// SQLite 存储后端
///
/// 一个实例对应一个 project 的数据库。多个 project 共用一个 root 目录时，
/// 每个 project 有独立的数据库文件。
///
/// ## 并发
///
/// 内部用 r2d2 连接池，WAL 模式下：
/// - 读操作可并发（多个连接同时读）
/// - 写操作串行化（SQLite 数据库级写锁）
/// - busy_timeout=5000ms 避免短时锁冲突
///
/// ## 跨进程安全
///
/// SQLite WAL 模式支持多进程并发访问同一数据库文件，但写仍是单进程独占。
pub struct SqliteStorage {
    /// r2d2 连接池
    pool: Pool<SqliteConnectionManager>,
    /// 序列化格式（决定 content 字段的存储格式）
    format: SerializationFormat,
    /// 绑定的 project_id（可为空，对应 _default.db）
    project_id: Option<String>,
}

impl std::fmt::Debug for SqliteStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteStorage")
            .field("format", &self.format)
            .field("project_id", &self.project_id)
            .field("pool_state", &self.pool.state())
            .finish()
    }
}

/// SQL 初始化语句
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
    session_id   TEXT PRIMARY KEY,
    scenario     TEXT NOT NULL,
    confidence   REAL NOT NULL,
    method       TEXT NOT NULL,
    detected_at  TEXT NOT NULL,
    agent_family TEXT DEFAULT '',  -- v2.40 新增：产生此 session 的 Agent family
    hook_mode    TEXT DEFAULT ''   -- v2.40 新增：钩子模式 real/pseudo
);

-- v2.40 迁移：为旧库补列（ALTER TABLE 幂等失败，用 try 忽略已存在）
-- 注意：schema 初始化每次启动都会执行，CREATE TABLE 已包含新列，
-- 此处 ALTER TABLE 仅用于升级旧库（列不存在时添加）。

-- v2.34: raw_contexts 表（pre_compress_hook 持久化完整原始上下文）
CREATE TABLE IF NOT EXISTS raw_contexts (
    session_id  TEXT NOT NULL,
    hook_id     TEXT NOT NULL,
    content     TEXT NOT NULL,
    stored_at   TEXT NOT NULL,
    PRIMARY KEY (session_id, hook_id)
);
"#;

/// v2.34 schema 迁移：memories 表新增 archive_reason / raw_context_path 列
///
/// SQLite 的 `ALTER TABLE ADD COLUMN` 不支持 `IF NOT EXISTS`，需用
/// `pragma_table_info` 检查列是否已存在后再 ALTER，确保幂等。
///
/// 在每个新连接创建时调用（with_init），幂等安全。
fn run_v2_34_migrations(conn: &mut rusqlite::Connection) -> rusqlite::Result<()> {
    // 检查 memories 表的所有列名
    let mut stmt = conn.prepare("SELECT name FROM pragma_table_info('memories')")?;
    let existing_cols: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt); // 释放 stmt 借用，允许后续 ALTER

    // 若 archive_reason 列不存在则添加
    if !existing_cols.contains(&"archive_reason".to_string()) {
        conn.execute("ALTER TABLE memories ADD COLUMN archive_reason TEXT", [])?;
    }

    // 若 raw_context_path 列不存在则添加
    if !existing_cols.contains(&"raw_context_path".to_string()) {
        conn.execute("ALTER TABLE memories ADD COLUMN raw_context_path TEXT", [])?;
    }

    Ok(())
}

impl SqliteStorage {
    /// 创建新的 SQLite 存储后端（默认 JSON 格式）
    ///
    /// - `root`：根目录（数据库文件存放位置）
    /// - `project_id`：项目 ID（决定数据库文件路径，None 时用 `_default`）
    ///
    /// 数据库路径：`{root}/projects/{project_id or "_default"}/memories.db`
    pub fn new(root: impl Into<PathBuf>, project_id: Option<String>) -> crate::Result<Self> {
        Self::with_format(root, project_id, SerializationFormat::Json)
    }

    /// 创建新的 SQLite 存储后端（指定序列化格式）
    pub fn with_format(
        root: impl Into<PathBuf>,
        project_id: Option<String>,
        format: SerializationFormat,
    ) -> crate::Result<Self> {
        let root = root.into();
        let project_dir = match &project_id {
            Some(pid) => root.join("projects").join(pid),
            None => root.join("projects").join("_default"),
        };

        // 创建目录（同步阻塞，仅初始化时执行一次）
        std::fs::create_dir_all(&project_dir).map_err(|e| {
            crate::Error::Storage(format!("创建项目目录失败 {:?}: {}", project_dir, e))
        })?;

        let db_path = project_dir.join("memories.db");
        let manager = SqliteConnectionManager::file(&db_path).with_init(|c| {
            c.execute_batch(INIT_SQL)?;
            run_v2_34_migrations(c)
        });

        let pool = Pool::builder()
            .max_size(8) // 默认 8 个连接（WAL 下读可并发）
            .build(manager)
            .map_err(|e| {
                crate::Error::Storage(format!("创建连接池失败 {:?}: {}", db_path, e))
            })?;

        Ok(Self {
            pool,
            format,
            project_id,
        })
    }

    /// 序列化格式
    pub fn format(&self) -> SerializationFormat {
        self.format
    }

    /// 绑定的 project_id
    pub fn project_id(&self) -> Option<&str> {
        self.project_id.as_deref()
    }

    /// 序列化 MemoryFile → BLOB
    fn serialize_memory(&self, file: &MemoryFile) -> crate::Result<Vec<u8>> {
        self.format.serialize_memory(file)
    }

    /// 序列化 Vec<Tag> → JSON 字符串
    fn serialize_tags(tags: &[crate::model::Tag]) -> crate::Result<String> {
        serde_json::to_string(tags)
            .map_err(|e| crate::Error::Serialize(format!("序列化 tags 失败: {}", e)))
    }

    /// 从数据库行构造 IndexHook
    fn hook_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<IndexHook> {
        let hook_id: String = row.get(0)?;
        let memory_id: String = row.get(1)?;
        let summary_title: String = row.get(5)?;
        let tags_json: String = row.get(6)?;
        let archived_at: String = row.get(7)?;
        let token_count: i64 = row.get(8)?;

        let tags: Vec<crate::model::Tag> =
            serde_json::from_str(&tags_json).unwrap_or_default();
        let archived_at = DateTime::parse_from_rfc3339(&archived_at)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        let period_str: String = row.get(4)?;
        let period = ArchivePeriod::from_str(&period_str).unwrap_or(ArchivePeriod::Daily);

        Ok(IndexHook {
            id: Uuid::parse_str(&hook_id).unwrap_or_else(|_| Uuid::new_v4()),
            memory_id,
            summary: crate::model::Summary {
                title: summary_title,
                abstract_text: None,
                key_facts: Vec::new(),
                key_entities: Vec::new(),
                clue_anchors: Vec::new(),
            },
            tags,
            archived_at,
            period,
            token_count: token_count as usize,
            file_status: crate::model::FileStatus::Normal,
            // v2.34：SQLite schema 尚未包含这两列，读取时默认 None
            // 后续 Task 会扩展 schema 并从对应列读取
            archive_reason: None,
            raw_context_path: None,
        })
    }

    /// 在 spawn_blocking 中执行数据库操作
    ///
    /// 由于 rusqlite 是同步的，用 spawn_blocking 包装避免阻塞 tokio runtime。
    /// `F` 闭包接收连接，返回 `crate::Result<T>`。
    async fn with_conn<F, T>(&self, f: F) -> crate::Result<T>
    where
        F: FnOnce(&mut rusqlite::Connection) -> crate::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let pool = self.pool.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = pool.get().map_err(|e| {
                crate::Error::Storage(format!("获取数据库连接失败: {}", e))
            })?;
            f(&mut conn)
        })
        .await
        .map_err(|e| crate::Error::Storage(format!("数据库任务执行失败: {}", e)))?
    }
}

#[async_trait::async_trait]
impl Storage for SqliteStorage {
    async fn write_memory(&self, file: &MemoryFile) -> crate::Result<String> {
        let content = self.serialize_memory(file)?;
        let memory_id = file.id.to_string();
        let session_id = file.session_id.clone();
        let project_id = file.project_id.clone();
        let period = file.period.as_str().to_string();
        let archived_at = file.archived_at.to_rfc3339();

        // 克隆 content 用于 spawn_blocking
        let content = content.clone();
        let memory_id_clone = memory_id.clone();

        self.with_conn(move |conn| {
            conn.execute(
                "INSERT OR REPLACE INTO memories (memory_id, session_id, project_id, period, archived_at, content)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    &memory_id_clone,
                    &session_id,
                    project_id.as_deref(),
                    &period,
                    &archived_at,
                    &content,
                ],
            )
            .map_err(|e| {
                crate::Error::Storage(format!("写入 memories 失败: {}", e))
            })?;
            Ok(())
        })
        .await?;

        // 返回 memory_id（UUID 字符串，与 LocalStorage 的路径不同）
        Ok(memory_id)
    }

    async fn read_memory(&self, memory_id: &str) -> crate::Result<MemoryFile> {
        let memory_id = memory_id.to_string();
        let format = self.format;

        self.with_conn(move |conn| {
            let content: Vec<u8> = conn
                .query_row(
                    "SELECT content FROM memories WHERE memory_id = ?1",
                    rusqlite::params![&memory_id],
                    |row| row.get(0),
                )
                .map_err(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => {
                        crate::Error::Storage(format!("记忆文件不存在: {}", memory_id))
                    }
                    other => crate::Error::Storage(format!("查询 memories 失败: {}", other)),
                })?;
            format.deserialize_memory(&content)
        })
        .await
    }

    async fn delete_memory(&self, memory_id: &str) -> crate::Result<()> {
        let memory_id = memory_id.to_string();

        self.with_conn(move |conn| {
            let affected = conn
                .execute(
                    "DELETE FROM memories WHERE memory_id = ?1",
                    rusqlite::params![&memory_id],
                )
                .map_err(|e| crate::Error::Storage(format!("删除 memories 失败: {}", e)))?;
            if affected == 0 {
                return Err(crate::Error::Storage(format!(
                    "记忆文件不存在: {}",
                    memory_id
                )));
            }
            Ok(())
        })
        .await
    }

    async fn write_index(&self, doc: &IndexDocument) -> crate::Result<String> {
        let session_id = doc.session_id.clone();
        let project_id = doc.project_id.clone();
        let period = doc.period.as_str().to_string();

        // 收集所有 hooks 的数据
        let hooks_data: Vec<(String, String, String, String, String, String, String, i64)> = doc
            .hooks
            .iter()
            .map(|h| {
                Ok::<_, crate::Error>((
                    h.id.to_string(),
                    h.memory_id.clone(),
                    session_id.clone(),
                    h.summary.title.clone(),
                    Self::serialize_tags(&h.tags)?,
                    h.archived_at.to_rfc3339(),
                    h.period.as_str().to_string(),
                    h.token_count as i64,
                ))
            })
            .collect::<crate::Result<Vec<_>>>()?;

        let doc_id = doc.id.to_string();

        self.with_conn(move |conn| {
            // 事务：先删除旧 hooks，再插入新 hooks
            let tx = conn.transaction().map_err(|e| {
                crate::Error::Storage(format!("开启事务失败: {}", e))
            })?;

            // 删除旧 hooks（同 session + period + scope=session）
            tx.execute(
                "DELETE FROM hooks WHERE session_id = ?1 AND period = ?2 AND scope = 'session'",
                rusqlite::params![&session_id, &period],
            )
            .map_err(|e| crate::Error::Storage(format!("删除旧 hooks 失败: {}", e)))?;

            // 插入新 hooks
            for (hook_id, memory_id, sess, summary_title, tags_json, archived_at, period_str, token_count) in &hooks_data {
                tx.execute(
                    "INSERT OR REPLACE INTO hooks (hook_id, memory_id, session_id, project_id, period, summary_title, tags, archived_at, token_count, scope)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'session')",
                    rusqlite::params![
                        hook_id,
                        memory_id,
                        sess,
                        project_id.as_deref(),
                        period_str,
                        summary_title,
                        tags_json,
                        archived_at,
                        token_count,
                    ],
                )
                .map_err(|e| crate::Error::Storage(format!("插入 hooks 失败: {}", e)))?;
            }

            tx.commit().map_err(|e| {
                crate::Error::Storage(format!("提交事务失败: {}", e))
            })?;
            Ok(())
        })
        .await?;

        Ok(doc_id)
    }

    async fn read_index(
        &self,
        session_id: &str,
        _project_id: Option<&str>,
        period: ArchivePeriod,
    ) -> crate::Result<Option<IndexDocument>> {
        let session_id = session_id.to_string();
        let period_str = period.as_str().to_string();
        let project_id = self.project_id.clone();

        self.with_conn(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT hook_id, memory_id, session_id, project_id, period, summary_title, tags, archived_at, token_count
                     FROM hooks
                     WHERE session_id = ?1 AND period = ?2 AND scope = 'session'
                     ORDER BY archived_at ASC",
                )
                .map_err(|e| crate::Error::Storage(format!("准备查询失败: {}", e)))?;

            let hooks: Vec<IndexHook> = stmt
                .query_map(
                    rusqlite::params![&session_id, &period_str],
                    Self::hook_from_row,
                )
                .map_err(|e| crate::Error::Storage(format!("查询 hooks 失败: {}", e)))?
                .filter_map(|r| r.ok())
                .collect();

            if hooks.is_empty() {
                return Ok(None);
            }

            Ok(Some(IndexDocument {
                id: Uuid::new_v4(),
                schema_version: 1,
                session_id,
                project_id,
                hooks,
                updated_at: Utc::now(),
                period,
            }))
        })
        .await
    }

    /// 删除索引文档（v2.16 IMP-02：SqliteStorage 实现）
    ///
    /// 删除 hooks 表中同 session + period + scope=session 的所有行。
    /// 行不存在视为已删除，返回 Ok(())。
    async fn delete_index(
        &self,
        session_id: &str,
        _project_id: Option<&str>,
        period: ArchivePeriod,
    ) -> crate::Result<()> {
        let session_id = session_id.to_string();
        let period_str = period.as_str().to_string();

        self.with_conn(move |conn| {
            conn.execute(
                "DELETE FROM hooks WHERE session_id = ?1 AND period = ?2 AND scope = 'session'",
                rusqlite::params![&session_id, &period_str],
            )
            .map_err(|e| crate::Error::Storage(format!("删除 hooks 失败: {}", e)))?;
            Ok(())
        })
        .await
    }

    async fn append_hook(
        &self,
        session_id: &str,
        project_id: Option<&str>,
        period: ArchivePeriod,
        hook: IndexHook,
    ) -> crate::Result<()> {
        let hook_id = hook.id.to_string();
        let memory_id = hook.memory_id.clone();
        let session_id = session_id.to_string();
        let project_id = project_id.map(|s| s.to_string());
        let period_str = period.as_str().to_string();
        let summary_title = hook.summary.title.clone();
        let tags_json = Self::serialize_tags(&hook.tags)?;
        let archived_at = hook.archived_at.to_rfc3339();
        let token_count = hook.token_count as i64;
        // 用 hook 自带的 period（与参数 period 可能不同？统一用参数 period）
        let _ = period;

        self.with_conn(move |conn| {
            conn.execute(
                "INSERT OR REPLACE INTO hooks (hook_id, memory_id, session_id, project_id, period, summary_title, tags, archived_at, token_count, scope)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'session')",
                rusqlite::params![
                    &hook_id,
                    &memory_id,
                    &session_id,
                    project_id.as_deref(),
                    &period_str,
                    &summary_title,
                    &tags_json,
                    &archived_at,
                    token_count,
                ],
            )
            .map_err(|e| crate::Error::Storage(format!("插入 hook 失败: {}", e)))?;
            Ok(())
        })
        .await
    }

    async fn list_memories(
        &self,
        session_id: &str,
        _project_id: Option<&str>,
        period: ArchivePeriod,
    ) -> crate::Result<Vec<String>> {
        let session_id = session_id.to_string();
        let period_str = period.as_str().to_string();

        self.with_conn(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT memory_id FROM memories
                     WHERE session_id = ?1 AND period = ?2
                     ORDER BY archived_at ASC",
                )
                .map_err(|e| crate::Error::Storage(format!("准备查询失败: {}", e)))?;

            let ids: Vec<String> = stmt
                .query_map(rusqlite::params![&session_id, &period_str], |row| {
                    row.get(0)
                })
                .map_err(|e| crate::Error::Storage(format!("查询 memories 失败: {}", e)))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(ids)
        })
        .await
    }

    // ========================================================================
    // project 层聚合索引
    // ========================================================================

    async fn read_project_index(
        &self,
        project_id: &str,
        period: ArchivePeriod,
    ) -> crate::Result<Option<IndexDocument>> {
        let project_id = project_id.to_string();
        let period_str = period.as_str().to_string();

        self.with_conn(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT hook_id, memory_id, session_id, project_id, period, summary_title, tags, archived_at, token_count
                     FROM hooks
                     WHERE project_id = ?1 AND period = ?2 AND scope = 'project'
                     ORDER BY archived_at ASC",
                )
                .map_err(|e| crate::Error::Storage(format!("准备查询失败: {}", e)))?;

            let hooks: Vec<IndexHook> = stmt
                .query_map(
                    rusqlite::params![&project_id, &period_str],
                    Self::hook_from_row,
                )
                .map_err(|e| crate::Error::Storage(format!("查询 project hooks 失败: {}", e)))?
                .filter_map(|r| r.ok())
                .collect();

            if hooks.is_empty() {
                return Ok(None);
            }

            // 取第一个 hook 的 session_id 作为文档 session_id（聚合文档）
            let sess = hooks
                .first()
                .map(|h| h.memory_id.clone())
                .unwrap_or_default();

            Ok(Some(IndexDocument {
                id: Uuid::new_v4(),
                schema_version: 1,
                session_id: sess, // project 级聚合文档，session_id 仅作占位
                project_id: Some(project_id),
                hooks,
                updated_at: Utc::now(),
                period,
            }))
        })
        .await
    }

    async fn append_project_hook(
        &self,
        project_id: &str,
        period: ArchivePeriod,
        hook: IndexHook,
    ) -> crate::Result<()> {
        let hook_id = hook.id.to_string();
        let memory_id = hook.memory_id.clone();
        let project_id = project_id.to_string();
        let period_str = period.as_str().to_string();
        let summary_title = hook.summary.title.clone();
        let tags_json = Self::serialize_tags(&hook.tags)?;
        let archived_at = hook.archived_at.to_rfc3339();
        let token_count = hook.token_count as i64;
        // project 级 hook 的 session_id 字段：IndexHook 没有 session_id 字段，
        // 用 memory_id 作为占位（project 级查询不依赖 session_id 字段）
        let session_id_placeholder = memory_id.clone();

        self.with_conn(move |conn| {
            conn.execute(
                "INSERT OR REPLACE INTO hooks (hook_id, memory_id, session_id, project_id, period, summary_title, tags, archived_at, token_count, scope)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'project')",
                rusqlite::params![
                    &hook_id,
                    &memory_id,
                    &session_id_placeholder,
                    &project_id,
                    &period_str,
                    &summary_title,
                    &tags_json,
                    &archived_at,
                    token_count,
                ],
            )
            .map_err(|e| crate::Error::Storage(format!("插入 project hook 失败: {}", e)))?;
            Ok(())
        })
        .await
    }

    async fn list_project_memories(
        &self,
        project_id: &str,
        period: ArchivePeriod,
    ) -> crate::Result<Vec<String>> {
        let project_id = project_id.to_string();
        let period_str = period.as_str().to_string();

        self.with_conn(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT memory_id FROM memories
                     WHERE project_id = ?1 AND period = ?2
                     ORDER BY archived_at ASC",
                )
                .map_err(|e| crate::Error::Storage(format!("准备查询失败: {}", e)))?;

            let ids: Vec<String> = stmt
                .query_map(rusqlite::params![&project_id, &period_str], |row| {
                    row.get(0)
                })
                .map_err(|e| crate::Error::Storage(format!("查询 project memories 失败: {}", e)))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(ids)
        })
        .await
    }

    // ========================================================================
    // 记忆迭代更新
    // ========================================================================

    async fn update_memory(
        &self,
        memory_id: &str,
        updates: MemoryUpdate,
    ) -> crate::Result<()> {
        // 委托给 update_memory_with_conflicts（传空 conflicts，向后兼容）
        self.update_memory_with_conflicts(memory_id, updates, vec![]).await
    }

    async fn update_memory_with_conflicts(
        &self,
        memory_id: &str,
        updates: MemoryUpdate,
        conflicts: Vec<crate::conflict::ConflictRecord>,
    ) -> crate::Result<()> {
        // 空更新直接返回（幂等）
        if updates.is_empty() {
            return Ok(());
        }

        let memory_id = memory_id.to_string();
        let format = self.format;
        let added = updates.added_facts.len();
        let revised = updates.revised_facts.len();
        let deprecated = updates.deprecated_facts.len();
        let update_record = crate::model::MemoryUpdateRecord {
            updated_at: chrono::Utc::now(),
            update: updates,
            conflicts,
        };

        // v2.4 并发修复：用 BEGIN IMMEDIATE 事务包装「读取-修改-写入」全流程
        //
        // 原实现：read_memory（连接A）→ 修改内存 → with_conn UPDATE（连接B）
        // 问题：两次连接操作之间无事务保护，并发时多个任务读到相同旧状态，
        //       后写入覆盖先写入，导致 updates 丢失。
        //
        // 修复：在同一个 with_conn 闭包内用 BEGIN IMMEDIATE 立即获取写锁，
        //       确保并发更新串行化。先读取 → 反序列化 → 追加 → 序列化 → 写回 → 提交，
        //       全程持有写锁，其他写事务会等待 busy_timeout（5s）。
        self.with_conn(move |conn| {
            // 1. 立即获取写锁（BEGIN IMMEDIATE，区别于 DEFERRED 的延迟加锁）
            conn.execute_batch("BEGIN IMMEDIATE")
                .map_err(|e| crate::Error::Storage(format!("BEGIN IMMEDIATE 失败: {}", e)))?;

            // 2. 在事务内读取现有 content（使用同一个连接，确保看到最新已提交数据）
            let content: Vec<u8> = match conn.query_row(
                "SELECT content FROM memories WHERE memory_id = ?1",
                rusqlite::params![&memory_id],
                |row| row.get(0),
            ) {
                Ok(c) => c,
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    return Err(crate::Error::Storage(format!("记忆文件不存在: {}", memory_id)));
                }
                Err(other) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    return Err(crate::Error::Storage(format!("查询 memories 失败: {}", other)));
                }
            };

            // 3. 反序列化
            let mut file = match format.deserialize_memory(&content) {
                Ok(f) => f,
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    return Err(e);
                }
            };

            // 4. 追加更新记录（v2.4 风险点修复：独立 updates 字段，不污染原始上下文）
            let total_updates = file.updates.len() + 1;
            file.updates.push(update_record);

            // 5. 重新序列化
            let new_content = match format.serialize_memory(&file) {
                Ok(c) => c,
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    return Err(e);
                }
            };

            // 6. 写回
            if let Err(e) = conn.execute(
                "UPDATE memories SET content = ?1 WHERE memory_id = ?2",
                rusqlite::params![&new_content, &memory_id],
            ) {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(crate::Error::Storage(format!("更新 memories 失败: {}", e)));
            }

            // 7. 提交事务（释放写锁）
            if let Err(e) = conn.execute_batch("COMMIT") {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(crate::Error::Storage(format!("COMMIT 失败: {}", e)));
            }

            tracing::info!(
                memory_id = %memory_id,
                added = added,
                revised = revised,
                deprecated = deprecated,
                total_updates = total_updates,
                "SQLite 记忆迭代更新完成（BEGIN IMMEDIATE 事务保护）"
            );

            Ok(())
        })
        .await
    }

    // ========================================================================
    // 批量操作（v2.5 批次 6：单连接优化，减少连接池获取次数）
    // ========================================================================

    /// 批量读取记忆文件（单连接优化）
    ///
    /// 在一个 `with_conn` 闭包内循环 SELECT，只获取 1 次连接池连接。
    /// 单个失败不影响其他条目（逐个返回 Result）。
    async fn read_memories_batch(
        &self,
        memory_ids: &[String],
    ) -> Vec<crate::Result<MemoryFile>> {
        let format = self.format;
        let ids: Vec<String> = memory_ids.to_vec();
        let count = ids.len();
        self.with_conn(move |conn| {
            let mut results = Vec::with_capacity(ids.len());
            for id in &ids {
                let r = (|| -> crate::Result<MemoryFile> {
                    let content: Vec<u8> = conn.query_row(
                        "SELECT content FROM memories WHERE memory_id = ?1",
                        rusqlite::params![id],
                        |row| row.get(0),
                    ).map_err(|e| match e {
                        rusqlite::Error::QueryReturnedNoRows => {
                            crate::Error::Storage(format!("记忆文件不存在: {}", id))
                        }
                        other => crate::Error::Storage(format!("查询失败: {}", other)),
                    })?;
                    format.deserialize_memory(&content)
                })();
                results.push(r);
            }
            Ok(results)
        })
        .await
        .unwrap_or_else(|e| (0..count).map(|_| Err(e.clone())).collect())
    }

    /// 批量删除记忆文件（单连接优化）
    ///
    /// 每条独立事务（BEGIN IMMEDIATE），单个失败不影响其他。
    /// 检查 affected_rows，不存在的 ID 返回错误（与 LocalStorage 行为一致）。
    async fn delete_memories_batch(
        &self,
        memory_ids: &[String],
    ) -> Vec<crate::Result<()>> {
        let ids: Vec<String> = memory_ids.to_vec();
        let count = ids.len();
        self.with_conn(move |conn| {
            let mut results = Vec::with_capacity(ids.len());
            for id in &ids {
                let r = (|| -> crate::Result<()> {
                    conn.execute_batch("BEGIN IMMEDIATE")
                        .map_err(|e| crate::Error::Storage(format!("BEGIN 失败: {}", e)))?;
                    match conn.execute(
                        "DELETE FROM memories WHERE memory_id = ?1",
                        rusqlite::params![id],
                    ) {
                        Ok(affected) => {
                            if affected == 0 {
                                let _ = conn.execute_batch("ROLLBACK");
                                return Err(crate::Error::Storage(format!(
                                    "记忆文件不存在: {}",
                                    id
                                )));
                            }
                            conn.execute_batch("COMMIT")
                                .map_err(|e| crate::Error::Storage(format!("COMMIT 失败: {}", e)))?;
                            Ok(())
                        }
                        Err(e) => {
                            let _ = conn.execute_batch("ROLLBACK");
                            Err(crate::Error::Storage(format!("删除失败: {}", e)))
                        }
                    }
                })();
                results.push(r);
            }
            Ok(results)
        })
        .await
        .unwrap_or_else(|e| (0..count).map(|_| Err(e.clone())).collect())
    }

    /// 批量更新记忆文件（单连接优化）
    ///
    /// 在一个 `with_conn` 闭包内循环更新，只获取 1 次连接池连接。
    /// 每个更新独立事务（BEGIN IMMEDIATE），单个失败不影响其他。
    async fn update_memories_batch(
        &self,
        updates: &[(String, crate::model::MemoryUpdate)],
    ) -> Vec<crate::Result<()>> {
        let format = self.format;
        let updates: Vec<(String, crate::model::MemoryUpdate)> = updates.to_vec();
        let count = updates.len();
        self.with_conn(move |conn| {
            let mut results = Vec::with_capacity(updates.len());
            for (memory_id, upd) in &updates {
                if upd.is_empty() {
                    results.push(Ok(()));
                    continue;
                }
                let r = (|| -> crate::Result<()> {
                    let update_record = crate::model::MemoryUpdateRecord {
                        updated_at: chrono::Utc::now(),
                        update: upd.clone(),
                        conflicts: vec![],
                    };
                    conn.execute_batch("BEGIN IMMEDIATE")
                        .map_err(|e| crate::Error::Storage(format!("BEGIN 失败: {}", e)))?;
                    let content: Vec<u8> = match conn.query_row(
                        "SELECT content FROM memories WHERE memory_id = ?1",
                        rusqlite::params![memory_id],
                        |row| row.get(0),
                    ) {
                        Ok(c) => c,
                        Err(rusqlite::Error::QueryReturnedNoRows) => {
                            let _ = conn.execute_batch("ROLLBACK");
                            return Err(crate::Error::Storage(format!("记忆文件不存在: {}", memory_id)));
                        }
                        Err(other) => {
                            let _ = conn.execute_batch("ROLLBACK");
                            return Err(crate::Error::Storage(format!("查询失败: {}", other)));
                        }
                    };
                    let mut file = match format.deserialize_memory(&content) {
                        Ok(f) => f,
                        Err(e) => {
                            let _ = conn.execute_batch("ROLLBACK");
                            return Err(e);
                        }
                    };
                    file.updates.push(update_record);
                    let new_content = match format.serialize_memory(&file) {
                        Ok(c) => c,
                        Err(e) => {
                            let _ = conn.execute_batch("ROLLBACK");
                            return Err(e);
                        }
                    };
                    if let Err(e) = conn.execute(
                        "UPDATE memories SET content = ?1 WHERE memory_id = ?2",
                        rusqlite::params![&new_content, memory_id],
                    ) {
                        let _ = conn.execute_batch("ROLLBACK");
                        return Err(crate::Error::Storage(format!("更新失败: {}", e)));
                    }
                    conn.execute_batch("COMMIT")
                        .map_err(|e| crate::Error::Storage(format!("COMMIT 失败: {}", e)))?;
                    Ok(())
                })();
                results.push(r);
            }
            Ok(results)
        })
        .await
        .unwrap_or_else(|e| (0..count).map(|_| Err(e.clone())).collect())
    }

    // ========================================================================
    // session 元数据（v2.33 新增，场景识别结果持久化）
    // ========================================================================

    /// 写入 session 元数据（v2.33 新增）
    ///
    /// 覆盖写入（INSERT OR REPLACE）。由 `resolve_effective_scenario` 在首次识别后调用，
    /// 失败不应阻塞 archive 主流程（调用方应忽略错误并日志 warn）。
    async fn write_session_meta(
        &self,
        session_id: &str,
        meta: &SessionMeta,
    ) -> crate::Result<()> {
        let sid = session_id.to_string();
        let scenario = meta.scenario.clone();
        let confidence = meta.confidence;
        let method = meta.method.clone();
        let detected_at = meta.detected_at.to_rfc3339();
        let agent_family = meta.agent_family.clone();
        let hook_mode = meta.hook_mode.clone();

        self.with_conn(move |conn| {
            conn.execute(
                "INSERT OR REPLACE INTO session_meta (session_id, scenario, confidence, method, detected_at, agent_family, hook_mode) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![sid, scenario, confidence, method, detected_at, agent_family, hook_mode],
            ).map_err(|e| crate::Error::Storage(format!("写入 session_meta 失败: {}", e)))?;
            Ok(())
        })
        .await?;

        tracing::debug!(
            session_id = %session_id,
            scenario = %meta.scenario,
            "session_meta 已写入 SQLite"
        );
        Ok(())
    }

    /// 读取 session 元数据（v2.33 新增）
    ///
    /// 未识别时返回 `Ok(None)`（首次 archive 前）。
    /// 由 `resolve_effective_scenario` 在每次 archive 时调用，命中则跳过识别。
    async fn read_session_meta(
        &self,
        session_id: &str,
    ) -> crate::Result<Option<SessionMeta>> {
        let sid = session_id.to_string();

        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT scenario, confidence, method, detected_at, agent_family, hook_mode FROM session_meta WHERE session_id = ?1"
            ).map_err(|e| crate::Error::Storage(format!("prepare 失败: {}", e)))?;

            let row_result: rusqlite::Result<(String, f64, String, String, Option<String>, Option<String>)> =
                stmt.query_row(rusqlite::params![sid], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?))
                });

            match row_result {
                Ok((scenario, confidence, method, detected_at, agent_family, hook_mode)) => {
                    let dt = DateTime::parse_from_rfc3339(&detected_at)
                        .map_err(|e| crate::Error::Serialize(format!(
                            "解析 detected_at 失败: {}", e
                        )))?
                        .with_timezone(&Utc);
                    Ok(Some(SessionMeta {
                        scenario,
                        confidence: confidence as f32,
                        method,
                        detected_at: dt,
                        agent_family: agent_family.unwrap_or_default(),
                        hook_mode: hook_mode.unwrap_or_default(),
                    }))
                }
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(crate::Error::Storage(format!(
                    "查询 session_meta 失败: {}", e
                ))),
            }
        })
        .await
    }

    // ========================================================================
    // raw_context 原始上下文（v2.34 新增，pre_compress_hook 使用）
    // ========================================================================

    /// 写入 raw_context（v2.34 新增，pre_compress_hook 调用）
    ///
    /// INSERT OR REPLACE 语义：同一 (session_id, hook_id) 重复写入会覆盖。
    /// 返回虚拟相对路径 `sessions/{session_id}/raw_contexts/{hook_id}.txt`，
    /// 与 LocalStorage 保持一致（实际内容存储在 raw_contexts 表中）。
    async fn write_raw_context(
        &self,
        session_id: &str,
        hook_id: &str,
        content: &str,
    ) -> crate::Result<String> {
        let sid = session_id.to_string();
        let hid = hook_id.to_string();
        let content = content.to_string();
        let stored_at = chrono::Utc::now().to_rfc3339();

        self.with_conn(move |conn| {
            conn.execute(
                "INSERT OR REPLACE INTO raw_contexts (session_id, hook_id, content, stored_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![&sid, &hid, &content, &stored_at],
            )
            .map_err(|e| crate::Error::Storage(format!("写入 raw_context 失败: {}", e)))?;
            Ok(())
        })
        .await?;

        // 返回与 LocalStorage 一致的虚拟路径（POSIX 分隔符）
        Ok(format!("sessions/{}/raw_contexts/{}.txt", session_id, hook_id))
    }

    /// 读取 raw_context（v2.34 新增）
    ///
    /// 按 (session_id, hook_id) 检索。记录不存在时返回 `Err`（与 LocalStorage 行为一致，
    /// 因为压缩后重建时 raw_context 缺失是异常情况，应让调用方感知）。
    async fn read_raw_context(
        &self,
        session_id: &str,
        hook_id: &str,
    ) -> crate::Result<String> {
        let sid = session_id.to_string();
        let hid = hook_id.to_string();

        self.with_conn(move |conn| {
            let content: String = conn
                .query_row(
                    "SELECT content FROM raw_contexts WHERE session_id = ?1 AND hook_id = ?2",
                    rusqlite::params![&sid, &hid],
                    |row| row.get(0),
                )
                .map_err(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => {
                        crate::Error::Storage(format!(
                            "raw_context 不存在: session={}, hook={}",
                            sid, hid
                        ))
                    }
                    other => crate::Error::Storage(format!("查询 raw_context 失败: {}", other)),
                })?;
            Ok(content)
        })
        .await
    }

    /// 删除 raw_context（v2.34 新增，随记忆删除级联）
    ///
    /// DELETE 0 行也返回 Ok(())（幂等，与 LocalStorage 行为一致）。
    async fn delete_raw_context(
        &self,
        session_id: &str,
        hook_id: &str,
    ) -> crate::Result<()> {
        let sid = session_id.to_string();
        let hid = hook_id.to_string();

        self.with_conn(move |conn| {
            conn.execute(
                "DELETE FROM raw_contexts WHERE session_id = ?1 AND hook_id = ?2",
                rusqlite::params![&sid, &hid],
            )
            .map_err(|e| crate::Error::Storage(format!("删除 raw_context 失败: {}", e)))?;
            Ok(())
        })
        .await
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        ArchiveConfig, ArchivePeriod, IndexDocument, IndexHook, MemoryFile, MessageContent,
        MessageTurn, MemoryUpdate, Tag,
    };
    use chrono::Utc;
    use std::sync::Arc;
    use tempfile::TempDir;
    use uuid::Uuid;

    /// 构造测试用 MemoryFile
    fn make_memory(session_id: &str, project_id: Option<&str>, period: ArchivePeriod) -> MemoryFile {
        let turn = MessageTurn {
            id: Uuid::new_v4(),
            user_message: MessageContent {
                text: Some("用户问：如何实现记忆库？".into()),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                thinking: None,
                file_changes: Vec::new(),
            },
            llm_message: MessageContent {
                text: Some("LLM 答：通过归档+索引+检索三级机制".into()),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                thinking: None,
                file_changes: Vec::new(),
            },
            tags: vec![Tag::Text, Tag::CodeBlock],
            timestamp: Utc::now(),
            token_count: 100,
            stop_reason: None,
            cost: None,
        };
        MemoryFile::new(
            String::from(session_id),
            project_id.map(String::from),
            vec![turn],
            period,
        )
    }

    #[tokio::test]
    async fn test_sqlite_write_and_read_memory() {
        let tmp = TempDir::new().unwrap();
        let storage = SqliteStorage::new(tmp.path(), Some("proj-test".into())).unwrap();

        let original = make_memory("sess-1", Some("proj-test"), ArchivePeriod::Daily);
        let memory_id = storage.write_memory(&original).await.unwrap();

        // memory_id 应该是 UUID 字符串
        assert!(Uuid::parse_str(&memory_id).is_ok());

        let restored = storage.read_memory(&memory_id).await.unwrap();
        assert_eq!(original.id, restored.id);
        assert_eq!(original.session_id, restored.session_id);
        assert_eq!(original.turns.len(), restored.turns.len());
        assert_eq!(original.total_tokens, restored.total_tokens);
    }

    #[tokio::test]
    async fn test_sqlite_read_nonexistent_memory() {
        let tmp = TempDir::new().unwrap();
        let storage = SqliteStorage::new(tmp.path(), None).unwrap();

        let result = storage.read_memory("nonexistent-uuid").await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("记忆文件不存在"));
    }

    #[tokio::test]
    async fn test_sqlite_delete_memory() {
        let tmp = TempDir::new().unwrap();
        let storage = SqliteStorage::new(tmp.path(), None).unwrap();

        let file = make_memory("sess-del", None, ArchivePeriod::Daily);
        let memory_id = storage.write_memory(&file).await.unwrap();

        // 删除
        storage.delete_memory(&memory_id).await.unwrap();

        // 再读应失败
        let result = storage.read_memory(&memory_id).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_sqlite_list_memories() {
        let tmp = TempDir::new().unwrap();
        let storage = SqliteStorage::new(tmp.path(), None).unwrap();

        // 写入 3 个文件（同 session + Daily）
        for i in 0..3 {
            let mut f = make_memory("sess-list", None, ArchivePeriod::Daily);
            f.total_tokens = 100 + i;
            storage.write_memory(&f).await.unwrap();
        }

        let ids = storage
            .list_memories("sess-list", None, ArchivePeriod::Daily)
            .await
            .unwrap();
        assert_eq!(ids.len(), 3);

        // 不同 session 不应被列出
        let other_ids = storage
            .list_memories("other-session", None, ArchivePeriod::Daily)
            .await
            .unwrap();
        assert_eq!(other_ids.len(), 0);
    }

    #[tokio::test]
    async fn test_sqlite_list_memories_period_filter() {
        let tmp = TempDir::new().unwrap();
        let storage = SqliteStorage::new(tmp.path(), None).unwrap();

        let daily_file = make_memory("sess-p", None, ArchivePeriod::Daily);
        let weekly_file = make_memory("sess-p", None, ArchivePeriod::Weekly);
        storage.write_memory(&daily_file).await.unwrap();
        storage.write_memory(&weekly_file).await.unwrap();

        let daily_ids = storage
            .list_memories("sess-p", None, ArchivePeriod::Daily)
            .await
            .unwrap();
        assert_eq!(daily_ids.len(), 1);

        let weekly_ids = storage
            .list_memories("sess-p", None, ArchivePeriod::Weekly)
            .await
            .unwrap();
        assert_eq!(weekly_ids.len(), 1);
    }

    #[tokio::test]
    async fn test_sqlite_append_and_read_hook() {
        let tmp = TempDir::new().unwrap();
        let storage = SqliteStorage::new(tmp.path(), None).unwrap();

        let file = make_memory("sess-h", None, ArchivePeriod::Daily);
        let memory_id = storage.write_memory(&file).await.unwrap();

        let hook = IndexHook::from_memory_file(&file, memory_id.clone());
        storage
            .append_hook("sess-h", None, ArchivePeriod::Daily, hook.clone())
            .await
            .unwrap();

        let doc = storage
            .read_index("sess-h", None, ArchivePeriod::Daily)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(doc.hooks.len(), 1);
        assert_eq!(doc.hooks[0].id, hook.id);
        assert_eq!(doc.hooks[0].memory_id, memory_id);
        assert_eq!(doc.hooks[0].summary.title, hook.summary.title);
    }

    #[tokio::test]
    async fn test_sqlite_read_index_empty() {
        let tmp = TempDir::new().unwrap();
        let storage = SqliteStorage::new(tmp.path(), None).unwrap();

        let result = storage
            .read_index("no-session", None, ArchivePeriod::Daily)
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_sqlite_append_multiple_hooks() {
        let tmp = TempDir::new().unwrap();
        let storage = SqliteStorage::new(tmp.path(), None).unwrap();

        for i in 0..3 {
            let mut f = make_memory("sess-mh", None, ArchivePeriod::Daily);
            f.total_tokens = 100 + i;
            let memory_id = storage.write_memory(&f).await.unwrap();
            let hook = IndexHook::from_memory_file(&f, memory_id);
            storage
                .append_hook("sess-mh", None, ArchivePeriod::Daily, hook)
                .await
                .unwrap();
        }

        let doc = storage
            .read_index("sess-mh", None, ArchivePeriod::Daily)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(doc.hooks.len(), 3);
    }

    #[tokio::test]
    async fn test_sqlite_write_index_overwrite() {
        let tmp = TempDir::new().unwrap();
        let storage = SqliteStorage::new(tmp.path(), None).unwrap();

        // 写入 2 个 memory（获得真实 memory_id）
        let file1 = make_memory("sess-ow", None, ArchivePeriod::Daily);
        let file2 = make_memory("sess-ow", None, ArchivePeriod::Daily);
        let mid1 = storage.write_memory(&file1).await.unwrap();
        let mid2 = storage.write_memory(&file2).await.unwrap();

        // 写入初始索引文档（含 2 个 hook，使用真实 memory_id 满足外键约束）
        let mut doc1 = IndexDocument::new(
            String::from("sess-ow"),
            None,
            ArchivePeriod::Daily,
        );
        doc1.add_hook(IndexHook::from_memory_file(&file1, mid1.clone()));
        doc1.add_hook(IndexHook::from_memory_file(&file2, mid2.clone()));
        storage.write_index(&doc1).await.unwrap();

        // 验证有 2 个 hook
        let read1 = storage
            .read_index("sess-ow", None, ArchivePeriod::Daily)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read1.hooks.len(), 2);

        // 覆盖写入（只含 1 个 hook）
        let mut doc2 = IndexDocument::new(
            String::from("sess-ow"),
            None,
            ArchivePeriod::Daily,
        );
        doc2.add_hook(IndexHook::from_memory_file(&file1, mid1.clone()));
        storage.write_index(&doc2).await.unwrap();

        let read2 = storage
            .read_index("sess-ow", None, ArchivePeriod::Daily)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read2.hooks.len(), 1);
        assert_eq!(read2.hooks[0].memory_id, mid1);
    }

    #[tokio::test]
    async fn test_sqlite_project_hook_dual_write() {
        let tmp = TempDir::new().unwrap();
        let storage = SqliteStorage::new(tmp.path(), Some("proj-dw".into())).unwrap();

        let file = make_memory("sess-dw", Some("proj-dw"), ArchivePeriod::Daily);
        let memory_id = storage.write_memory(&file).await.unwrap();

        let hook = IndexHook::from_memory_file(&file, memory_id.clone());

        // 双写：session 级 + project 级
        storage
            .append_hook("sess-dw", Some("proj-dw"), ArchivePeriod::Daily, hook.clone())
            .await
            .unwrap();
        storage
            .append_project_hook("proj-dw", ArchivePeriod::Daily, hook.clone())
            .await
            .unwrap();

        // session 级索引
        let sess_doc = storage
            .read_index("sess-dw", None, ArchivePeriod::Daily)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(sess_doc.hooks.len(), 1);

        // project 级索引
        let proj_doc = storage
            .read_project_index("proj-dw", ArchivePeriod::Daily)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(proj_doc.hooks.len(), 1);
        assert_eq!(proj_doc.hooks[0].memory_id, memory_id);
    }

    #[tokio::test]
    async fn test_sqlite_list_project_memories() {
        let tmp = TempDir::new().unwrap();
        let storage = SqliteStorage::new(tmp.path(), Some("proj-lpm".into())).unwrap();

        // 写入 2 个文件（同 project 不同 session）
        let f1 = make_memory("sess-a", Some("proj-lpm"), ArchivePeriod::Daily);
        let f2 = make_memory("sess-b", Some("proj-lpm"), ArchivePeriod::Daily);
        storage.write_memory(&f1).await.unwrap();
        storage.write_memory(&f2).await.unwrap();

        // 跨 session 列出
        let ids = storage
            .list_project_memories("proj-lpm", ArchivePeriod::Daily)
            .await
            .unwrap();
        assert_eq!(ids.len(), 2);
    }

    #[tokio::test]
    async fn test_sqlite_msgpack_format() {
        let tmp = TempDir::new().unwrap();
        let storage = SqliteStorage::with_format(
            tmp.path(),
            Some("proj-mp".into()),
            SerializationFormat::MessagePack,
        )
        .unwrap();

        let original = make_memory("sess-mp", Some("proj-mp"), ArchivePeriod::Daily);
        let memory_id = storage.write_memory(&original).await.unwrap();

        let restored = storage.read_memory(&memory_id).await.unwrap();
        assert_eq!(original.id, restored.id);
        assert_eq!(original.session_id, restored.session_id);
    }

    #[tokio::test]
    async fn test_sqlite_update_memory() {
        let tmp = TempDir::new().unwrap();
        let storage = SqliteStorage::new(tmp.path(), None).unwrap();

        let file = make_memory("sess-upd", None, ArchivePeriod::Daily);
        let memory_id = storage.write_memory(&file).await.unwrap();
        let original_text = file.turns[0].user_message.text.clone().unwrap();

        // 更新记忆：added + revised + deprecated
        let updates = MemoryUpdate::new()
            .add_fact("新事实：MemoryCenter 项目 v2.4 完成")
            .revise_fact("修正：原定 v2.3 改为 v2.4")
            .deprecate_fact("废弃：旧的评分模型已过时");

        storage.update_memory(&memory_id, updates).await.unwrap();

        // 验证 updates 字段（v2.4 风险点修复：独立存储）
        let restored = storage.read_memory(&memory_id).await.unwrap();
        assert_eq!(restored.updates.len(), 1, "应有 1 条更新记录");

        let record = &restored.updates[0];
        assert_eq!(
            record.update.added_facts,
            vec!["新事实：MemoryCenter 项目 v2.4 完成"]
        );
        assert_eq!(
            record.update.revised_facts,
            vec!["修正：原定 v2.3 改为 v2.4"]
        );
        assert_eq!(
            record.update.deprecated_facts,
            vec!["废弃：旧的评分模型已过时"]
        );

        // 验证原始 text 未被污染
        let restored_text = restored.turns[0].user_message.text.as_ref().unwrap();
        assert_eq!(
            *restored_text, original_text,
            "原始 text 不应被 update 修改"
        );
    }

    #[tokio::test]
    async fn test_sqlite_update_memory_empty_is_noop() {
        let tmp = TempDir::new().unwrap();
        let storage = SqliteStorage::new(tmp.path(), None).unwrap();

        let file = make_memory("sess-upd-empty", None, ArchivePeriod::Daily);
        let memory_id = storage.write_memory(&file).await.unwrap();
        let original_text = storage
            .read_memory(&memory_id)
            .await
            .unwrap()
            .turns[0]
            .user_message
            .text
            .clone()
            .unwrap();

        // 空更新应是 no-op
        let updates = MemoryUpdate::new();
        storage.update_memory(&memory_id, updates).await.unwrap();

        let restored = storage.read_memory(&memory_id).await.unwrap();
        let restored_text = restored.turns[0].user_message.text.as_ref().unwrap();
        assert_eq!(*restored_text, original_text, "空更新不应修改内容");
        assert!(restored.updates.is_empty(), "空更新不应产生 updates 记录");
    }

    #[tokio::test]
    async fn test_sqlite_full_workflow() {
        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> = Arc::new(
            SqliteStorage::new(tmp.path(), Some("proj-fw".into())).unwrap(),
        );

        // 模拟 Agent 多轮归档
        let config = ArchiveConfig {
            token_threshold: 100,
            force_truncate_limit: 150,
            wait_for_turn_completion: true,
        };
        let mut archiver = crate::archive::Archiver::new(
            config,
            storage.clone(),
            "sess-fw",
            Some("proj-fw".into()),
        );

        archiver.push_turn({
            let t = MessageTurn {
                id: Uuid::new_v4(),
                user_message: MessageContent {
                    text: Some("讨论 SQLite WAL 模式".into()),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    thinking: None,
                    file_changes: Vec::new(),
                },
                llm_message: MessageContent {
                    text: Some("WAL 模式支持并发读".into()),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    thinking: None,
                    file_changes: Vec::new(),
                },
                tags: vec![Tag::Text],
                timestamp: Utc::now(),
                token_count: 60,
                stop_reason: None,
                cost: None,
            };
            t
        });
        archiver.push_turn({
            let t = MessageTurn {
                id: Uuid::new_v4(),
                user_message: MessageContent {
                    text: Some("继续讨论 r2d2 连接池".into()),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    thinking: None,
                    file_changes: Vec::new(),
                },
                llm_message: MessageContent {
                    text: Some("r2d2 提供 8 个连接".into()),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    thinking: None,
                    file_changes: Vec::new(),
                },
                tags: vec![Tag::Text],
                timestamp: Utc::now(),
                token_count: 50,
                stop_reason: None,
                cost: None,
            };
            t
        });

        let (memory, hook) = archiver.archive().await.unwrap();

        // 验证归档后状态
        assert_eq!(archiver.current_tokens(), 0);
        assert_eq!(archiver.pending_turns_count(), 0);

        // 用 Retriever 渲染 system prompt
        let retriever = crate::retrieve::Retriever::new(
            storage.clone(),
            "sess-fw",
            Some("proj-fw".into()),
        );
        let prompt = retriever.render_to_system_prompt().await.unwrap();
        assert!(prompt.contains("# 可用记忆索引"));
        assert!(prompt.contains("SQLite WAL"));
        assert!(prompt.contains(&hook.id.to_string()));

        // 通过 hook_id 检索详细记忆
        let retrieved = retriever
            .retrieve_memory(&hook.id.to_string())
            .await
            .unwrap();
        assert_eq!(retrieved.id, memory.id);
        assert_eq!(retrieved.turns.len(), 2);
        assert_eq!(retrieved.total_tokens, 110);
        assert!(!retrieved.truncated);
    }

    #[tokio::test]
    async fn test_sqlite_session_isolation() {
        // 验证不同 session 的记忆互不干扰
        let tmp = TempDir::new().unwrap();
        let storage = SqliteStorage::new(tmp.path(), None).unwrap();

        let f1 = make_memory("sess-iso-a", None, ArchivePeriod::Daily);
        let f2 = make_memory("sess-iso-b", None, ArchivePeriod::Daily);
        storage.write_memory(&f1).await.unwrap();
        storage.write_memory(&f2).await.unwrap();

        let a_ids = storage
            .list_memories("sess-iso-a", None, ArchivePeriod::Daily)
            .await
            .unwrap();
        let b_ids = storage
            .list_memories("sess-iso-b", None, ArchivePeriod::Daily)
            .await
            .unwrap();

        assert_eq!(a_ids.len(), 1);
        assert_eq!(b_ids.len(), 1);
        assert_ne!(a_ids[0], b_ids[0]);
    }

    // ========================================================================
    // 批量操作测试（v2.5 批次 6：SqliteStorage 优化版验证）
    // ========================================================================

    #[tokio::test]
    async fn test_sqlite_batch_read_memories() {
        // 验证 SqliteStorage batch 优化实现：单连接批量查询
        let tmp = TempDir::new().unwrap();
        let storage = SqliteStorage::new(tmp.path(), Some("proj-batch-r".into())).unwrap();

        // 写入 3 个记忆文件
        let mut ids = Vec::new();
        for i in 0..3 {
            let mut f = make_memory("sess-batch-r", Some("proj-batch-r"), ArchivePeriod::Daily);
            f.total_tokens = 100 + i;
            ids.push(storage.write_memory(&f).await.unwrap());
        }

        // 批量读取
        let results = storage.read_memories_batch(&ids).await;
        assert_eq!(results.len(), 3, "应返回 3 个结果");
        for r in &results {
            assert!(r.is_ok(), "全部应成功");
        }
        assert_eq!(results[0].as_ref().unwrap().total_tokens, 100);
        assert_eq!(results[1].as_ref().unwrap().total_tokens, 101);
        assert_eq!(results[2].as_ref().unwrap().total_tokens, 102);
    }

    #[tokio::test]
    async fn test_sqlite_batch_read_partial_failure() {
        // 验证：单个失败不影响其他条目（SQLite 优化版）
        let tmp = TempDir::new().unwrap();
        let storage = SqliteStorage::new(tmp.path(), None).unwrap();

        let file = make_memory("sess-batch-pf", None, ArchivePeriod::Daily);
        let good_id = storage.write_memory(&file).await.unwrap();
        let bad_id = Uuid::new_v4().to_string(); // 不存在的 UUID

        let results = storage
            .read_memories_batch(&[good_id.clone(), bad_id, good_id.clone()])
            .await;
        assert_eq!(results.len(), 3);
        assert!(results[0].is_ok(), "第 1 个应成功");
        assert!(results[1].is_err(), "第 2 个应失败（不存在）");
        assert!(results[2].is_ok(), "第 3 个应成功（不受前一个失败影响）");
    }

    #[tokio::test]
    async fn test_sqlite_batch_delete_memories() {
        let tmp = TempDir::new().unwrap();
        let storage = SqliteStorage::new(tmp.path(), None).unwrap();

        // 写入 3 个记忆
        let mut ids = Vec::new();
        for _ in 0..3 {
            let f = make_memory("sess-batch-d", None, ArchivePeriod::Daily);
            ids.push(storage.write_memory(&f).await.unwrap());
        }

        // 批量删除前 2 个
        let results = storage.delete_memories_batch(&ids[..2]).await;
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.is_ok()));

        // 验证已删除
        let remaining = storage
            .list_memories("sess-batch-d", None, ArchivePeriod::Daily)
            .await
            .unwrap();
        assert_eq!(remaining.len(), 1, "应剩 1 个");
        assert_eq!(remaining[0], ids[2], "剩余应为第 3 个");
    }

    #[tokio::test]
    async fn test_sqlite_batch_delete_mixed() {
        // 混合存在/不存在的 ID
        let tmp = TempDir::new().unwrap();
        let storage = SqliteStorage::new(tmp.path(), None).unwrap();

        let f = make_memory("sess-batch-dm", None, ArchivePeriod::Daily);
        let good_id = storage.write_memory(&f).await.unwrap();
        let bad_id = Uuid::new_v4().to_string();

        let results = storage
            .delete_memories_batch(&[good_id.clone(), bad_id])
            .await;
        assert_eq!(results.len(), 2);
        assert!(results[0].is_ok(), "存在的应删除成功");
        assert!(results[1].is_err(), "不存在的应返回错误（不影响其他）");

        // 验证 good_id 确实被删除
        let r = storage.read_memory(&good_id).await;
        assert!(r.is_err(), "good_id 应已被删除");
    }

    #[tokio::test]
    async fn test_sqlite_batch_update_memories() {
        let tmp = TempDir::new().unwrap();
        let storage = SqliteStorage::new(tmp.path(), None).unwrap();

        // 写入 2 个记忆
        let mut ids = Vec::new();
        for _ in 0..2 {
            let f = make_memory("sess-batch-u", None, ArchivePeriod::Daily);
            ids.push(storage.write_memory(&f).await.unwrap());
        }

        // 批量更新
        let updates: Vec<(String, MemoryUpdate)> = vec![
            (ids[0].clone(), MemoryUpdate::new().add_fact("事实 A")),
            (
                ids[1].clone(),
                MemoryUpdate::new().add_fact("事实 B").revise_fact("修正 X"),
            ),
        ];

        let results = storage.update_memories_batch(&updates).await;
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.is_ok()));

        // 验证更新已应用
        let m0 = storage.read_memory(&ids[0]).await.unwrap();
        assert_eq!(m0.updates.len(), 1);
        assert_eq!(m0.updates[0].update.added_facts, vec!["事实 A"]);

        let m1 = storage.read_memory(&ids[1]).await.unwrap();
        assert_eq!(m1.updates.len(), 1);
        assert_eq!(m1.updates[0].update.added_facts, vec!["事实 B"]);
        assert_eq!(m1.updates[0].update.revised_facts, vec!["修正 X"]);
    }

    #[tokio::test]
    async fn test_sqlite_batch_update_partial_failure() {
        let tmp = TempDir::new().unwrap();
        let storage = SqliteStorage::new(tmp.path(), None).unwrap();

        let f = make_memory("sess-batch-upf", None, ArchivePeriod::Daily);
        let good_id = storage.write_memory(&f).await.unwrap();
        let bad_id = Uuid::new_v4().to_string(); // 不存在

        let updates: Vec<(String, MemoryUpdate)> = vec![
            (good_id.clone(), MemoryUpdate::new().add_fact("OK")),
            (bad_id, MemoryUpdate::new().add_fact("FAIL")),
        ];

        let results = storage.update_memories_batch(&updates).await;
        assert_eq!(results.len(), 2);
        assert!(results[0].is_ok(), "存在的应更新成功");
        assert!(results[1].is_err(), "不存在的应返回错误");

        // 验证成功的那条确实更新了
        let m = storage.read_memory(&good_id).await.unwrap();
        assert_eq!(m.updates.len(), 1);
        assert_eq!(m.updates[0].update.added_facts, vec!["OK"]);
    }

    #[tokio::test]
    async fn test_sqlite_batch_empty_input() {
        // 空 slice 应返回空 Vec
        let tmp = TempDir::new().unwrap();
        let storage = SqliteStorage::new(tmp.path(), None).unwrap();

        let r1 = storage.read_memories_batch(&[]).await;
        assert!(r1.is_empty());

        let r2 = storage.delete_memories_batch(&[]).await;
        assert!(r2.is_empty());

        let r3 = storage.update_memories_batch(&[]).await;
        assert!(r3.is_empty());
    }

    #[tokio::test]
    async fn test_sqlite_batch_read_consistency_with_single() {
        // 验证：batch 读取的结果与单条读取一致
        let tmp = TempDir::new().unwrap();
        let storage = SqliteStorage::new(tmp.path(), None).unwrap();

        let mut ids = Vec::new();
        for i in 0..5 {
            let mut f = make_memory("sess-batch-c", None, ArchivePeriod::Daily);
            f.total_tokens = 200 + i;
            ids.push(storage.write_memory(&f).await.unwrap());
        }

        // 单条读取（顺序调用）
        let mut single: Vec<MemoryFile> = Vec::with_capacity(ids.len());
        for id in &ids {
            single.push(storage.read_memory(id).await.unwrap());
        }

        // 批量读取
        let batch = storage.read_memories_batch(&ids).await;
        assert_eq!(batch.len(), single.len());
        for (b, s) in batch.iter().zip(single.iter()) {
            assert!(b.is_ok());
            let b = b.as_ref().unwrap();
            assert_eq!(b.id, s.id);
            assert_eq!(b.total_tokens, s.total_tokens);
            assert_eq!(b.turns.len(), s.turns.len());
        }
    }

    // ========================================================================
    // v2.34 raw_context 3 方法测试（pre_compress_hook 持久化原始上下文）
    // ========================================================================

    mod v2_34_raw_context_tests {
        use super::*;

        /// 验证 raw_contexts 表存在且可查询（COUNT(*) 不报错）
        #[tokio::test]
        async fn test_raw_contexts_table_creation() {
            let tmp = TempDir::new().unwrap();
            let storage = SqliteStorage::new(tmp.path(), None).unwrap();

            // 直接通过 with_conn 验证 raw_contexts 表存在
            let count: i64 = storage
                .with_conn(|conn| {
                    let c: i64 = conn
                        .query_row("SELECT COUNT(*) FROM raw_contexts", [], |row| row.get(0))
                        .map_err(|e| {
                            crate::Error::Storage(format!("查询 raw_contexts 失败: {}", e))
                        })?;
                    Ok(c)
                })
                .await
                .expect("raw_contexts 表应存在且可查询");
            assert_eq!(count, 0, "新建数据库的 raw_contexts 表应为空");
        }

        /// write → read → delete → read 失败 完整 CRUD 流程
        #[tokio::test]
        async fn test_raw_contexts_crud() {
            let tmp = TempDir::new().unwrap();
            let storage = SqliteStorage::new(tmp.path(), None).unwrap();

            let session_id = "sess-crud";
            let hook_id = "hook-crud-001";
            let content = r#"{"turns":[{"user":"完整原始上下文"}]}"#;

            // Write
            let path = storage
                .write_raw_context(session_id, hook_id, content)
                .await
                .expect("write_raw_context 应成功");
            assert!(
                path.contains("raw_contexts"),
                "返回路径应包含 raw_contexts 目录, 实际: {}",
                path
            );
            assert!(
                path.contains(hook_id),
                "返回路径应包含 hook_id, 实际: {}",
                path
            );

            // Read
            let read_back = storage
                .read_raw_context(session_id, hook_id)
                .await
                .expect("read_raw_context 应成功");
            assert_eq!(read_back, content, "读取内容应与写入内容一致");

            // Delete
            storage
                .delete_raw_context(session_id, hook_id)
                .await
                .expect("delete_raw_context 应成功");

            // Read 失败
            let result = storage.read_raw_context(session_id, hook_id).await;
            assert!(
                result.is_err(),
                "删除后读取应失败, 实际: {:?}",
                result
            );
        }

        /// 验证 memories 表有 archive_reason 和 raw_context_path 列
        #[tokio::test]
        async fn test_alter_memories_add_archive_reason_column() {
            let tmp = TempDir::new().unwrap();
            let storage = SqliteStorage::new(tmp.path(), None).unwrap();

            let cols: Vec<String> = storage
                .with_conn(|conn| {
                    let mut stmt = conn
                        .prepare("SELECT name FROM pragma_table_info('memories') ORDER BY name")
                        .map_err(|e| {
                            crate::Error::Storage(format!("prepare pragma_table_info 失败: {}", e))
                        })?;
                    let rows: Vec<String> = stmt
                        .query_map([], |row| row.get::<_, String>(0))
                        .map_err(|e| {
                            crate::Error::Storage(format!("查询 pragma_table_info 失败: {}", e))
                        })?
                        .filter_map(|r| r.ok())
                        .collect();
                    Ok(rows)
                })
                .await
                .expect("应能查询 memories 表的列");

            assert!(
                cols.contains(&"archive_reason".to_string()),
                "memories 表应有 archive_reason 列, 实际列: {:?}",
                cols
            );
            assert!(
                cols.contains(&"raw_context_path".to_string()),
                "memories 表应有 raw_context_path 列, 实际列: {:?}",
                cols
            );
        }

        /// 同一 hook_id 重复写入应覆盖（INSERT OR REPLACE 语义）
        #[tokio::test]
        async fn test_write_raw_context_overwrites_existing() {
            let tmp = TempDir::new().unwrap();
            let storage = SqliteStorage::new(tmp.path(), None).unwrap();

            let session_id = "sess-overwrite";
            let hook_id = "hook-overwrite-001";
            let content_v1 = r#"{"version":1}"#;
            let content_v2 = r#"{"version":2,"turns":[]}"#;

            storage
                .write_raw_context(session_id, hook_id, content_v1)
                .await
                .expect("第一次 write_raw_context 应成功");

            storage
                .write_raw_context(session_id, hook_id, content_v2)
                .await
                .expect("第二次 write_raw_context 应成功");

            let read_back = storage
                .read_raw_context(session_id, hook_id)
                .await
                .expect("read_raw_context 应成功");
            assert_eq!(
                read_back, content_v2,
                "覆盖后读取应为 v2 内容, 实际: {}",
                read_back
            );
        }

        /// 删除不存在的 raw_context 应幂等成功（与 LocalStorage 行为一致）
        #[tokio::test]
        async fn test_delete_raw_context_idempotent() {
            let tmp = TempDir::new().unwrap();
            let storage = SqliteStorage::new(tmp.path(), None).unwrap();

            // 删除不存在的记录应成功（DELETE 0 行也返回 Ok）
            storage
                .delete_raw_context("non-existent-sess", "non-existent-hook")
                .await
                .expect("删除不存在的 raw_context 应幂等成功");
        }
    }
}
