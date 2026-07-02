//! # 数据模型模块
//!
//! 定义 Hippocampus 记忆库的核心数据结构：
//! - [`MemoryFile`]：记忆文件（一次归档的完整上下文）
//! - [`IndexHook`]：索引钩子（指向记忆文件的指针 + 标签）
//! - [`IndexDocument`]：索引文档（钩子集合）
//! - [`Tag`]：17 类细粒度标签
//! - [`MessageTurn`]：一轮消息（用户消息 + LLM 消息）

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Schema 版本号，用于未来迁移
pub const SCHEMA_VERSION: u32 = 1;

/// 17 类细粒度标签（索引钩子细粒度）
///
/// 标签可叠加（一条消息可有多个标签），非互斥。
/// 预留 `Other(String)` 兜底以支持未来扩展。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
pub enum Tag {
    /// 1. 文本消息
    Text,
    /// 2. 文件附件
    FileAttachment,
    /// 3. 图片
    Image,
    /// 4. 视频
    Video,
    /// 5. 工具调用
    ToolCall,
    /// 6. 思考过程
    Thinking,
    /// 7. 会话 ID
    SessionId,
    /// 8. 项目 ID
    ProjectId,
    /// 9. URL
    Url,
    /// 10. 引用
    Citation,
    /// 11. 状态
    Status,
    /// 12. UI
    Ui,
    /// 13. 代码块
    CodeBlock,
    /// 14. 语音
    Voice,
    /// 15. 计划
    Plan,
    /// 16. 使用的 Agent 工具（如 Codex 等）
    AgentTool,
    /// 17. 其他待定类型（预留扩展位）
    Other(String),
}

/// 一轮消息（用户消息 + LLM 消息）
///
/// 记忆文件内部的基本单元。每轮消息被打上类型标签（[`Tag`]），
/// 标签将被用于索引钩子。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageTurn {
    /// 轮次唯一 ID
    pub id: Uuid,
    /// 用户消息内容（原始完整内容，非摘要）
    pub user_message: MessageContent,
    /// LLM 消息内容（原始完整内容，非摘要）
    pub llm_message: MessageContent,
    /// 该轮次的标签集合（可叠加）
    pub tags: Vec<Tag>,
    /// 时间戳
    pub timestamp: DateTime<Utc>,
    /// 该轮次消耗的 token 数（用于归档阈值计量）
    pub token_count: usize,
}

/// 消息内容（支持多种媒介）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageContent {
    /// 文本部分（可能为空，如纯图片消息）
    pub text: Option<String>,
    /// 附件列表（文件/图片/视频/语音等）
    pub attachments: Vec<Attachment>,
    /// 工具调用列表
    pub tool_calls: Vec<ToolInvocation>,
    /// 思考过程（如 reasoning model 的思考链）
    pub thinking: Option<String>,
}

/// 附件（文件/图片/视频/语音等非文本内容）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    /// 附件类型
    pub kind: AttachmentKind,
    /// 引用路径（记忆库内的相对路径或外部 URL）
    pub uri: String,
    /// MIME 类型
    pub mime_type: Option<String>,
    /// 大小（字节）
    pub size: Option<u64>,
}

/// 附件种类
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AttachmentKind {
    /// 文件
    File,
    /// 图片
    Image,
    /// 视频
    Video,
    /// 语音
    Voice,
}

/// 工具调用记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInvocation {
    /// 工具名称（如 Codex / WebSearch 等）
    pub name: String,
    /// 调用参数（JSON 字符串）
    pub arguments: String,
    /// 调用结果（JSON 字符串）
    pub result: String,
    /// 调用耗时（毫秒）
    pub duration_ms: Option<u64>,
}

/// 记忆文件（一次归档的完整上下文）
///
/// 当会话窗口达到阈值（如 400K token）时，将该批次的完整上下文
/// （用户消息 + LLM 消息，轮次不限）冻结为一个记忆文件。
///
/// **注意**：记忆文件保存的是**完整上下文**，非摘要。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryFile {
    /// 记忆文件唯一 ID
    pub id: Uuid,
    /// Schema 版本（用于未来迁移）
    pub schema_version: u32,
    /// 归档时间戳
    pub archived_at: DateTime<Utc>,
    /// 所属会话 ID
    pub session_id: String,
    /// 所属项目 ID（可选）
    pub project_id: Option<String>,
    /// 该批次包含的所有轮次（完整内容，非摘要）
    pub turns: Vec<MessageTurn>,
    /// 该记忆文件的标签集合（所有轮次标签的并集）
    pub tags: Vec<Tag>,
    /// 总 token 数
    pub total_tokens: usize,
    /// 是否被强制截断（超过 1.5 倍阈值时）
    pub truncated: bool,
    /// 归档周期层级（Daily / Weekly / Monthly）
    pub period: ArchivePeriod,
    /// 访问计数（用于评分维度之一）
    pub access_count: u64,
    /// 用户显式重要性标记（0-100，默认 0）
    pub importance: u8,
}

/// 归档周期层级
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArchivePeriod {
    /// 天级：持续归档产生
    Daily,
    /// 周级：7 个天级文件无损去重合并而来
    Weekly,
    /// 月级：4 个周级文件评分淘汰后的主记忆
    Monthly,
}

/// 索引钩子（指向记忆库中一个记忆文件的指针）
///
/// 钩子是分层设计：
/// - **摘要钩子**：注入到 system prompt，包含标题+标签+时间戳（轻量）
/// - **详细钩子**：通过 tool 调用按需检索（含完整信息）
///
/// 本结构体包含完整信息，分层展示由 [`retrieve`] 模块处理。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexHook {
    /// 钩子唯一 ID
    pub id: Uuid,
    /// 指向的记忆文件 ID
    pub memory_file_id: Uuid,
    /// 记忆文件在记忆库中的相对路径
    pub memory_file_path: String,
    /// 摘要标题（用于 system prompt 注入）
    pub summary_title: String,
    /// 该钩子的标签集合
    pub tags: Vec<Tag>,
    /// 记忆文件归档时间
    pub archived_at: DateTime<Utc>,
    /// 归档周期层级
    pub period: ArchivePeriod,
    /// Token 数（供检索参考）
    pub token_count: usize,
}

/// 索引文档（钩子集合）
///
/// 一个索引文档包含多个索引钩子，指向记忆库中的多个记忆文件。
/// 索引文档按周期维护：天级持续追加，周级合并，月级评分淘汰后合并。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexDocument {
    /// 索引文档唯一 ID
    pub id: Uuid,
    /// Schema 版本
    pub schema_version: u32,
    /// 所属会话 ID
    pub session_id: String,
    /// 所属项目 ID（可选）
    pub project_id: Option<String>,
    /// 索引钩子集合
    pub hooks: Vec<IndexHook>,
    /// 最后更新时间
    pub updated_at: DateTime<Utc>,
    /// 周期层级
    pub period: ArchivePeriod,
}

/// 归档触发条件配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveConfig {
    /// Token 阈值（达到此值触发归档，如 400_000）
    pub token_threshold: usize,
    /// 强制截断上限（1.5 倍阈值，如 600_000）
    pub force_truncate_limit: usize,
    /// 是否等待当前轮次完成（动态范围）
    pub wait_for_turn_completion: bool,
}

impl Default for ArchiveConfig {
    fn default() -> Self {
        Self {
            token_threshold: 400_000,
            force_truncate_limit: 600_000,
            wait_for_turn_completion: true,
        }
    }
}
