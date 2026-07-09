//! # Storage trait 定义
//!
//! 可插拔存储后端 trait，无原生 IO 依赖，WASM 兼容。
//! 具体实现（LocalStorage / SqliteStorage / CachedStorage）在 MemoryCenter-core crate。
//!
//! ## 设计
//!
//! - [`Storage`] trait：存储后端接口，可插拔
//! - [`SessionMeta`]：session 元数据（场景识别结果持久化）
//! - 实现端在 MemoryCenter-core：`LocalStorage`（本地文件树）、`SqliteStorage`、`CachedStorage<T>`
//!
//! ## ID 导向 + 双层检索（v2.4 重构）
//!
//! - **session 层**：单一会话内的记忆存储（隔离）
//!   - `write_memory` / `read_memory` / `delete_memory`：按 `memory_id`（相对路径或数据库 ID）
//!   - `read_index` / `append_hook` / `list_memories`：按 `session_id` + `period`
//! - **project 层**：跨会话的聚合索引（共享）
//!   - `read_project_index` / `append_project_hook` / `list_project_memories`：按 `project_id` + `period`

use crate::model::{ArchivePeriod, IndexDocument, IndexHook, MemoryFile};
use chrono::{DateTime, Utc};

/// Session 元数据（v2.33 新增，v2.40 扩展）
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
/// - `agent_family`（v2.40 新增）：产生此 session 的 Agent family（如 "OpenCode" / "Trae"）
/// - `hook_mode`（v2.40 新增）：钩子模式（"real"=真钩子 / "pseudo"=伪钩子）
///
/// ## 向后兼容
///
/// v2.40 新增的 `agent_family` / `hook_mode` 字段使用 `#[serde(default)]`，
/// 旧版 meta.json / 旧版 session_meta 行无这两个字段时反序列化为空字符串，
/// 上层读取时若为空可按需补全（如从 session_id 前缀反解 family）。
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
    /// 产生此 session 的 Agent family 显示名（v2.40 新增）
    ///
    /// 如 "OpenCode" / "Trae" / "Cursor" / "Claude Code"。
    /// 旧版数据无此字段时为空字符串，上层可从 session_id 前缀补全。
    #[serde(default)]
    pub agent_family: String,
    /// 钩子模式（v2.40 新增）："real"（真钩子）/ "pseudo"（伪钩子）
    ///
    /// 由 `HookModeResolver::resolve(family).as_str()` 生成。
    /// 旧版数据无此字段时为空字符串，上层降级为 "pseudo"。
    #[serde(default)]
    pub hook_mode: String,
}

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
// WASM 下 JsFuture 不 Send，用 ?Send 模式；native 下保持 Send 以支持多线程
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
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
    /// MemoryCenter 维护一份 project_memory 副本，LLM 调用 `update_project_memory`
    /// 更新副本后，用 Write 工具将内容写入 Trae 客户端的 memory 文件夹，
    /// 让 MemoryCenter 记忆"流入"第7层 Memory Context。
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

    // ========================================================================
    // raw_context 原始上下文（v2.34 新增，pre_compress_hook 使用）
    // ========================================================================

    /// 写入 raw_context 文件（仅 pre_compress_hook 调用）
    ///
    /// 在 Trae 客户端压缩上下文前，由 pre_compress_hook 将完整原始上下文
    /// （未摘要的轮次 JSON）持久化到存储后端，避免压缩丢失原始内容。
    ///
    /// ## 路径约定
    ///
    /// `sessions/{session_id}/raw_contexts/{hook_id}.txt`
    ///
    /// 返回相对路径（POSIX 分隔符）。
    ///
    /// ## 默认实现
    ///
    /// 默认返回 `Err`（旧后端不支持 raw_context 持久化）。
    async fn write_raw_context(
        &self,
        _session_id: &str,
        _hook_id: &str,
        _content: &str,
    ) -> crate::Result<String> {
        Err(crate::Error::Storage(
            "write_raw_context 未实现: 后端不支持 raw_context 持久化".into(),
        ))
    }

    /// 读取 raw_context 文件内容（按 hook_id 检索）
    ///
    /// 用于压缩后重建上下文：LLM 通过 hook_id 拉取对应的原始上下文，
    /// 与 MemoryCenter 一手记忆交叉校准 Trae Summary。
    ///
    /// ## 默认实现
    ///
    /// 默认返回 `Err`（旧后端不支持 raw_context 持久化）。
    async fn read_raw_context(
        &self,
        _session_id: &str,
        _hook_id: &str,
    ) -> crate::Result<String> {
        Err(crate::Error::Storage(
            "read_raw_context 未实现: 后端不支持 raw_context 持久化".into(),
        ))
    }

    /// 删除 raw_context 文件（随记忆删除级联）
    ///
    /// 当 `delete_memory_complete` 删除记忆时，应级联删除对应的 raw_context 文件。
    /// NotFound 视为成功（幂等，与 `delete_index` 行为一致）。
    ///
    /// ## 默认实现
    ///
    /// 默认返回 `Err`（旧后端不支持 raw_context 持久化）。
    async fn delete_raw_context(
        &self,
        _session_id: &str,
        _hook_id: &str,
    ) -> crate::Result<()> {
        Err(crate::Error::Storage(
            "delete_raw_context 未实现: 后端不支持 raw_context 持久化".into(),
        ))
    }
}
