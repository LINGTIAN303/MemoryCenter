//! # 存储模块
//!
//! 可插拔存储后端 trait。
//!
//! ## 设计
//!
//! - [`Storage`] trait：存储后端接口，可插拔
//! - [`LocalStorage`]：默认实现，本地文件树结构
//! - **并发策略**：RwLock 串行化写操作（读可并发）
//! - **原子写入**：temp + rename，防止崩溃导致文件损坏
//! - **索引追加**：读-改-写（read → add_hook → write back）
//!
//! ## 记忆库文件树结构
//!
//! ```text
//! memory_store/
//! ├── sessions/
//! │   └── {session_id}/
//! │       ├── daily/
//! │       │   ├── 2026-07-02_143052.json   # 天级记忆文件（日期_时间戳）
//! │       │   └── 2026-07-02_150230.json
//! │       ├── weekly/
//! │       │   └── 2026-W27.json           # 周级合并文件（ISO 周数）
//! │       ├── monthly/
//! │       │   └── 2026-07.json              # 月级主记忆文件
//! │       └── index/
//! │           ├── daily_index.json         # 天级索引文档
//! │           ├── weekly_index.json        # 周级索引文档
//! │           └── monthly_index.json       # 月级索引文档
//! └── projects/
//!     └── {project_id}/
//!         └── ... (同 sessions 结构)
//! ```
//!
//! ## 路径约定
//!
//! - 所有 `write_*` 方法返回**相对路径**（POSIX 分隔符 `/`，跨平台一致）
//! - 所有 `read_*` / `delete_*` 方法接受相对路径
//! - `read_index` / `list_memories` 按 session_id + period 查找（无需路径）

use crate::serialization::SerializationFormat;
use crate::model::{ArchivePeriod, IndexDocument, IndexHook, MemoryFile};
use chrono::{Datelike, NaiveDateTime};
use dashmap::DashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

/// 存储后端 trait
///
/// 所有存储后端（本地文件树、SQLite、S3 等）需实现此 trait。
/// 设计为单写多读：写入操作串行化，读取操作可并发。
///
/// ## ID 导向 + 双层检索（v2.4 重构）
///
/// - **session 层**：单一会话内的记忆存储（隔离）
///   - `write_memory` / `read_memory` / `delete_memory`：按 `memory_id`（相对路径或数据库 ID）
///   - `read_index` / `append_hook` / `list_memories`：按 `session_id` + `period`
/// - **project 层**：跨会话的聚合索引（共享）
///   - `read_project_index` / `append_project_hook` / `list_project_memories`：按 `project_id` + `period`
///
/// ### 双写模式
///
/// `archive` 时调用方应同时调用 `append_hook`（session 级）和 `append_project_hook`（project 级），
/// 实现跨会话检索能力。project 级方法带默认实现返回未实现错误，旧后端可继续工作。
///
/// ### 记忆迭代更新
///
/// `update_memory` 用于批次 3 的记忆迭代更新（added/revised/deprecated facts），
/// 默认实现返回未实现错误。
#[async_trait::async_trait]
pub trait Storage: Send + Sync {
    /// 写入记忆文件，返回 memory_id（相对路径或数据库 ID）
    async fn write_memory(&self, file: &MemoryFile) -> crate::Result<String>;

    /// 读取记忆文件（按 memory_id）
    async fn read_memory(&self, memory_id: &str) -> crate::Result<MemoryFile>;

    /// 删除记忆文件
    async fn delete_memory(&self, memory_id: &str) -> crate::Result<()>;

    /// 写入索引文档（全量覆盖写）
    async fn write_index(&self, doc: &IndexDocument) -> crate::Result<String>;

    /// 读取索引文档（按 session + period 查找）
    ///
    /// 返回 `Ok(None)` 表示文档不存在
    async fn read_index(
        &self,
        session_id: &str,
        project_id: Option<&str>,
        period: ArchivePeriod,
    ) -> crate::Result<Option<IndexDocument>>;

    /// 删除索引文档（v2.16 IMP-02 新增）
    ///
    /// 按 session + period 定位并删除整个索引文档。
    /// 主要用于周级合并后清理已合并的 daily 索引文档（可选配置，默认关闭）。
    ///
    /// ## 默认实现
    ///
    /// 默认返回 `Ok(())`（no-op，向后兼容旧后端）。
    /// 后端可覆写为实际的删除逻辑。
    ///
    /// ## 容错策略
    ///
    /// 索引文档不存在时视为已删除，返回 `Ok(())`（与 `delete_memory` 的"不存在则报错"行为不同，
    /// 因为索引文档是衍生数据，缺失不影响正确性）。
    async fn delete_index(
        &self,
        _session_id: &str,
        _project_id: Option<&str>,
        _period: ArchivePeriod,
    ) -> crate::Result<()> {
        // 默认 no-op：旧后端不支持删除索引
        Ok(())
    }

    /// 追加钩子到索引文档（读-改-写便利方法）
    ///
    /// 内部实现：读取现有索引文档 → 追加钩子 → 写回。
    /// 若文档不存在则创建新的。
    async fn append_hook(
        &self,
        session_id: &str,
        project_id: Option<&str>,
        period: ArchivePeriod,
        hook: IndexHook,
    ) -> crate::Result<()>;

    /// 列出指定会话/项目下某周期层级的所有记忆文件路径
    async fn list_memories(
        &self,
        session_id: &str,
        project_id: Option<&str>,
        period: ArchivePeriod,
    ) -> crate::Result<Vec<String>>;

    // ========================================================================
    // project 层聚合索引（v2.4 新增，跨会话检索）
    // ========================================================================

    /// 读取 project 级聚合索引文档
    ///
    /// 用于跨会话检索：同一个 project 下的所有 session 的记忆钩子
    /// 都聚合到此索引文档中。
    ///
    /// 返回 `Ok(None)` 表示文档不存在
    async fn read_project_index(
        &self,
        _project_id: &str,
        _period: ArchivePeriod,
    ) -> crate::Result<Option<IndexDocument>> {
        // 默认实现：返回未实现错误（旧后端可继续工作，但不支持跨会话检索）
        Err(crate::Error::Storage(
            "read_project_index 未实现: 后端不支持 project 级聚合索引".into(),
        ))
    }

    /// 追加钩子到 project 级聚合索引（双写模式）
    ///
    /// `archive` 时应同时调用 `append_hook`（session 级）和 `append_project_hook`（project 级），
    /// 实现跨会话检索能力。
    async fn append_project_hook(
        &self,
        _project_id: &str,
        _period: ArchivePeriod,
        _hook: IndexHook,
    ) -> crate::Result<()> {
        Err(crate::Error::Storage(
            "append_project_hook 未实现: 后端不支持 project 级聚合索引".into(),
        ))
    }

    /// 列出 project 下某周期层级的所有记忆文件路径（跨 session）
    async fn list_project_memories(
        &self,
        _project_id: &str,
        _period: ArchivePeriod,
    ) -> crate::Result<Vec<String>> {
        Err(crate::Error::Storage(
            "list_project_memories 未实现: 后端不支持 project 级聚合索引".into(),
        ))
    }

    // ========================================================================
    // 访问计数自增（v2.16 批次 1 新增：IMP-01）
    // ========================================================================

    /// 自增记忆文件的访问计数（retrieve 成功后调用）
    ///
    /// 默认实现为 no-op（旧后端可继续工作，不影响 retrieve 主流程）。
    /// 后端可覆写为原子自增以持久化访问次数，用于月级评分淘汰的 access_frequency 维度。
    ///
    /// ## 失败容忍
    ///
    /// 调用方（Retriever）应忽略此方法的错误，避免 access_count 自增失败影响 retrieve 主流程。
    async fn update_access_count(&self, _memory_id: &str) -> crate::Result<()> {
        // 默认 no-op：旧后端不支持访问计数自增
        Ok(())
    }

    // ========================================================================
    // 记忆迭代更新（v2.4 批次 3 新增）
    // ========================================================================

    /// 更新记忆文件（added/revised/deprecated facts）
    ///
    /// 用于批次 3 的记忆迭代更新：当 LLM 检测到新事实/修正事实/废弃事实时，
    /// 通过此方法更新记忆文件。
    async fn update_memory(
        &self,
        _memory_id: &str,
        _updates: crate::model::MemoryUpdate,
    ) -> crate::Result<()> {
        Err(crate::Error::Storage(
            "update_memory 未实现: 后端不支持记忆迭代更新".into(),
        ))
    }

