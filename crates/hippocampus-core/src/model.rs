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

// v2.30.1：以下默认值生成函数供 MessageTurn 字段级 #[serde(default = "...")] 使用
// 让 Agent 调用 archive 时可省略 id/timestamp/tags/token_count，服务端反序列化时自动补全

/// 生成新 UUID（用于 MessageTurn.id 缺省时）
fn default_uuid() -> Uuid {
    Uuid::new_v4()
}

/// 取当前时间（用于 MessageTurn.timestamp 缺省时）
fn default_timestamp() -> DateTime<Utc> {
    Utc::now()
}

/// 默认标签集合（空，用于 MessageTurn.tags 缺省时）
/// v2.30.1：改为空 Vec，由 apply_turn_defaults 在反序列化后根据内容推断
fn default_tags() -> Vec<Tag> {
    Vec::new()
}

// v2.30.1：以下函数用于反序列化后的二次补全
// 当 Agent 未传 tags（空 Vec）或未传 token_count（0）时，根据内容启发式推断

/// 根据 MessageContent 内容启发式推断标签
///
/// 判定规则（按优先级，可叠加）：
/// 1. `tool_calls` 非空 → `ToolCall` + `AgentTool`
/// 2. `attachments` 含图片 → `Image`
/// 3. `attachments` 含视频 → `Video`
/// 4. `attachments` 含语音 → `Voice`
/// 5. `attachments` 含文件 → `FileAttachment`
/// 6. `thinking` 非空 → `Thinking`
/// 7. `text` 含代码块（``` ） → `CodeBlock`
/// 8. `text` 含 URL（http:// 或 https://） → `Url`
/// 9. 以上都不匹配 → `Text`（兜底）
pub fn infer_tags(content: &MessageContent) -> Vec<Tag> {
    let mut tags: Vec<Tag> = Vec::new();

    // 1. 工具调用（最高优先级，通常意味着 Agent 执行了动作）
    if !content.tool_calls.is_empty() {
        tags.push(Tag::ToolCall);
        tags.push(Tag::AgentTool);
    }

    // 2-5. 附件类型
    for att in &content.attachments {
        match att.kind {
            AttachmentKind::Image => {
                if !tags.contains(&Tag::Image) {
                    tags.push(Tag::Image);
                }
            }
            AttachmentKind::Video => {
                if !tags.contains(&Tag::Video) {
                    tags.push(Tag::Video);
                }
            }
            AttachmentKind::Voice => {
                if !tags.contains(&Tag::Voice) {
                    tags.push(Tag::Voice);
                }
            }
            AttachmentKind::File => {
                if !tags.contains(&Tag::FileAttachment) {
                    tags.push(Tag::FileAttachment);
                }
            }
        }
    }

    // 6. 思考过程（reasoning model 的思考链）
    if content.thinking.as_ref().is_some_and(|s| !s.is_empty()) {
        tags.push(Tag::Thinking);
    }

    // 7-8. 文本特征检测
    if let Some(text) = &content.text {
        // 代码块检测（``` 或缩进 4 空格的代码风格）
        if text.contains("```") {
            tags.push(Tag::CodeBlock);
        }
        // URL 检测（简单的 http/https 协议匹配）
        if text.contains("http://") || text.contains("https://") {
            tags.push(Tag::Url);
        }
    }

    // 9. 兜底：如果没有任何特征标签，标为 Text
    if tags.is_empty() {
        tags.push(Tag::Text);
    }

    tags
}

/// 估算 MessageContent 的 token 数
///
/// 经验值：英文 4 char ≈ 1 token，中文 1.5 char ≈ 1 token
/// 取折中：3 char ≈ 1 token（中文偏高，英文偏低，整体可接受）
///
/// 估算范围：text + thinking + tool_calls(arguments + result)
pub fn estimate_tokens(content: &MessageContent) -> usize {
    let mut chars: usize = 0;

    // 文本部分
    if let Some(text) = &content.text {
        chars += text.chars().count();
    }

    // 思考过程
    if let Some(thinking) = &content.thinking {
        chars += thinking.chars().count();
    }

    // 工具调用的参数和结果
    for tc in &content.tool_calls {
        chars += tc.name.chars().count();
        chars += tc.arguments.chars().count();
        chars += tc.result.chars().count();
    }

    // 3 char ≈ 1 token（折中估算）
    // 最小 1，避免完全空消息返回 0
    (chars / 3).max(1)
}

