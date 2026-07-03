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