    /// 更新记忆文件并携带冲突记录（v2.6 批次 8）
    ///
    /// 与 [`Storage::update_memory`] 相同，但额外接受 `conflicts` 参数，
    /// 将冲突记录随 [`crate::model::MemoryUpdateRecord`] 一起持久化。
    ///
    /// ## 默认实现
    ///
    /// 忽略 `conflicts`，直接调用 [`Storage::update_memory`]。
    /// 后端可覆写为完整实现以持久化冲突记录。
    ///
    /// ## 调用方
    ///
    /// 通常由 HTTP/MCP 层在调用 [`crate::conflict::ConflictDetector::detect`] 后调用：
    ///
    /// ```text,ignore
    /// let memory = storage.read_memory(&memory_id).await?;
    /// let report = detector.detect(&update, &memory).await;
    /// storage.update_memory_with_conflicts(&memory_id, update, report.conflicts).await?;
    /// ```
    async fn update_memory_with_conflicts(
        &self,
        memory_id: &str,
        updates: crate::model::MemoryUpdate,
        _conflicts: Vec<crate::conflict::ConflictRecord>,
    ) -> crate::Result<()> {
        // 默认实现：忽略 conflicts，降级为普通 update_memory
        self.update_memory(memory_id, updates).await
    }

    // ========================================================================
    // 批量操作（v2.5 批次 6 新增，带默认实现：循环调用单个方法）
    // ========================================================================

    /// 批量读取记忆文件
    ///
    /// 按传入的 `memory_ids` 顺序返回结果，单个失败不影响其他条目。
    ///
    /// **默认实现**：循环调用 `read_memory`。后端可覆写为单事务批量查询以优化性能。
    async fn read_memories_batch(
        &self,
        memory_ids: &[String],
    ) -> Vec<crate::Result<MemoryFile>> {
        let mut results = Vec::with_capacity(memory_ids.len());
        for id in memory_ids {
            results.push(self.read_memory(id).await);
        }
        results
    }

    /// 批量删除记忆文件
    ///
    /// 按传入的 `memory_ids` 顺序返回结果，单个失败不影响其他条目。
    ///
    /// **默认实现**：循环调用 `delete_memory`。后端可覆写为单事务批量删除以优化性能。
    async fn delete_memories_batch(
        &self,
        memory_ids: &[String],
    ) -> Vec<crate::Result<()>> {
        let mut results = Vec::with_capacity(memory_ids.len());
        for id in memory_ids {
            results.push(self.delete_memory(id).await);
        }
        results
    }

    /// 完整删除记忆（v2.31 新增，软删除方案）
    ///
    /// 删除记忆文件 + 将索引钩子标记为 `FileStatus::Deleted`（软删除）。
    ///
    /// ## 事务边界（不回滚）
    ///
    /// 1. 先删记忆文件（失败则直接返回错误，不继续）
    /// 2. 再读取索引文档，将对应 hook 的 `file_status` 标记为 `Deleted`，写回索引
    /// 3. 若索引更新失败：仅记录警告，**不回滚文件删除**
    ///
    /// **不回滚的理由**：文件已删 + 索引残留是脏数据（不影响正确性，retrieve 会防御性降级）；
    /// 反之文件残留 + 索引清了会导致 retrieve 崩溃，更危险。
    ///
    /// ## 软删除的价值
    ///
    /// 索引钩子保留 `summary` / `key_facts` 等元数据，让 LLM 知道"该记忆曾经存在但已被删除"，
    /// 而非崩溃或返回幽灵记忆。semantic_search 会过滤 `Deleted` 状态的 hook。
    ///
    /// ## 向后兼容
    ///
    /// 默认实现仅调 `delete_memory`（与旧行为一致）。LocalStorage 覆写为完整软删除逻辑。
    async fn delete_memory_complete(
        &self,
        memory_id: &str,
        hook_id: &str,
        session_id: &str,
        project_id: Option<&str>,
        period: crate::model::ArchivePeriod,
    ) -> crate::Result<()> {
        // 默认实现：仅删文件（与旧行为一致，向后兼容）
        let _ = (hook_id, session_id, project_id, period);
        self.delete_memory(memory_id).await
    }

    /// 批量更新记忆文件（added/revised/deprecated facts）
    ///
    /// 按传入的 `(memory_id, updates)` 顺序返回结果，单个失败不影响其他条目。
    ///
    /// **默认实现**：循环调用 `update_memory`。后端可覆写为单事务批量更新以优化性能。
    async fn update_memories_batch(
        &self,
        updates: &[(String, crate::model::MemoryUpdate)],
    ) -> Vec<crate::Result<()>> {
        let mut results = Vec::with_capacity(updates.len());
        for (id, upd) in updates {
            results.push(self.update_memory(id, upd.clone()).await);
        }
        results
    }

    /// 读取 session 任务状态快照（v2.31 新增，动手点 2）
    ///
    /// 从 `sessions/{session_id}/session_state.json` 读取最新任务状态。
    /// 返回 `Ok(None)` 表示该 session 无快照（首次归档前）。
    ///
    /// ## 默认实现
    ///
    /// 默认返回 `Ok(None)`（旧后端不支持任务状态快照）。
    async fn read_session_state(
        &self,
        _session_id: &str,
    ) -> crate::Result<Option<crate::model::TaskStateSnapshot>> {
        Ok(None)
    }

    /// 写入 session 任务状态快照（v2.31 新增，动手点 2）
    ///
    /// 覆盖写入 `sessions/{session_id}/session_state.json`。
    /// 每次 archive 时调用，保留最新状态。
    ///
    /// ## 默认实现
    ///
    /// 默认返回 `Ok(())`（no-op，旧后端不支持任务状态快照）。
    async fn write_session_state(
        &self,
        _session_id: &str,
        _snapshot: &crate::model::TaskStateSnapshot,
    ) -> crate::Result<()> {
        Ok(())
    }

    // ========================================================================
    // project_memory.md 反向写入（v2.31 新增，动手点 4）
    // ========================================================================

    /// 读取 project_memory.md 副本内容（v2.31 新增，动手点 4）
    ///
    /// 从 `projects/{project_id}/project_memory.md` 读取完整 Markdown 内容。
    /// 返回 `Ok(None)` 表示文件不存在（首次写入前）。
    ///
    /// ## 设计动机
    ///
    /// hippocampus 维护一份 project_memory 副本，LLM 调用 `update_project_memory`
    /// 更新副本后，用 Write 工具将内容写入 Trae 客户端的 memory 文件夹，
    /// 让 hippocampus 记忆"流入"第7层 Memory Context。
    ///
    /// ## 默认实现
    ///
    /// 默认返回 `Ok(None)`（旧后端不支持 project_memory 反向写入）。
    async fn read_project_memory(
        &self,
        _project_id: &str,
    ) -> crate::Result<Option<String>> {
        Ok(None)
    }

    /// 覆盖写入 project_memory.md 副本（v2.31 新增，动手点 4）
    ///
    /// 写入 `projects/{project_id}/project_memory.md`。
    /// 由 `update_project_memory` 工具调用，章节覆盖逻辑在工具层处理。
    ///
    /// ## 默认实现
    ///
    /// 默认返回 `Ok(())`（no-op，旧后端不支持 project_memory 反向写入）。
    async fn write_project_memory(
        &self,
        _project_id: &str,
        _content: &str,
    ) -> crate::Result<()> {
        Ok(())
    }

    /// 列出所有 session_id（v2.31 新增）
    ///
    /// 扫描 `sessions/` 目录下所有子目录名，返回 session_id 列表。
    /// 用于 prompt 工具返回可用 session 列表，引导 LLM 用正确的 session_id。
    ///
    /// ## 默认实现
    ///
    /// 默认返回空 Vec（旧后端不支持 session 列表）。
    async fn list_sessions(&self) -> crate::Result<Vec<String>> {
        Ok(Vec::new())
    }
}

/// 本地文件树存储后端
///
/// 将记忆文件以 JSON 或 MessagePack 格式存储在本地文件系统中。
/// 文件树结构见模块文档。
///
/// ## 并发（v2.4 升级：细粒度锁）
///
/// 内部用 [`DashMap`] + per-scope [`RwLock`] 实现细粒度并发：
/// - 不同 session/project 的写操作可并发
/// - 同一 session/project 的写操作串行化
/// - 读操作无锁可并发
///
/// 跨进程并发需由调用方保证（如文件锁）。
///
/// ## 双格式支持（v2.4 新增）
///
/// 通过 [`SerializationFormat`] 配置序列化格式：
/// - `Json`（默认）：可读性好，便于调试
/// - `MessagePack`：二进制紧凑，体积更小
///
/// 读取时根据文件后缀自动识别格式（`.json` / `.msgpack`）。
pub struct LocalStorage {
    /// 根目录路径
    root: PathBuf,
    /// 序列化格式（默认 JSON）
    format: SerializationFormat,
    /// 细粒度写锁（key = "session:{id}" 或 "project:{id}"）
    write_locks: DashMap<String, Arc<RwLock<()>>>,
}