/// 对 MessageTurn 应用自动补全（反序列化后调用）
///
/// 规则：
/// - `tags` 为空（Agent 未传或传了空数组）→ 根据 user_message + llm_message 推断
/// - `token_count` 为 0（Agent 未传）→ 根据内容估算
/// - 已传入的值不覆盖（向后兼容 + Agent 显式优先）
pub fn apply_turn_defaults(turn: &mut MessageTurn) {
    // tags 为空时，合并 user_message + llm_message 的推断标签
    if turn.tags.is_empty() {
        let mut inferred = infer_tags(&turn.user_message);
        let llm_tags = infer_tags(&turn.llm_message);
        for t in llm_tags {
            if !inferred.contains(&t) {
                inferred.push(t);
            }
        }
        // 如果两边都只推断出 Text，保持单个 Text
        if inferred.iter().all(|t| *t == Tag::Text) {
            turn.tags = vec![Tag::Text];
        } else {
            // 有非 Text 标签时，移除多余的 Text（避免工具调用也带 Text）
            turn.tags = inferred.into_iter().filter(|t| *t != Tag::Text).collect();
            // 如果过滤后为空（理论上不会，因为 infer_tags 有兜底），补 Text
            if turn.tags.is_empty() {
                turn.tags = vec![Tag::Text];
            }
        }
    }

    // token_count 为 0 时，估算
    if turn.token_count == 0 {
        let estimated = estimate_tokens(&turn.user_message) + estimate_tokens(&turn.llm_message);
        turn.token_count = estimated.max(1); // 最小 1，避免全 0
    }
}

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
///
/// v2.30.1：除 user_message/llm_message 外，其余字段均可省略，服务端反序列化时自动补全：
/// - `id` 缺省 → 生成新 UUID（`Uuid::new_v4()`）
/// - `timestamp` 缺省 → 取当前时间（`Utc::now()`）
/// - `tags` 缺省 → `[Tag::Text]`（纯对话场景默认标签）
/// - `token_count` 缺省 → `0`（不影响归档，但阈值检测会按 0 计算）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageTurn {
    /// 轮次唯一 ID（缺省时服务端自动生成）
    #[serde(default = "default_uuid")]
    pub id: Uuid,
    /// 用户消息内容（原始完整内容，非摘要）
    pub user_message: MessageContent,
    /// LLM 消息内容（原始完整内容，非摘要）
    pub llm_message: MessageContent,
    /// 该轮次的标签集合（可叠加，缺省时为 `[Tag::Text]`）
    #[serde(default = "default_tags")]
    pub tags: Vec<Tag>,
    /// 时间戳（缺省时取服务端当前时间）
    #[serde(default = "default_timestamp")]
    pub timestamp: DateTime<Utc>,
    /// 该轮次消耗的 token 数（用于归档阈值计量，缺省时为 0）
    #[serde(default)]
    pub token_count: usize,
}

