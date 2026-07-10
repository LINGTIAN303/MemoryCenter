//! # MemoryCenter Agent 数据源适配器抽象层（v2.46 新增）
//!
//! 定义 [`AgentAdapter`] trait，抽象不同 Agent 工具的数据读取逻辑，
//! 让 sidecar 的 watcher 和 main 不直接依赖具体 Agent 的 DB/文件格式。
//!
//! ## 定位
//!
//! 位于 agents（分类层）和 sidecar（实现层）之间：
//!
//! ```text
//! agents    (分类层: AgentFamily / HookMode / AgentProfile)
//!    ↑
//! adapter   (抽象层: AgentAdapter trait + SidecarTurn + CompactionRecord)  ← 本 crate
//!    ↑
//! sidecar   (实现层: OpenCodeDb impl + ArchiveClient + watcher + main)
//! ```
//!
//! 依赖链单向：agents ← adapter ← sidecar，无循环。
//!
//! ## 当前实现
//!
//! | Adapter | Agent | 数据源 | 实现状态 |
//! |---------|-------|--------|---------|
//! | `OpenCodeDb` | OpenCode | SQLite（session_message / message + part 表） | ✅ 完整 |
//! | `ClaudeCodeAdapter` | Claude Code | JSONL 日志文件 | 🚧 未来 |
//! | 其他开源 Agent | - | - | 🚧 未来 |
//!
//! ## 设计原则
//!
//! 1. **trait 方法最小化**：只包含 sidecar watcher 和 main 需要的方法
//! 2. **类型通用化**：`CompactionRecord` / `SidecarTurn` 不含 Agent 专属字段
//! 3. **错误统一化**：`AdapterError` 保存错误信息字符串，不绑定具体 Agent 的错误类型
//! 4. **动态分发**：sidecar 用 `Box<dyn AgentAdapter>`，启动时按 `--agent` 选定，运行期不变。
//!    trait 只要求 `Send`（sidecar 单线程运行，不需要 `Sync`）

pub mod error;
pub mod record;
pub mod types;

pub use error::AdapterError;
pub use record::{CompactionRecord, SessionTokenInfo};
pub use types::{SidecarContent, SidecarFileChange, SidecarToolCall, SidecarTurn};

use memory_center_agents::AgentFamily;
use std::collections::HashMap;

/// Agent 数据源适配器 trait（v2.46 新增）
///
/// 抽象不同 Agent 工具的数据读取逻辑，让 watcher 和 main 不直接依赖
/// 具体 Agent 的 DB/文件格式。
///
/// ## 方法说明
///
/// | 方法 | 用途 | 调用方 |
/// |------|------|--------|
/// | `query_compactions()` | 查询所有压缩事件（按 time_created 升序） | watcher.poll() / watcher.backfill_events() |
/// | `read_turns_between()` | 读取两个 seq 之间的结构化 turns（增量归档） | main.archive_compaction_event() |
/// | `query_session_title()` | 查询 session 标题（用于日志） | main 启动时 |
/// | `family()` | 返回此 adapter 对应的 Agent 家族 | 日志 / 元数据 |
///
/// ## 实现示例
///
/// ```rust,ignore
/// use memory_center_adapter::{AgentAdapter, AdapterError, CompactionRecord, SidecarTurn};
/// use memory_center_agents::AgentFamily;
///
/// pub struct OpenCodeDb {
///     conn: rusqlite::Connection,
/// }
///
/// impl AgentAdapter for OpenCodeDb {
///     fn query_compactions(&self) -> Result<Vec<CompactionRecord>, AdapterError> {
///         // 查询 session_message 表 type='compaction' 的消息
///         // ...
///         # Ok(Vec::new())
///     }
///
///     fn read_turns_between(
///         &self,
///         session_id: &str,
///         from_seq: Option<i64>,
///         to_seq: i64,
///         max_turns: usize,
///     ) -> Result<Vec<SidecarTurn>, AdapterError> {
///         // 读取 (from_seq, to_seq) 之间的消息，解析为结构化 turns
///         // ...
///         # Ok(Vec::new())
///     }
///
///     fn query_session_title(&self, session_id: &str) -> Result<String, AdapterError> {
///         // SELECT title FROM session WHERE id = ?
///         // ...
///         # Ok(session_id.to_string())
///     }
///
///     fn family(&self) -> AgentFamily {
///         AgentFamily::OpenCode
///     }
/// }
/// ```
pub trait AgentAdapter: Send {
    /// 查询所有压缩事件（按 time_created 升序）
    ///
    /// sidecar 用 `message_id` 去重，发现新消息即触发归档。
    ///
    /// ## 返回
    ///
    /// 所有 compaction 消息，按 `time_created` 升序排列。
    /// 空 Vec 表示无压缩事件（或该 Agent 不支持压缩检测）。
    fn query_compactions(&self) -> Result<Vec<CompactionRecord>, AdapterError>;