impl LocalStorage {
    /// 创建新的本地存储后端（默认 JSON 格式）
    ///
    /// 注意：不会立即创建根目录，延迟到首次写入时创建。
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_format(root, SerializationFormat::Json)
    }

    /// 创建新的本地存储后端（指定序列化格式）
    ///
    /// **自动迁移**：构造时检测旧文件树结构（v2.3 `{root}/{session_id}/...`）
    /// 并迁移到新结构（v2.4 `{root}/sessions/{session_id}/...`）。幂等。
    pub fn with_format(root: impl Into<PathBuf>, format: SerializationFormat) -> Self {
        let root = root.into();
        Self::migrate_legacy_structure(&root);
        Self {
            root,
            format,
            write_locks: DashMap::new(),
        }
    }

    /// 检测并迁移旧文件树结构（v2.3 → v2.4）
    ///
    /// 旧结构：`{root}/{session_id}/{period}/...`
    /// 新结构：`{root}/sessions/{session_id}/{period}/...`
    ///
    /// 判定规则：root 下的直接子目录（排除 `sessions`/`projects`），
    /// 若包含 `daily`/`weekly`/`monthly` 任意子目录，视为旧 session 目录。
    ///
    /// 幂等：已是新结构则跳过；目标已存在则跳过（不覆盖）。
    fn migrate_legacy_structure(root: &Path) {
        let entries = match std::fs::read_dir(root) {
            Ok(e) => e,
            Err(_) => return, // 目录不存在，无需迁移
        };

        const PERIOD_DIRS: &[&str] = &["daily", "weekly", "monthly"];
        const NEW_TOP_DIRS: &[&str] = &["sessions", "projects"];

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };

            // 跳过新结构顶层目录
            if NEW_TOP_DIRS.contains(&name.as_str()) {
                continue;
            }

            // 检测旧 session 目录特征
            let is_legacy_session = PERIOD_DIRS.iter().any(|p| path.join(p).exists());
            if !is_legacy_session {
                continue;
            }

            // 迁移到 sessions/{session_id}/
            let sessions_dir = root.join("sessions");
            if let Err(e) = std::fs::create_dir_all(&sessions_dir) {
                tracing::warn!(error = %e, "创建 sessions/ 目录失败，跳过迁移");
                continue;
            }

            let dest = sessions_dir.join(&name);
            if dest.exists() {
                tracing::warn!(
                    legacy_dir = %path.display(),
                    dest = %dest.display(),
                    "跳过迁移：目标目录已存在"
                );
                continue;
            }

            match std::fs::rename(&path, &dest) {
                Ok(()) => tracing::info!(
                    session_id = %name,
                    "迁移旧结构 session 目录: {} → {}",
                    path.display(),
                    dest.display()
                ),
                Err(e) => tracing::warn!(
                    legacy_dir = %path.display(),
                    error = %e,
                    "迁移旧结构 session 目录失败"
                ),
            }
        }
    }

    /// 根目录
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 序列化格式
    pub fn format(&self) -> SerializationFormat {
        self.format
    }

    // ========================================================================
    // 路径生成（纯函数，无 IO）
    // ========================================================================

    /// 生成 session scope 根目录（sessions/{session_id}）
    fn session_scope_dir(&self, session_id: &str) -> PathBuf {
        PathBuf::from("sessions").join(session_id)
    }

    /// 生成 project scope 根目录（projects/{project_id}）
    fn project_scope_dir(&self, project_id: &str) -> PathBuf {
        PathBuf::from("projects").join(project_id)
    }

    /// 生成 session 某周期层级的目录路径（相对）
    fn period_dir(&self, session_id: &str, period: ArchivePeriod) -> PathBuf {
        self.session_scope_dir(session_id).join(period.as_dir_name())
    }

    /// 生成记忆文件的相对路径
    ///
    /// 记忆文件始终存到 `sessions/{session_id}/{period}/...`（v2.4 设计：session 隔离）。
    /// project 级聚合通过 `append_project_hook` 双写索引实现跨会话检索。
    fn memory_relative_path(&self, file: &MemoryFile) -> PathBuf {
        let dir = self.period_dir(&file.session_id, file.period);
        dir.join(self.memory_filename(file))
    }

    /// 生成记忆文件名
    ///
    /// - Daily: `{YYYY-MM-DD}_{HHMMSS}_{mmm}.{ext}`（日期+毫秒时间戳）
    /// - Weekly: `{YYYY}-W{WW}.{ext}`（ISO 周数）
    /// - Monthly: `{YYYY}-{MM}.{ext}`
    ///
    /// 文件后缀由序列化格式决定（`json` / `msgpack`）。
    ///
    /// # 毫秒精度的理由
    ///
    /// 秒级精度在快速连续归档场景（如单元测试、批量回填）下会冲突覆盖。
    /// 毫秒精度足以区分正常归档节奏，且可在文件名中保留可读性。
    fn memory_filename(&self, file: &MemoryFile) -> String {
        let dt: NaiveDateTime = file.archived_at.naive_utc();
        let ext = self.format.extension();
        match file.period {
            ArchivePeriod::Daily => format!("{}.{}", dt.format("%Y-%m-%d_%H%M%S_%3f"), ext),
            ArchivePeriod::Weekly => {
                let iso = file.archived_at.iso_week();
                format!("{:04}-W{:02}.{}", iso.year(), iso.week(), ext)
            }
            ArchivePeriod::Monthly => format!("{:04}-{:02}.{}", dt.year(), dt.month(), ext),
        }
    }

    /// 生成 session 级索引文档的相对路径
    fn session_index_path(&self, session_id: &str, period: ArchivePeriod) -> PathBuf {
        self.session_scope_dir(session_id)
            .join("index")
            .join(format!("{}_index.json", period.as_dir_name()))
    }

    /// 生成 project 级聚合索引文档的相对路径
    fn project_index_path(&self, project_id: &str, period: ArchivePeriod) -> PathBuf {
        self.project_scope_dir(project_id)
            .join("index")
            .join(format!("{}_index.json", period.as_dir_name()))
    }

    /// 拼接根目录得到绝对路径
    fn abs_path(&self, relative: &Path) -> PathBuf {
        self.root.join(relative)
    }

    /// 将相对路径转换为 POSIX 分隔符字符串（跨平台一致）
    fn to_posix_string(relative: &Path) -> String {
        relative.to_string_lossy().replace('\\', "/")
    }

    // ========================================================================
    // 细粒度锁
    // ========================================================================

    /// 获取 scope 写锁的 Arc 句柄（不阻塞）
    ///
    /// 若该 scope 的锁不存在则创建。返回 `Arc<RwLock<()>>` 供调用方 `.write().await`。
    fn get_write_lock(&self, scope_type: &str, scope_id: &str) -> Arc<RwLock<()>> {
        let key = format!("{}:{}", scope_type, scope_id);
        self.write_locks
            .entry(key)
            .or_insert_with(|| Arc::new(RwLock::new(())))
            .clone()
    }

    /// 获取 session 写锁
    fn session_write_lock(&self, session_id: &str) -> Arc<RwLock<()>> {
        self.get_write_lock("session", session_id)
    }

    /// 获取 project 写锁
    fn project_write_lock(&self, project_id: &str) -> Arc<RwLock<()>> {
        self.get_write_lock("project", project_id)
    }

    // ========================================================================
    // 路径解析（辅助方法）
    // ========================================================================

    /// 从 memory_id（相对路径）解析出 session_id
    ///
    /// memory_id 格式：`sessions/{session_id}/daily/xxx.json`
    /// 返回 `None` 表示无法解析（路径不符合预期格式）。
    fn parse_session_id(memory_id: &str) -> Option<String> {
        let parts: Vec<&str> = memory_id.splitn(4, '/').collect();
        if parts.len() >= 2 && parts[0] == "sessions" {
            Some(parts[1].to_string())
        } else {
            None
        }
    }

    // ========================================================================
    // IO 辅助方法
    // ========================================================================

    /// 确保目标文件的父目录存在
    async fn ensure_parent_dir(&self, path: &Path) -> crate::Result<()> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| crate::Error::Storage(format!("创建目录失败 {:?}: {}", parent, e)))?;
        }
        Ok(())
    }

    /// 原子写入（temp + rename）
    ///
    /// 流程：写入 `{filename}.tmp` → rename 到目标路径
    /// rename 在 Windows/Linux/macOS 上均原子替换目标文件
    async fn atomic_write(&self, path: &Path, content: &[u8]) -> crate::Result<()> {
        let tmp_name = format!(
            "{}.tmp",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("tmp")
        );
        let tmp_path = path.with_file_name(tmp_name);

        // 写入临时文件
        tokio::fs::write(&tmp_path, content)
            .await
            .map_err(|e| crate::Error::Storage(format!("写入临时文件失败 {:?}: {}", tmp_path, e)))?;

        // 原子 rename（覆盖目标）
        tokio::fs::rename(&tmp_path, path)
            .await
            .map_err(|e| crate::Error::Storage(format!("重命名失败 {:?} → {:?}: {}", tmp_path, path, e)))?;

        Ok(())
    }
}