/// 消息内容（支持多种媒介）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageContent {
    /// 文本部分（可能为空，如纯图片消息）
    #[serde(default)]
    pub text: Option<String>,
    /// 附件列表（文件/图片/视频/语音等）
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// 工具调用列表
    #[serde(default)]
    pub tool_calls: Vec<ToolInvocation>,
    /// 思考过程（如 reasoning model 的思考链）
    #[serde(default)]
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
    /// 记忆迭代更新历史（v2.4 批次 3 风险点修复）
    ///
    /// 每次通过 `update_memory` 更新时追加一条 [`MemoryUpdateRecord`]，
    /// 不污染原始 `turns` 内容。旧文件无此字段时默认为空（向后兼容）。
    #[serde(default)]
    pub updates: Vec<MemoryUpdateRecord>,
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
/// - **摘要钩子**：注入到 system prompt，包含结构化摘要+标签+时间戳（轻量）
/// - **详细钩子**：通过 tool 调用按需检索（含完整信息）
///
/// 本结构体包含完整信息，分层展示由 [`retrieve`] 模块处理。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexHook {
    /// 钩子唯一 ID
    pub id: Uuid,
    /// 指向的记忆文件 ID（LocalStorage 用路径作 ID，SQLite 用 UUID）
    pub memory_id: String,
    /// 结构化摘要（借鉴 Memora 线索锚点设计）
    pub summary: Summary,
    /// 该钩子的标签集合
    pub tags: Vec<Tag>,
    /// 记忆文件归档时间
    pub archived_at: DateTime<Utc>,
    /// 归档周期层级
    pub period: ArchivePeriod,
    /// Token 数（供检索参考）
    pub token_count: usize,
    /// 关联文件状态（v2.31 新增，支持软删除）
    ///
    /// - `Normal`：文件可读（默认）
    /// - `Deleted`：文件已删除，索引钩子保留元数据供 LLM 知晓"曾经存在"
    /// - `Corrupted`：文件存在但读取失败（未来扩展）
    ///
    /// 旧索引文件未序列化此字段时，`#[serde(default)]` 自动填充为 `Normal`，向后兼容。
    #[serde(default)]
    pub file_status: FileStatus,
}

/// 索引钩子关联文件的状态（v2.31 新增，软删除支持）
///
/// 设计动机：原 `delete_memory` 只删文件不清索引，导致索引与存储不一致。
/// v2.31 改为软删除——在索引钩子上标记 `Deleted`，保留元数据（summary/key_facts），
/// 让 LLM 知道"该记忆曾经存在但已被删除"，而非崩溃或返回幽灵记忆。
///
/// ## 序列化兼容
///
/// 使用 `#[serde(rename_all = "lowercase")]`，序列化为 `normal` / `deleted` / `corrupted`。
/// 旧索引文件未包含此字段时，`#[serde(default)]` 自动填充为 `Normal`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum FileStatus {
    /// 正常，文件可读
    #[default]
    Normal,
    /// 已删除（软删除标记，文件不存在但索引钩子保留元数据）
    Deleted,
    /// 损坏（文件存在但读取失败，未来扩展用）
    Corrupted,
}

/// 结构化摘要（借鉴 Memora 线索锚点设计）
///
/// 从单一标题升级为多维摘要，支持分级生成：
/// - 日级：启发式生成（title + key_facts）
/// - 周级：LLM 生成（title + abstract + key_facts + key_entities）
/// - 月级：LLM 生成（全字段，含 clue_anchors）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    /// 一句话标题（向后兼容原 summary_title）
    ///
    /// 日级启发式：首条消息前 80 字符
    pub title: String,

    /// 抽象摘要（2-3 句话，提炼主题）
    ///
    /// 周级/月级 LLM 生成，日级为 None
    #[serde(default)]
    pub abstract_text: Option<String>,

    /// 关键事实（事实级别，可被直接引用）
    ///
    /// 周级/月级 LLM 生成，日级为空
    #[serde(default)]
    pub key_facts: Vec<String>,

    /// 关键实体（人名/项目名/技术名词等）
    ///
    /// 周级/月级 LLM 生成，日级为空
    #[serde(default)]
    pub key_entities: Vec<String>,

    /// 线索锚点（用于检索匹配的关键词）
    ///
    /// 月级 LLM 生成，日级/周级为空
    #[serde(default)]
    pub clue_anchors: Vec<String>,
}