    /// 读取两个 seq 之间的结构化 turns（增量归档）
    ///
    /// 归档范围：`(from_seq, to_seq)`，exclusive 两端。
    /// - `from_seq`：上次 compaction 的 seq（None 表示从会话开头）
    /// - `to_seq`：本次 compaction 的 seq
    ///
    /// 跳过 compaction 类型消息本身。
    ///
    /// ## 参数
    ///
    /// - `session_id`：会话 ID
    /// - `from_seq`：起始 seq（exclusive），None 表示从会话开头
    /// - `to_seq`：结束 seq（exclusive）
    /// - `max_turns`：最大 turns 数（防止超大会话撑爆 MemoryCenter）
    fn read_turns_between(
        &self,
        session_id: &str,
        from_seq: Option<i64>,
        to_seq: i64,
        max_turns: usize,
    ) -> Result<Vec<SidecarTurn>, AdapterError>;

    /// 查询 session 标题（用于日志展示）
    ///
    /// 若该 Agent 无 session 标题概念，返回 session_id 本身。
    fn query_session_title(&self, session_id: &str) -> Result<String, AdapterError>;

    /// 返回此 adapter 对应的 Agent 家族
    ///
    /// 用于日志展示和元数据记录。
    fn family(&self) -> AgentFamily;

    /// 查询所有活跃 session 的 token 累积信息（v2.47 新增）
    ///
    /// 用于阈值监控：sidecar 每次轮询时调用，获取每个活跃 session
    /// 从上次归档 seq 到最新 seq 之间的 token 累积值。
    /// 达到阈值时触发主动归档 + 插入 compaction 消息对（清空上下文）。
    ///
    /// ## 参数
    ///
    /// - `last_archived_seqs`：每个 session 上次归档的 seq（来自 SidecarState）
    ///   - key: session_id
    ///   - value: 上次归档的 seq（该 seq 之前的数据已归档）
    ///
    /// ## 返回
    ///
    /// 所有 `accumulated_tokens > 0` 的活跃 session 列表。
    /// 空 Vec 表示无活跃 session 或所有 session 均无新消息。
    ///
    /// ## 实现说明
    ///
    /// token 来源因 Agent 而异：
    /// - OpenCode V2：session_message 表 step-finish part 的 input + output + reasoning
    /// - OpenCode V1：message + part 表的 step-finish part
    /// - 未来 ClaudeCode：可能从 JSONL 日志解析
    fn query_active_sessions_tokens(
        &self,
        last_archived_seqs: &HashMap<String, i64>,
    ) -> Result<Vec<SessionTokenInfo>, AdapterError>;

    /// 检测 DB 的 schema 标签（风险 3 修复）
    ///
    /// 返回当前 DB 的 schema 标签（如 "v1"、"v2"），用于启动时检测 schema 变化。
    /// 当 OpenCode 升级导致 schema 变化时，旧的 `last_archived_seq` 语义可能
    /// 不再适用（如 V1 用毫秒时间戳，V2 用整数序列号），需重置增量归档状态。
    ///
    /// 默认返回 "unknown"，具体 adapter 按实际 DB schema 返回。
    fn detect_schema_tag(&self) -> String {
        "unknown".to_string()
    }
}