#[async_trait::async_trait]
impl Storage for LocalStorage {
    async fn write_memory(&self, file: &MemoryFile) -> crate::Result<String> {
        // 细粒度锁：per session 串行化写
        let lock = self.session_write_lock(&file.session_id);
        let _guard = lock.write().await;

        let relative = self.memory_relative_path(file);
        let abs = self.abs_path(&relative);
        self.ensure_parent_dir(&abs).await?;

        // 双格式序列化（v2.4）
        let content = self.format.serialize_memory(file)?;
        self.atomic_write(&abs, &content).await?;

        Ok(Self::to_posix_string(&relative))
    }

    async fn read_memory(&self, memory_id: &str) -> crate::Result<MemoryFile> {
        let abs = self.root.join(memory_id);
        let content = tokio::fs::read(&abs)
            .await
            .map_err(|e| crate::Error::Storage(format!("读取记忆文件失败 {:?}: {}", memory_id, e)))?;

        // 根据文件后缀自动识别格式（v2.4）
        let path = Path::new(memory_id);
        let ext = path.extension().and_then(|e| e.to_str());
        let format = SerializationFormat::detect_from_extension(ext);
        format.deserialize_memory(&content)
    }

    async fn delete_memory(&self, memory_id: &str) -> crate::Result<()> {
        // 从 memory_id 解析 session_id 获取细粒度锁（解析失败则无锁删除）
        if let Some(session_id) = Self::parse_session_id(memory_id) {
            let lock = self.session_write_lock(&session_id);
            let _guard = lock.write().await;
            let abs = self.root.join(memory_id);
            tokio::fs::remove_file(&abs)
                .await
                .map_err(|e| crate::Error::Storage(format!("删除记忆文件失败 {:?}: {}", memory_id, e)))?;
        } else {
            // 无法解析 session_id，直接删除（无锁）
            let abs = self.root.join(memory_id);
            tokio::fs::remove_file(&abs)
                .await
                .map_err(|e| crate::Error::Storage(format!("删除记忆文件失败 {:?}: {}", memory_id, e)))?;
        }
        Ok(())
    }

    async fn write_index(&self, doc: &IndexDocument) -> crate::Result<String> {
        // 细粒度锁：per session
        let lock = self.session_write_lock(&doc.session_id);
        let _guard = lock.write().await;

        // v2.4: session 级索引始终存到 sessions/{session_id}/index/
        let relative = self.session_index_path(&doc.session_id, doc.period);
        let abs = self.abs_path(&relative);
        self.ensure_parent_dir(&abs).await?;

        let json = serde_json::to_vec_pretty(doc)
            .map_err(|e| crate::Error::Serialize(format!("序列化 IndexDocument 失败: {}", e)))?;

        self.atomic_write(&abs, &json).await?;

        Ok(Self::to_posix_string(&relative))
    }