impl Summary {
    /// 创建仅含标题的摘要（日级启发式用）
    pub fn from_title(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            abstract_text: None,
            key_facts: Vec::new(),
            key_entities: Vec::new(),
            clue_anchors: Vec::new(),
        }
    }

    /// 判断是否为高级摘要（含 abstract 或 key_facts）
    pub fn is_rich(&self) -> bool {
        self.abstract_text.is_some() || !self.key_facts.is_empty()
    }
}

/// 记忆迭代更新（借鉴 QwenLong-L1.5 阶段 2：细化/扩展/修正）
///
/// 用于 [`crate::storage::Storage::update_memory`] 方法，
/// 支持记忆随新信息迭代更新，而非静态归档。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryUpdate {
    /// 新增的事实（细化记忆）
    #[serde(default)]
    pub added_facts: Vec<String>,

    /// 修正的事实（扩展/修正已有记忆）
    #[serde(default)]
    pub revised_facts: Vec<String>,

    /// 标记为过时的事实（不再有效）
    #[serde(default)]
    pub deprecated_facts: Vec<String>,
}

impl MemoryUpdate {
    /// 创建空的更新
    pub fn new() -> Self {
        Self::default()
    }

    /// 判断是否为空更新
    pub fn is_empty(&self) -> bool {
        self.added_facts.is_empty() && self.revised_facts.is_empty() && self.deprecated_facts.is_empty()
    }

    /// 添加一条新事实
    pub fn add_fact(mut self, fact: impl Into<String>) -> Self {
        self.added_facts.push(fact.into());
        self
    }

    /// 修正一条事实
    pub fn revise_fact(mut self, fact: impl Into<String>) -> Self {
        self.revised_facts.push(fact.into());
        self
    }

    /// 标记一条事实为过时
    pub fn deprecate_fact(mut self, fact: impl Into<String>) -> Self {
        self.deprecated_facts.push(fact.into());
        self
    }
}

/// 记忆更新记录（带时间戳）
///
/// 包装 [`MemoryUpdate`] + 更新时间，用于 [`MemoryFile::updates`] 字段。
///
/// 设计目的：将迭代更新历史独立存储，不污染原始 `turns` 内容。
/// 多次 PATCH 同一 memory 时，updates 追加新记录，便于追溯演进过程。
///
/// ## v2.6 批次 8：冲突检测
///
/// 新增 `conflicts` 字段记录本次更新时检测到的冲突（[`crate::conflict::ConflictRecord`]）。
/// 通过 `#[serde(default)]` 确保旧文件（无此字段）能正常反序列化为空 Vec。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryUpdateRecord {
    /// 更新时间戳
    pub updated_at: DateTime<Utc>,
    /// 更新内容（added/revised/deprecated facts）
    #[serde(flatten)]
    pub update: MemoryUpdate,
    /// 本次更新检测到的冲突记录（v2.6 批次 8）
    ///
    /// 由 [`crate::conflict::ConflictDetector`] 在 update 前同步检测生成，
    /// 随更新记录一起持久化。旧文件无此字段时默认为空（向后兼容）。
    #[serde(default)]
    pub conflicts: Vec<crate::conflict::ConflictRecord>,
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

// ============================================================================
// 辅助方法
// ============================================================================

impl std::fmt::Display for Tag {
    /// 中文输出，用于 system prompt 渲染
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text => write!(f, "文本消息"),
            Self::FileAttachment => write!(f, "文件附件"),
            Self::Image => write!(f, "图片"),
            Self::Video => write!(f, "视频"),
            Self::ToolCall => write!(f, "工具调用"),
            Self::Thinking => write!(f, "思考过程"),
            Self::SessionId => write!(f, "会话ID"),
            Self::ProjectId => write!(f, "项目ID"),
            Self::Url => write!(f, "URL"),
            Self::Citation => write!(f, "引用"),
            Self::Status => write!(f, "状态"),
            Self::Ui => write!(f, "UI"),
            Self::CodeBlock => write!(f, "代码块"),
            Self::Voice => write!(f, "语音"),
            Self::Plan => write!(f, "计划"),
            Self::AgentTool => write!(f, "Agent工具"),
            Self::Other(s) => write!(f, "{}", s),
        }
    }
}

impl ArchivePeriod {
    /// 返回对应目录名（用于文件树路径生成）
    pub fn as_dir_name(&self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
        }
    }

    /// 返回字符串标识（用于 SQLite 存储等场景）
    pub fn as_str(&self) -> &'static str {
        self.as_dir_name()
    }

    /// 从字符串解析（与 [`as_str`](Self::as_str) 互逆）
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "daily" => Some(Self::Daily),
            "weekly" => Some(Self::Weekly),
            "monthly" => Some(Self::Monthly),
            _ => None,
        }
    }

    /// 返回所有变体（用于遍历）
    pub fn all() -> [Self; 3] {
        [Self::Daily, Self::Weekly, Self::Monthly]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// v2.30.1：验证最简调用——Agent 只传 user_message/llm_message
    /// serde 层：id/timestamp 自动补全，tags 为空 Vec，token_count 为 0
    /// apply_turn_defaults 层：tags 推断为 [Text]，token_count 估算
    #[test]
    fn test_minimal_message_turn_deserialize() {
        let json = r#"{"user_message":{"text":"用户问"},"llm_message":{"text":"AI答"}}"#;
        let mut turn: MessageTurn = serde_json::from_str(json).unwrap();

        // serde 层：id 应自动生成（非全 0 UUID）
        assert_ne!(turn.id, Uuid::nil(), "id 应被自动补全为非 nil UUID");

        // serde 层：timestamp 应为近期时间（非 1970）
        let now = Utc::now();
        let diff = now.signed_duration_since(turn.timestamp);
        assert!(
            diff.num_seconds().abs() < 60,
            "timestamp 应为当前时间附近，实际差值: {}s",
            diff.num_seconds()
        );

        // serde 层：tags 应为空 Vec（等待 apply_turn_defaults 推断）
        assert!(turn.tags.is_empty(), "serde 层 tags 应为空 Vec");

        // serde 层：token_count 应为 0（等待 apply_turn_defaults 估算）
        assert_eq!(turn.token_count, 0, "serde 层 token_count 应为 0");

        // 应用自动补全
        apply_turn_defaults(&mut turn);

        // apply_turn_defaults 后：tags 应为 [Text]（纯文本对话）
        assert_eq!(turn.tags.len(), 1, "应用后 tags 应为 [Text]");
        assert_eq!(turn.tags[0], Tag::Text);

        // apply_turn_defaults 后：token_count 应 > 0（估算值）
        assert!(turn.token_count > 0, "应用后 token_count 应 > 0");
    }

    /// v2.30.1：验证完整调用仍然兼容（向后兼容性）
    /// Agent 传了完整字段，apply_turn_defaults 不应覆盖
    #[test]
    fn test_full_message_turn_deserialize_backward_compat() {
        let json = r#"{
            "id":"7f9c1b2a-3d4e-4f5a-8a9b-0c1d2e3f4a5b",
            "user_message":{"text":"用户消息","attachments":[],"tool_calls":[],"thinking":null},
            "llm_message":{"text":"LLM 回复","attachments":[],"tool_calls":[],"thinking":null},
            "tags":[{"kind":"Text"}],
            "timestamp":"2026-07-05T00:00:00Z",
            "token_count":100
        }"#;
        let mut turn: MessageTurn = serde_json::from_str(json).unwrap();

        // 应使用传入的 id，不覆盖
        assert_eq!(
            turn.id.to_string(),
            "7f9c1b2a-3d4e-4f5a-8a9b-0c1d2e3f4a5b"
        );
        // 应使用传入的 token_count
        assert_eq!(turn.token_count, 100);

        // 应用 apply_turn_defaults —— 不应覆盖已传入的值
        apply_turn_defaults(&mut turn);

        // tags 不应被覆盖（Agent 传了 [Text]，保持）
        assert_eq!(turn.tags.len(), 1);
        assert_eq!(turn.tags[0], Tag::Text);
        // token_count 不应被覆盖（Agent 传了 100，保持）
        assert_eq!(turn.token_count, 100);
    }

    /// v2.30.1：验证缺 user_message 应报错（必填字段保护）
    #[test]
    fn test_missing_user_message_should_fail() {
        let json = r#"{"llm_message":{"text":"AI答"}}"#;
        let result: Result<MessageTurn, _> = serde_json::from_str(json);
        assert!(result.is_err(), "缺 user_message 时应反序列化失败");
    }

    /// v2.30.1：验证工具调用记录的 tags 自动推断
    /// Agent 未传 tags 时，应自动推断为 [ToolCall, AgentTool]
    #[test]
    fn test_infer_tags_tool_call() {
        let json = r#"{
            "user_message":{"text":"请帮我搜索资料"},
            "llm_message":{
                "text":"我帮你搜索了",
                "tool_calls":[{"name":"WebSearch","arguments":"{\"q\":\"rust\"}","result":"[]","duration_ms":100}]
            }
        }"#;
        let mut turn: MessageTurn = serde_json::from_str(json).unwrap();
        assert!(turn.tags.is_empty(), "未传 tags 时 serde 层应为空");

        apply_turn_defaults(&mut turn);

        // 应推断出 ToolCall + AgentTool（不含 Text）
        assert!(turn.tags.contains(&Tag::ToolCall), "应含 ToolCall");
        assert!(turn.tags.contains(&Tag::AgentTool), "应含 AgentTool");
        assert!(
            !turn.tags.contains(&Tag::Text),
            "有工具调用时不应兜底 Text"
        );

        // token_count 应 > 0（估算）
        assert!(turn.token_count > 0);
    }

    /// v2.30.1：验证代码块的 tags 自动推断
    #[test]
    fn test_infer_tags_code_block() {
        let json = r#"{
            "user_message":{"text":"写个函数"},
            "llm_message":{"text":"```rust\nfn hello() {}\n```"}
        }"#;
        let mut turn: MessageTurn = serde_json::from_str(json).unwrap();
        apply_turn_defaults(&mut turn);

        assert!(turn.tags.contains(&Tag::CodeBlock), "应含 CodeBlock");
    }

    /// v2.30.1：验证 URL 的 tags 自动推断
    #[test]
    fn test_infer_tags_url() {
        let json = r#"{
            "user_message":{"text":"看看这个 https://example.com"},
            "llm_message":{"text":"好的"}
        }"#;
        let mut turn: MessageTurn = serde_json::from_str(json).unwrap();
        apply_turn_defaults(&mut turn);

        assert!(turn.tags.contains(&Tag::Url), "应含 Url");
    }

    /// v2.30.1：验证思考过程的 tags 自动推断
    #[test]
    fn test_infer_tags_thinking() {
        let json = r#"{
            "user_message":{"text":"复杂问题"},
            "llm_message":{"text":"答案是...","thinking":"让我想想..."}
        }"#;
        let mut turn: MessageTurn = serde_json::from_str(json).unwrap();
        apply_turn_defaults(&mut turn);

        assert!(turn.tags.contains(&Tag::Thinking), "应含 Thinking");
    }

    /// v2.30.1：验证 token_count 估算（Agent 传了不覆盖）
    #[test]
    fn test_token_count_not_overwritten() {
        let json = r#"{
            "user_message":{"text":"短文本"},
            "llm_message":{"text":"回复"},
            "token_count":999
        }"#;
        let mut turn: MessageTurn = serde_json::from_str(json).unwrap();
        apply_turn_defaults(&mut turn);

        // Agent 传了 999，不应被估算值覆盖
        assert_eq!(turn.token_count, 999);
    }
}