    async fn delete_memory_complete(
        &self,
        memory_id: &str,
        hook_id: &str,
        session_id: &str,
        project_id: Option<&str>,
        period: ArchivePeriod,
    ) -> crate::Result<()> {
        // v2.31：完整删除 = 删文件 + 标记索引钩子为 Deleted（软删除）
        //
        // 事务边界：不回滚。文件已删 + 索引更新失败 = 脏数据（不影响正确性）；
        // 反之文件残留 + 索引清了 = retrieve 崩溃，更危险。

        use crate::model::FileStatus;

        // 1. 获取 session 写锁（整个流程在锁内，保证原子性）
        let lock = self.session_write_lock(session_id);
        let _guard = lock.write().await;

        // 2. 删除记忆文件（NotFound 视为已删除，幂等）
        let abs = self.root.join(memory_id);
        match tokio::fs::remove_file(&abs).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!(
                    memory_id = %memory_id,
                    "记忆文件不存在（视为已删除，继续标记索引）"
                );
            }
            Err(e) => {
                return Err(crate::Error::Storage(format!(
                    "删除记忆文件失败 {:?}: {}",
                    memory_id, e
                )));
            }
        }

        // 3. 读取索引文档 → 标记 hook 为 Deleted → 写回
        match self.read_index(session_id, project_id, period).await? {
            Some(mut doc) => {
                let mut found = false;
                for hook in &mut doc.hooks {
                    if hook.id.to_string() == hook_id {
                        hook.file_status = FileStatus::Deleted;
                        found = true;
                        tracing::info!(
                            hook_id = %hook_id,
                            memory_id = %memory_id,
                            session_id = %session_id,
                            "索引钩子已标记为 Deleted（软删除）"
                        );
                        break;
                    }
                }

                if !found {
                    tracing::warn!(
                        hook_id = %hook_id,
                        session_id = %session_id,
                        "索引文档中未找到 hook，无法标记 Deleted（文件已删除）"
                    );
                    // 不返回错误，因为文件已删，主要目标已达成
                } else {
                    // 写回索引文档（失败时仅警告，不回滚文件删除）
                    if let Err(e) = self.write_index(&doc).await {
                        tracing::warn!(
                            error = %e,
                            hook_id = %hook_id,
                            "索引写回失败：文件已删除但索引未标记为 Deleted，可能产生脏数据"
                        );
                        // 不返回错误：文件已删，主要目标已达成
                    }
                }
            }
            None => {
                tracing::warn!(
                    hook_id = %hook_id,
                    session_id = %session_id,
                    "索引文档不存在，无法标记 Deleted（文件已删除）"
                );
            }
        }

        Ok(())
    }

    async fn read_index(
        &self,
        session_id: &str,
        _project_id: Option<&str>,
        period: ArchivePeriod,
    ) -> crate::Result<Option<IndexDocument>> {
        // v2.4: session 级索引始终从 sessions/{session_id}/index/ 读取
        let relative = self.session_index_path(session_id, period);
        let abs = self.abs_path(&relative);

        match tokio::fs::read(&abs).await {
            Ok(content) => {
                let doc: IndexDocument = serde_json::from_slice(&content)
                    .map_err(|e| crate::Error::Serialize(format!("反序列化 IndexDocument 失败: {}", e)))?;
                Ok(Some(doc))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(crate::Error::Storage(format!(
                "读取索引文档失败 {:?}: {}",
                path_display(&relative),
                e
            ))),
        }
    }

    /// 读取 session 任务状态快照（v2.31 动手点 2）
    ///
    /// 从 `sessions/{session_id}/session_state.json` 读取。
    /// 文件不存在时返回 `Ok(None)`（首次归档前）。
    async fn read_session_state(
        &self,
        session_id: &str,
    ) -> crate::Result<Option<crate::model::TaskStateSnapshot>> {
        let relative = self.session_scope_dir(session_id).join("session_state.json");
        let abs = self.abs_path(&relative);

        match tokio::fs::read(&abs).await {
            Ok(content) => {
                let snapshot: crate::model::TaskStateSnapshot = serde_json::from_slice(&content)
                    .map_err(|e| crate::Error::Serialize(format!(
                        "反序列化 TaskStateSnapshot 失败: {}", e
                    )))?;
                Ok(Some(snapshot))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(crate::Error::Storage(format!(
                "读取 session_state.json 失败 {:?}: {}",
                path_display(&relative), e
            ))),
        }
    }

    /// 写入 session 任务状态快照（v2.31 动手点 2）
    ///
    /// 覆盖写入 `sessions/{session_id}/session_state.json`。
    /// 不加 session 写锁（与索引/记忆文件独立，无并发冲突风险）。
    async fn write_session_state(
        &self,
        session_id: &str,
        snapshot: &crate::model::TaskStateSnapshot,
    ) -> crate::Result<()> {
        let relative = self.session_scope_dir(session_id).join("session_state.json");
        let abs = self.abs_path(&relative);
        self.ensure_parent_dir(&abs).await?;

        let json = serde_json::to_vec_pretty(snapshot)
            .map_err(|e| crate::Error::Serialize(format!("序列化 TaskStateSnapshot 失败: {}", e)))?;

        self.atomic_write(&abs, &json).await?;

        tracing::debug!(
            session_id = %session_id,
            current_task = %snapshot.current_task,
            "session_state.json 已写入"
        );

        Ok(())
    }

    /// 读取 project_memory.md 副本内容（v2.31 动手点 4）
    ///
    /// 从 `projects/{project_id}/project_memory.md` 读取。
    /// 文件不存在时返回 `Ok(None)`（首次写入前）。
    async fn read_project_memory(
        &self,
        project_id: &str,
    ) -> crate::Result<Option<String>> {
        let relative = self.project_scope_dir(project_id).join("project_memory.md");
        let abs = self.abs_path(&relative);

        match tokio::fs::read_to_string(&abs).await {
            Ok(content) => Ok(Some(content)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(crate::Error::Storage(format!(
                "读取 project_memory.md 失败 {:?}: {}",
                path_display(&relative), e
            ))),
        }
    }

    /// 覆盖写入 project_memory.md 副本（v2.31 动手点 4）
    ///
    /// 写入 `projects/{project_id}/project_memory.md`。
    /// 不加 project 写锁（与索引/记忆文件独立，无并发冲突风险）。
    async fn write_project_memory(
        &self,
        project_id: &str,
        content: &str,
    ) -> crate::Result<()> {
        let relative = self.project_scope_dir(project_id).join("project_memory.md");
        let abs = self.abs_path(&relative);
        self.ensure_parent_dir(&abs).await?;

        self.atomic_write(&abs, content.as_bytes()).await?;

        tracing::debug!(
            project_id = %project_id,
            content_bytes = content.len(),
            "project_memory.md 已写入"
        );

        Ok(())
    }

    /// 列出所有 session_id（v2.31 新增）
    ///
    /// 扫描 `sessions/` 目录下所有子目录名。
    /// 目录不存在时返回空 Vec（首次使用前）。
    async fn list_sessions(&self) -> crate::Result<Vec<String>> {
        let sessions_dir = self.abs_path(&PathBuf::from("sessions"));

        let mut sessions = Vec::new();
        match tokio::fs::read_dir(&sessions_dir).await {
            Ok(mut entries) => {
                while let Some(entry) = entries.next_entry().await.map_err(|e| {
                    crate::Error::Storage(format!("读取 sessions/ 目录失败: {e}"))
                })? {
                    if entry.file_type().await.map_err(|e| {
                        crate::Error::Storage(format!("读取目录项类型失败: {e}"))
                    })?.is_dir() {
                        if let Some(name) = entry.file_name().to_str() {
                            sessions.push(name.to_string());
                        }
                    }
                }
                // 按名称排序，便于 LLM 阅读
                sessions.sort();
                Ok(sessions)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(crate::Error::Storage(format!(
                "读取 sessions/ 目录失败 {:?}: {}",
                sessions_dir, e
            ))),
        }
    }

    /// 删除索引文档（v2.16 IMP-02：LocalStorage 实现）
    ///
    /// 删除 sessions/{session_id}/index/{period}_index.json 文件。
    /// 文件不存在视为已删除，返回 Ok(())。
    async fn delete_index(
        &self,
        session_id: &str,
        _project_id: Option<&str>,
        period: ArchivePeriod,
    ) -> crate::Result<()> {
        // 细粒度锁：per session
        let lock = self.session_write_lock(session_id);
        let _guard = lock.write().await;

        let relative = self.session_index_path(session_id, period);
        let abs = self.abs_path(&relative);

        match tokio::fs::remove_file(&abs).await {
            Ok(()) => {
                tracing::debug!(
                    session_id = %session_id,
                    period = %period.as_str(),
                    path = %path_display(&relative),
                    "已删除索引文档"
                );
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // 文件不存在视为已删除
                Ok(())
            }
            Err(e) => Err(crate::Error::Storage(format!(
                "删除索引文档失败 {:?}: {}",
                path_display(&relative),
                e
            ))),
        }
    }

    async fn append_hook(
        &self,
        session_id: &str,
        _project_id: Option<&str>,
        period: ArchivePeriod,
        hook: IndexHook,
    ) -> crate::Result<()> {
        // 细粒度锁：per session
        let lock = self.session_write_lock(session_id);
        let _guard = lock.write().await;

        // v2.4: session 级索引始终存到 sessions/{session_id}/index/
        let relative = self.session_index_path(session_id, period);
        let abs = self.abs_path(&relative);

        // 读-改-写
        let mut doc: IndexDocument = match tokio::fs::read(&abs).await {
            Ok(content) => serde_json::from_slice(&content).map_err(|e| {
                crate::Error::Serialize(format!("反序列化 IndexDocument 失败: {}", e))
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // 文档不存在则创建新的
                IndexDocument::new(session_id.to_string(), None, period)
            }
            Err(e) => {
                return Err(crate::Error::Storage(format!(
                    "读取索引文档失败 {:?}: {}",
                    path_display(&relative),
                    e
                )))
            }
        };

        doc.add_hook(hook);

        let json = serde_json::to_vec_pretty(&doc)
            .map_err(|e| crate::Error::Serialize(format!("序列化 IndexDocument 失败: {}", e)))?;

        self.ensure_parent_dir(&abs).await?;
        self.atomic_write(&abs, &json).await?;

        Ok(())
    }

    async fn list_memories(
        &self,
        session_id: &str,
        _project_id: Option<&str>,
        period: ArchivePeriod,
    ) -> crate::Result<Vec<String>> {
        // v2.4: 记忆文件始终从 sessions/{session_id}/{period}/ 列出
        let relative = self.period_dir(session_id, period);
        let abs = self.abs_path(&relative);

        let mut entries = match tokio::fs::read_dir(&abs).await {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(crate::Error::Storage(format!(
                    "读取目录失败 {:?}: {}",
                    path_display(&relative),
                    e
                )))
            }
        };

        let mut paths = Vec::new();
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| crate::Error::Storage(format!("遍历目录失败: {}", e)))?
        {
            let p = entry.path();
            // v2.4: 支持双后缀（json / msgpack）
            if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                if ext == "json" || ext == "msgpack" {
                    // 转为相对路径（POSIX 分隔符）
                    let rel = p
                        .strip_prefix(&self.root)
                        .map_err(|e| crate::Error::Storage(format!("路径截取失败: {}", e)))?;
                    paths.push(Self::to_posix_string(rel));
                }
            }
        }

        paths.sort();
        Ok(paths)
    }

    // ========================================================================
    // project 层聚合索引（v2.4 新增，跨会话检索）
    // ========================================================================

    async fn read_project_index(
        &self,
        project_id: &str,
        period: ArchivePeriod,
    ) -> crate::Result<Option<IndexDocument>> {
        let relative = self.project_index_path(project_id, period);
        let abs = self.abs_path(&relative);

        match tokio::fs::read(&abs).await {
            Ok(content) => {
                let doc: IndexDocument = serde_json::from_slice(&content)
                    .map_err(|e| crate::Error::Serialize(format!("反序列化 IndexDocument 失败: {}", e)))?;
                Ok(Some(doc))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(crate::Error::Storage(format!(
                "读取 project 索引文档失败 {:?}: {}",
                path_display(&relative),
                e
            ))),
        }
    }

    async fn append_project_hook(
        &self,
        project_id: &str,
        period: ArchivePeriod,
        hook: IndexHook,
    ) -> crate::Result<()> {
        // 细粒度锁：per project
        let lock = self.project_write_lock(project_id);
        let _guard = lock.write().await;

        let relative = self.project_index_path(project_id, period);
        let abs = self.abs_path(&relative);

        // 读-改-写
        let mut doc: IndexDocument = match tokio::fs::read(&abs).await {
            Ok(content) => serde_json::from_slice(&content).map_err(|e| {
                crate::Error::Serialize(format!("反序列化 IndexDocument 失败: {}", e))
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // 文档不存在则创建新的（project 级索引的 session_id 字段填 project_id）
                IndexDocument::new(project_id.to_string(), Some(project_id.to_string()), period)
            }
            Err(e) => {
                return Err(crate::Error::Storage(format!(
                    "读取 project 索引文档失败 {:?}: {}",
                    path_display(&relative),
                    e
                )))
            }
        };

        doc.add_hook(hook);

        let json = serde_json::to_vec_pretty(&doc)
            .map_err(|e| crate::Error::Serialize(format!("序列化 IndexDocument 失败: {}", e)))?;

        self.ensure_parent_dir(&abs).await?;
        self.atomic_write(&abs, &json).await?;

        Ok(())
    }

    async fn list_project_memories(
        &self,
        project_id: &str,
        period: ArchivePeriod,
    ) -> crate::Result<Vec<String>> {
        // 通过 project 级聚合索引的 hooks 反查 memory_id 列表
        let doc = self.read_project_index(project_id, period).await?;
        let mut memory_ids: Vec<String> = match doc {
            Some(d) => d.hooks.into_iter().map(|h| h.memory_id).collect(),
            None => Vec::new(),
        };
        memory_ids.sort();
        memory_ids.dedup();
        Ok(memory_ids)
    }

    // ========================================================================
    // 访问计数自增（v2.16 批次 1：IMP-01）
    // ========================================================================

    async fn update_access_count(&self, memory_id: &str) -> crate::Result<()> {
        // 从 memory_id 解析 session_id 获取细粒度锁
        let session_id = Self::parse_session_id(memory_id)
            .unwrap_or_else(|| "unknown".to_string());
        let lock = self.session_write_lock(&session_id);
        let _guard = lock.write().await;

        // 读取 → record_access → 原子写回
        let mut file = self.read_memory(memory_id).await?;
        file.record_access();

        // 序列化回写（保持原格式）
        let abs = self.root.join(memory_id);
        let path = Path::new(memory_id);
        let ext = path.extension().and_then(|e| e.to_str());
        let format = SerializationFormat::detect_from_extension(ext);
        let content = format.serialize_memory(&file)?;
        self.atomic_write(&abs, &content).await?;

        tracing::debug!(
            memory_id = %memory_id,
            access_count = file.access_count,
            "访问计数自增完成"
        );

        Ok(())
    }

    // ========================================================================
    // 记忆迭代更新（v2.4 批次 3）
    // ========================================================================

    async fn update_memory(
        &self,
        memory_id: &str,
        updates: crate::model::MemoryUpdate,
    ) -> crate::Result<()> {
        // 委托给 update_memory_with_conflicts（传空 conflicts，向后兼容）
        self.update_memory_with_conflicts(memory_id, updates, vec![]).await
    }

    async fn update_memory_with_conflicts(
        &self,
        memory_id: &str,
        updates: crate::model::MemoryUpdate,
        conflicts: Vec<crate::conflict::ConflictRecord>,
    ) -> crate::Result<()> {
        // 空更新直接返回（幂等）
        if updates.is_empty() {
            return Ok(());
        }

        // 从 memory_id 解析 session_id 获取细粒度锁
        let session_id = Self::parse_session_id(memory_id)
            .unwrap_or_else(|| "unknown".to_string());
        let lock = self.session_write_lock(&session_id);
        let _guard = lock.write().await;

        // 读取现有 MemoryFile
        let mut file = self.read_memory(memory_id).await?;

        // v2.4 风险点修复：将 updates 追加到独立的 updates 字段
        // v2.6 批次 8：同时持久化冲突记录
        file.updates.push(crate::model::MemoryUpdateRecord {
            updated_at: chrono::Utc::now(),
            update: updates.clone(),
            conflicts,
        });

        // 序列化回写（保持原格式）
        let abs = self.root.join(memory_id);
        let path = Path::new(memory_id);
        let ext = path.extension().and_then(|e| e.to_str());
        let format = SerializationFormat::detect_from_extension(ext);
        let content = format.serialize_memory(&file)?;
        self.atomic_write(&abs, &content).await?;

        tracing::info!(
            memory_id = %memory_id,
            added = updates.added_facts.len(),
            revised = updates.revised_facts.len(),
            deprecated = updates.deprecated_facts.len(),
            total_updates = file.updates.len(),
            "记忆迭代更新完成（含冲突记录）"
        );

        Ok(())
    }
}

/// 路径显示辅助（用于错误信息）
fn path_display(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        ArchivePeriod, IndexDocument, IndexHook, MemoryFile, MessageContent, MessageTurn, Tag,
    };
    use chrono::Utc;
    use tempfile::TempDir;
    use uuid::Uuid;

    /// 构造测试用的 MemoryFile
    fn make_test_memory(period: ArchivePeriod, session_id: &str) -> MemoryFile {
        let turn = MessageTurn {
            id: Uuid::new_v4(),
            user_message: MessageContent {
                text: Some("用户问：如何实现一个记忆库？".into()),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                thinking: None,
            },
            llm_message: MessageContent {
                text: Some("LLM 答：可以通过归档+索引+检索三级机制实现...".into()),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                thinking: None,
            },
            tags: vec![Tag::Text, Tag::CodeBlock],
            timestamp: Utc::now(),
            token_count: 100,
        };
        MemoryFile::new(session_id, None, vec![turn], period)
    }

    /// 构造测试用的 MemoryFile（带 project_id）
    fn make_test_memory_with_project(period: ArchivePeriod) -> MemoryFile {
        let mut file = make_test_memory(period, "test-session");
        file.project_id = Some("proj-001".into());
        file
    }

    #[tokio::test]
    async fn test_write_and_read_memory() {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());

        let original = make_test_memory(ArchivePeriod::Daily, "sess-001");
        let path = storage.write_memory(&original).await.unwrap();

        // 验证返回的相对路径（POSIX 分隔符）
        assert!(path.contains("sessions/sess-001/daily/"));
        assert!(path.ends_with(".json"));
        assert!(!path.contains('\\'));

        // 读回验证
        let read_back = storage.read_memory(&path).await.unwrap();
        assert_eq!(read_back.session_id, "sess-001");
        assert_eq!(read_back.period, ArchivePeriod::Daily);
        assert_eq!(read_back.turns.len(), 1);
        assert_eq!(read_back.total_tokens, 100);
        assert!(read_back.tags.contains(&Tag::Text));
        assert!(read_back.tags.contains(&Tag::CodeBlock));
    }

    #[tokio::test]
    async fn test_read_memory_nonexistent() {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());

        let result = storage.read_memory("nonexistent.json").await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, crate::Error::Storage(_)));
    }

    #[tokio::test]
    async fn test_delete_memory() {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());

        let file = make_test_memory(ArchivePeriod::Daily, "sess-del");
        let path = storage.write_memory(&file).await.unwrap();

        // 删除
        storage.delete_memory(&path).await.unwrap();

        // 再读应失败
        let result = storage.read_memory(&path).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_list_memories() {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());

        // 写入 3 个 daily 记忆文件
        for _ in 0..3 {
            let file = make_test_memory(ArchivePeriod::Daily, "sess-list");
            // 加一点延迟避免时间戳冲突
            tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
            storage.write_memory(&file).await.unwrap();
        }

        let paths = storage
            .list_memories("sess-list", None, ArchivePeriod::Daily)
            .await
            .unwrap();
        assert_eq!(paths.len(), 3);

        // 验证路径已排序
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted);
    }

    #[tokio::test]
    async fn test_list_memories_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());

        // 目录不存在时返回空数组（而非错误）
        let paths = storage
            .list_memories("nonexistent-session", None, ArchivePeriod::Daily)
            .await
            .unwrap();
        assert!(paths.is_empty());
    }

    #[tokio::test]
    async fn test_append_hook_new_doc() {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());

        // 先写入一个记忆文件
        let file = make_test_memory(ArchivePeriod::Daily, "sess-hook");
        let memory_path = storage.write_memory(&file).await.unwrap();

        // 生成钩子并追加
        let hook = IndexHook::from_memory_file(&file, memory_path.clone());
        storage
            .append_hook("sess-hook", None, ArchivePeriod::Daily, hook)
            .await
            .unwrap();

        // 读回索引文档验证
        let doc = storage
            .read_index("sess-hook", None, ArchivePeriod::Daily)
            .await
            .unwrap();
        assert!(doc.is_some());
        let doc = doc.unwrap();
        assert_eq!(doc.hooks.len(), 1);
        assert_eq!(doc.hooks[0].memory_id, memory_path);
    }

    #[tokio::test]
    async fn test_append_hook_multiple() {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());

        // 追加 3 个钩子到同一个索引文档
        for _ in 0..3 {
            let file = make_test_memory(ArchivePeriod::Daily, "sess-multi");
            let path = storage.write_memory(&file).await.unwrap();
            let hook = IndexHook::from_memory_file(&file, path);
            tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
            storage
                .append_hook("sess-multi", None, ArchivePeriod::Daily, hook)
                .await
                .unwrap();
        }

        let doc = storage
            .read_index("sess-multi", None, ArchivePeriod::Daily)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(doc.hooks.len(), 3);
    }

    #[tokio::test]
    async fn test_read_index_nonexistent() {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());

        let result = storage
            .read_index("nonexistent", None, ArchivePeriod::Daily)
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_write_index_overwrite() {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());

        // 写入一个索引文档
        let mut doc = IndexDocument::new("sess-overwrite", None, ArchivePeriod::Weekly);
        let file = make_test_memory(ArchivePeriod::Weekly, "sess-overwrite");
        let path = storage.write_memory(&file).await.unwrap();
        doc.add_hook(IndexHook::from_memory_file(&file, path));
        storage.write_index(&doc).await.unwrap();

        // 覆盖写入
        let mut doc2 = IndexDocument::new("sess-overwrite", None, ArchivePeriod::Weekly);
        doc2.add_hook(IndexHook::from_memory_file(&file, "new-path".into()));
        storage.write_index(&doc2).await.unwrap();

        // 读回验证只剩新的钩子
        let read_back = storage
            .read_index("sess-overwrite", None, ArchivePeriod::Weekly)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read_back.hooks.len(), 1);
        assert_eq!(read_back.hooks[0].memory_id, "new-path");
    }

    #[tokio::test]
    async fn test_project_id_path() {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());

        let file = make_test_memory_with_project(ArchivePeriod::Daily);
        let path = storage.write_memory(&file).await.unwrap();

        // v2.4: 记忆文件始终存到 sessions/{session_id}/（session 隔离）
        assert!(path.starts_with("sessions/test-session/daily/"));
        assert!(!path.contains("projects/"));

        // list_memories 用 session_id 参数
        let paths = storage
            .list_memories("test-session", Some("proj-001"), ArchivePeriod::Daily)
            .await
            .unwrap();
        assert_eq!(paths.len(), 1);

        // project 级聚合索引（双写）：append_project_hook
        let hook = IndexHook::from_memory_file(&file, path.clone());
        storage
            .append_project_hook("proj-001", ArchivePeriod::Daily, hook.clone())
            .await
            .unwrap();

        // 从 project 级索引能读到这个 hook
        let proj_index = storage
            .read_project_index("proj-001", ArchivePeriod::Daily)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(proj_index.hooks.len(), 1);
        assert_eq!(proj_index.hooks[0].memory_id, path);
    }

    #[tokio::test]
    async fn test_memory_filename_daily_format() {
        let storage = LocalStorage::new(std::path::Path::new("/tmp/test"));
        let mut file = make_test_memory(ArchivePeriod::Daily, "x");
        // 固定时间验证格式
        file.archived_at = chrono::DateTime::parse_from_rfc3339("2026-07-02T14:30:52Z")
            .unwrap()
            .with_timezone(&Utc);

        let name = storage.memory_filename(&file);
        assert_eq!(name, "2026-07-02_143052_000.json");
    }

    #[tokio::test]
    async fn test_memory_filename_weekly_format() {
        let storage = LocalStorage::new(std::path::Path::new("/tmp/test"));
        let mut file = make_test_memory(ArchivePeriod::Weekly, "x");
        // 2026-07-02 是 ISO 第 27 周
        file.archived_at = chrono::DateTime::parse_from_rfc3339("2026-07-02T14:30:52Z")
            .unwrap()
            .with_timezone(&Utc);

        let name = storage.memory_filename(&file);
        assert_eq!(name, "2026-W27.json");
    }

    #[tokio::test]
    async fn test_memory_filename_monthly_format() {
        let storage = LocalStorage::new(std::path::Path::new("/tmp/test"));
        let mut file = make_test_memory(ArchivePeriod::Monthly, "x");
        file.archived_at = chrono::DateTime::parse_from_rfc3339("2026-07-02T14:30:52Z")
            .unwrap()
            .with_timezone(&Utc);

        let name = storage.memory_filename(&file);
        assert_eq!(name, "2026-07.json");
    }

    #[tokio::test]
    async fn test_atomic_write_survives_overwrite() {
        // 验证原子写入能正确覆盖已有文件
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());

        let file1 = make_test_memory(ArchivePeriod::Monthly, "sess-atomic");
        let path1 = storage.write_memory(&file1).await.unwrap();

        // 同一月份覆盖（monthly 文件名相同）
        let file2 = make_test_memory(ArchivePeriod::Monthly, "sess-atomic");
        let path2 = storage.write_memory(&file2).await.unwrap();

        assert_eq!(path1, path2);

        // 读回应该是 file2 的内容
        let read_back = storage.read_memory(&path2).await.unwrap();
        assert_eq!(read_back.id, file2.id);
    }

    // ====================================================================
    // v2.4 自动迁移测试
    // ====================================================================

    #[test]
    fn test_migrate_legacy_structure() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // 旧结构：{root}/sess-old/daily/... + index/
        let old_dir = root.join("sess-old");
        std::fs::create_dir_all(old_dir.join("daily")).unwrap();
        std::fs::create_dir_all(old_dir.join("index")).unwrap();
        std::fs::write(old_dir.join("daily").join("test.json"), "{}").unwrap();

        // 触发迁移
        let _storage = LocalStorage::new(root);

        // 旧目录应被移动到 sessions/sess-old/
        assert!(!old_dir.exists(), "旧目录应已迁移");
        let new_dir = root.join("sessions").join("sess-old");
        assert!(new_dir.exists(), "新目录应存在");
        assert!(new_dir.join("daily").join("test.json").exists());
        assert!(new_dir.join("index").exists());
    }

    #[test]
    fn test_migrate_idempotent() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // 旧结构
        std::fs::create_dir_all(root.join("sess-1").join("daily")).unwrap();

        // 第一次迁移
        let _s1 = LocalStorage::new(root);
        assert!(root.join("sessions").join("sess-1").exists());
        assert!(!root.join("sess-1").exists());

        // 第二次构造（已是新结构），不应重复迁移
        let _s2 = LocalStorage::new(root);
        assert!(root.join("sessions").join("sess-1").exists());
        assert!(!root.join("sess-1").exists());
    }

    #[test]
    fn test_migrate_skips_non_session_dirs() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // 非 session 目录（无 daily/weekly/monthly 子目录）
        std::fs::create_dir_all(root.join("random-dir")).unwrap();
        std::fs::write(root.join("random-dir").join("file.txt"), "hello").unwrap();

        let _storage = LocalStorage::new(root);

        // 应保留不动
        assert!(root.join("random-dir").exists());
        assert!(!root.join("sessions").exists());
    }

    #[test]
    fn test_migrate_skips_when_dest_exists() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // 先建新结构 sessions/sess-dup/daily/new.json
        let new_file = root
            .join("sessions")
            .join("sess-dup")
            .join("daily")
            .join("new.json");
        std::fs::create_dir_all(new_file.parent().unwrap()).unwrap();
        std::fs::write(&new_file, "{}").unwrap();

        // 再建旧结构 sess-dup/daily/old.json
        let old_file = root.join("sess-dup").join("daily").join("old.json");
        std::fs::create_dir_all(old_file.parent().unwrap()).unwrap();
        std::fs::write(&old_file, "{}").unwrap();

        // 触发迁移
        let _storage = LocalStorage::new(root);

        // 目标已存在 → 旧目录保留不覆盖
        assert!(root.join("sess-dup").exists(), "目标已存在时旧目录应保留");
        assert!(new_file.exists(), "新结构文件应保留");
        assert!(old_file.exists(), "旧文件应保留");
    }

    #[test]
    fn test_migrate_multiple_sessions() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // 多个旧 session 目录
        for sid in ["sess-a", "sess-b", "sess-c"] {
            std::fs::create_dir_all(root.join(sid).join("weekly")).unwrap();
        }

        let _storage = LocalStorage::new(root);

        for sid in ["sess-a", "sess-b", "sess-c"] {
            assert!(!root.join(sid).exists(), "旧目录 {} 应已迁移", sid);
            assert!(
                root.join("sessions").join(sid).join("weekly").exists(),
                "新目录 {} 应存在",
                sid
            );
        }
    }

    #[test]
    fn test_migrate_nonexistent_root() {
        // root 不存在时不报错
        let _storage = LocalStorage::new(std::path::Path::new("/nonexistent/path/xyz"));
    }

    // ====================================================================
    // v2.4 记忆迭代更新测试
    // ====================================================================

    #[tokio::test]
    async fn test_local_update_memory_added_revised_deprecated() {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());

        let file = make_test_memory(ArchivePeriod::Daily, "sess-upd");
        let memory_id = storage.write_memory(&file).await.unwrap();
        let original_text = file.turns[0].user_message.text.clone().unwrap();

        // 应用三类更新
        let updates = crate::model::MemoryUpdate::new()
            .add_fact("新事实：v2.4 批次 3 完成")
            .revise_fact("修正：原计划 v2.5 改为 v2.4")
            .deprecate_fact("废弃：旧的启发式评分已过时");

        storage.update_memory(&memory_id, updates).await.unwrap();

        // 验证 updates 字段（v2.4 风险点修复：独立存储）
        let restored = storage.read_memory(&memory_id).await.unwrap();
        assert_eq!(restored.updates.len(), 1, "应有 1 条更新记录");

        let record = &restored.updates[0];
        assert_eq!(
            record.update.added_facts,
            vec!["新事实：v2.4 批次 3 完成"]
        );
        assert_eq!(
            record.update.revised_facts,
            vec!["修正：原计划 v2.5 改为 v2.4"]
        );
        assert_eq!(
            record.update.deprecated_facts,
            vec!["废弃：旧的启发式评分已过时"]
        );

        // 验证原始 text 未被污染
        let restored_text = restored.turns[0].user_message.text.as_ref().unwrap();
        assert_eq!(
            *restored_text, original_text,
            "原始 text 不应被 update 修改"
        );
        assert!(
            !restored_text.contains("[新增事实]"),
            "原始 text 不应包含 update 标记"
        );
    }

    #[tokio::test]
    async fn test_local_update_memory_empty_is_noop() {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());

        let file = make_test_memory(ArchivePeriod::Daily, "sess-empty-upd");
        let memory_id = storage.write_memory(&file).await.unwrap();
        let original = storage.read_memory(&memory_id).await.unwrap();
        let original_text = original.turns[0].user_message.text.clone().unwrap();

        // 空更新 no-op
        storage
            .update_memory(&memory_id, crate::model::MemoryUpdate::new())
            .await
            .unwrap();

        let restored = storage.read_memory(&memory_id).await.unwrap();
        let restored_text = restored.turns[0].user_message.text.as_ref().unwrap();
        assert_eq!(*restored_text, original_text);
        assert!(restored.updates.is_empty(), "空更新不应产生 updates 记录");
    }

    #[tokio::test]
    async fn test_local_update_memory_nonexistent_fails() {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());

        let result = storage
            .update_memory(
                "sessions/sess-x/daily/nonexistent.json",
                crate::model::MemoryUpdate::new().add_fact("test"),
            )
            .await;
        assert!(result.is_err(), "更新不存在的记忆应失败");
    }

    #[tokio::test]
    async fn test_local_update_memory_idempotent_append() {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());

        let file = make_test_memory(ArchivePeriod::Daily, "sess-idem");
        let memory_id = storage.write_memory(&file).await.unwrap();

        // 第一次更新
        let updates1 = crate::model::MemoryUpdate::new().add_fact("事实 A");
        storage.update_memory(&memory_id, updates1).await.unwrap();

        // 第二次更新
        let updates2 = crate::model::MemoryUpdate::new().add_fact("事实 B");
        storage.update_memory(&memory_id, updates2).await.unwrap();

        // 验证两次更新都保留为独立记录
        let restored = storage.read_memory(&memory_id).await.unwrap();
        assert_eq!(restored.updates.len(), 2, "应有 2 条独立更新记录");
        assert_eq!(restored.updates[0].update.added_facts, vec!["事实 A"]);
        assert_eq!(restored.updates[1].update.added_facts, vec!["事实 B"]);
    }

    #[tokio::test]
    async fn test_local_update_memory_with_conflicts_persisted() {
        // v2.6 批次 8：验证 conflicts 字段被正确持久化
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());

        let file = make_test_memory(ArchivePeriod::Daily, "sess-conflict");
        let memory_id = storage.write_memory(&file).await.unwrap();

        // 构造带冲突记录的更新
        let updates = crate::model::MemoryUpdate::new().add_fact("用户不喜欢咖啡");
        let conflicts = vec![crate::conflict::ConflictRecord {
            kind: crate::conflict::ConflictKind::DirectContradict,
            severity: crate::conflict::Severity::Critical,
            description: "与历史事实直接矛盾".to_string(),
            existing_fact: Some("用户喜欢咖啡".to_string()),
            new_fact: "用户不喜欢咖啡".to_string(),
        }];

        storage
            .update_memory_with_conflicts(&memory_id, updates, conflicts)
            .await
            .unwrap();

        // 验证 conflicts 被持久化
        let restored = storage.read_memory(&memory_id).await.unwrap();
        assert_eq!(restored.updates.len(), 1);
        assert_eq!(restored.updates[0].conflicts.len(), 1);

        let c = &restored.updates[0].conflicts[0];
        assert_eq!(c.kind, crate::conflict::ConflictKind::DirectContradict);
        assert_eq!(c.severity, crate::conflict::Severity::Critical);
        assert_eq!(c.new_fact, "用户不喜欢咖啡");
        assert_eq!(c.existing_fact.as_deref(), Some("用户喜欢咖啡"));
    }

    #[tokio::test]
    async fn test_local_update_memory_empty_conflicts_backward_compat() {
        // v2.6 批次 8：空 conflicts 向后兼容（与旧 update_memory 行为一致）
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());

        let file = make_test_memory(ArchivePeriod::Daily, "sess-empty-c");
        let memory_id = storage.write_memory(&file).await.unwrap();

        let updates = crate::model::MemoryUpdate::new().add_fact("普通事实");
        storage
            .update_memory_with_conflicts(&memory_id, updates, vec![])
            .await
            .unwrap();

        let restored = storage.read_memory(&memory_id).await.unwrap();
        assert_eq!(restored.updates.len(), 1);
        assert!(restored.updates[0].conflicts.is_empty(), "空 conflicts 应正确持久化");
    }

    // ========================================================================
    // 批量操作测试（v2.5 批次 6：默认实现验证）
    // ========================================================================

    #[tokio::test]
    async fn test_batch_read_memories_default_impl() {
        // 验证 Storage trait 的默认 batch 实现：循环调用 read_memory
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());

        // 写入 3 个记忆文件
        let mut ids = Vec::new();
        for i in 0..3 {
            let mut f = make_test_memory(ArchivePeriod::Daily, "sess-batch-r");
            f.total_tokens = 100 + i;
            // 加延迟避免时间戳冲突
            tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
            let id = storage.write_memory(&f).await.unwrap();
            ids.push(id);
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
    async fn test_batch_read_memories_partial_failure() {
        // 验证：单个失败不影响其他条目
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());

        let file = make_test_memory(ArchivePeriod::Daily, "sess-batch-pf");
        let good_id = storage.write_memory(&file).await.unwrap();
        let bad_id = "nonexistent.json".to_string();

        let results = storage
            .read_memories_batch(&[good_id.clone(), bad_id, good_id.clone()])
            .await;
        assert_eq!(results.len(), 3);
        assert!(results[0].is_ok(), "第 1 个应成功");
        assert!(results[1].is_err(), "第 2 个应失败（不存在）");
        assert!(results[2].is_ok(), "第 3 个应成功（不受前一个失败影响）");
    }

    #[tokio::test]
    async fn test_batch_delete_memories_default_impl() {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());

        // 写入 3 个记忆
        let mut ids = Vec::new();
        for _ in 0..3 {
            let f = make_test_memory(ArchivePeriod::Daily, "sess-batch-d");
            tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
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
    async fn test_batch_delete_memories_mixed() {
        // 混合存在/不存在的 ID
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());

        let f = make_test_memory(ArchivePeriod::Daily, "sess-batch-dm");
        let good_id = storage.write_memory(&f).await.unwrap();
        let bad_id = "does-not-exist.json".to_string();

        let results = storage
            .delete_memories_batch(&[good_id.clone(), bad_id])
            .await;
        assert_eq!(results.len(), 2);
        assert!(results[0].is_ok(), "存在的应删除成功");
        assert!(
            results[1].is_err(),
            "不存在的应返回错误（但不影响其他条目）"
        );
    }

    #[tokio::test]
    async fn test_batch_update_memories_default_impl() {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());

        // 写入 2 个记忆
        let mut ids = Vec::new();
        for _ in 0..2 {
            let f = make_test_memory(ArchivePeriod::Daily, "sess-batch-u");
            tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
            ids.push(storage.write_memory(&f).await.unwrap());
        }

        // 批量更新
        let updates: Vec<(String, crate::model::MemoryUpdate)> = vec![
            (
                ids[0].clone(),
                crate::model::MemoryUpdate::new().add_fact("事实 A"),
            ),
            (
                ids[1].clone(),
                crate::model::MemoryUpdate::new()
                    .add_fact("事实 B")
                    .revise_fact("修正 X"),
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
    async fn test_batch_update_memories_partial_failure() {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());

        let f = make_test_memory(ArchivePeriod::Daily, "sess-batch-upf");
        let good_id = storage.write_memory(&f).await.unwrap();
        let bad_id = "nonexistent-update.json".to_string();

        let updates: Vec<(String, crate::model::MemoryUpdate)> = vec![
            (good_id.clone(), crate::model::MemoryUpdate::new().add_fact("OK")),
            (bad_id, crate::model::MemoryUpdate::new().add_fact("FAIL")),
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
    async fn test_batch_empty_input() {
        // 空 slice 应返回空 Vec（不报错）
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());

        let r1 = storage.read_memories_batch(&[]).await;
        assert!(r1.is_empty());

        let r2 = storage.delete_memories_batch(&[]).await;
        assert!(r2.is_empty());

        let r3 = storage.update_memories_batch(&[]).await;
        assert!(r3.is_empty());
    }
}