impl MemoryFile {
    /// 创建新的记忆文件
    ///
    /// - 自动计算 `total_tokens`（所有轮次 token_count 之和）
    /// - 自动计算 `tags`（所有轮次标签的并集，去重）
    /// - `schema_version` 设为当前版本
    /// - `truncated` 默认 false
    pub fn new(
        session_id: impl Into<String>,
        project_id: Option<String>,
        turns: Vec<MessageTurn>,
        period: ArchivePeriod,
    ) -> Self {
        use std::collections::HashSet;

        let total_tokens = turns.iter().map(|t| t.token_count).sum();
        let tags: Vec<Tag> = {
            let mut seen: HashSet<Tag> = turns.iter().flat_map(|t| t.tags.iter().cloned()).collect();
            seen.drain().collect()
        };

        Self {
            id: Uuid::new_v4(),
            schema_version: SCHEMA_VERSION,
            archived_at: Utc::now(),
            session_id: session_id.into(),
            project_id,
            turns,
            tags,
            total_tokens,
            truncated: false,
            period,
            access_count: 0,
            importance: 0,
            updates: Vec::new(),
        }
    }

    /// 标记为强制截断
    pub fn mark_truncated(&mut self) {
        self.truncated = true;
    }

    /// 增加访问计数（用于评分）
    pub fn record_access(&mut self) {
        self.access_count = self.access_count.saturating_add(1);
    }

    /// 设置用户显式重要性（0-100）
    ///
    /// 超过 100 会被截断为 100
    pub fn set_importance(&mut self, importance: u8) {
        self.importance = importance.min(100);
    }
}

impl IndexHook {
    /// 从记忆文件生成索引钩子
    ///
    /// `summary` 在 P1 阶段采用启发式：取首个轮次的用户文本前 80 字符作为 title
    /// （P2 阶段接入 LLM 后可优化为结构化摘要）
    ///
    /// `memory_id` 参数：
    /// - LocalStorage 后端：传入文件相对路径（POSIX 分隔符）
    /// - SQLite 后端：传入 UUID 字符串
    pub fn from_memory_file(file: &MemoryFile, memory_id: String) -> Self {
        let title = file
            .turns
            .first()
            .and_then(|t| t.user_message.text.as_ref())
            .map(|text| {
                // 截取前 80 字符作为摘要标题（按字符边界，避免截断 UTF-8）
                let chars: Vec<char> = text.chars().take(80).collect();
                let mut s: String = chars.into_iter().collect();
                s.push_str("...");
                s
            })
            .unwrap_or_else(|| format!("记忆文件 {}", file.id));

        Self {
            id: Uuid::new_v4(),
            memory_id,
            summary: Summary::from_title(title),
            tags: file.tags.clone(),
            archived_at: file.archived_at,
            period: file.period,
            token_count: file.total_tokens,
            file_status: FileStatus::Normal,
        }
    }
}

impl IndexDocument {
    /// 创建新的空索引文档
    pub fn new(
        session_id: impl Into<String>,
        project_id: Option<String>,
        period: ArchivePeriod,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            schema_version: SCHEMA_VERSION,
            session_id: session_id.into(),
            project_id,
            hooks: Vec::new(),
            updated_at: Utc::now(),
            period,
        }
    }

    /// 追加一个钩子，并更新 `updated_at`
    pub fn add_hook(&mut self, hook: IndexHook) {
        self.hooks.push(hook);
        self.updated_at = Utc::now();
    }

    /// 按 ID 移除钩子
    pub fn remove_hook(&mut self, hook_id: Uuid) -> Option<IndexHook> {
        if let Some(pos) = self.hooks.iter().position(|h| h.id == hook_id) {
            self.updated_at = Utc::now();
            Some(self.hooks.remove(pos))
        } else {
            None
        }
    }

    /// 按记忆文件 ID 查找钩子
    ///
    /// `memory_id` 在 LocalStorage 后端为路径字符串，在 SQLite 后端为 UUID 字符串
    pub fn find_by_memory(&self, memory_id: &str) -> Option<&IndexHook> {
        self.hooks.iter().find(|h| h.memory_id == memory_id)
    }
}
