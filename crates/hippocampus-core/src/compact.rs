//! # 周期任务模块
//!
//! 实现三级索引周期任务：
//!
//! - **天级**：持续归档（由 [`archive`] 模块处理）
//! - **周级**：无损去重合并（7 个天级文件 → 1 个周级文件）
//! - **月级**：评分淘汰（4 个周级文件 → 1 个主记忆文件 + 高价值片段）
//!
//! ## 周级合并（寒暄/元信息剥离）
//!
//! 7 个天级记忆文件合并为 1 个周级文件：
//! - **寒暄剥离**：去除「你好」「谢谢」「好的」等无信息量 turn
//! - **元信息剥离**：去除纯 UI 状态/状态切换类 turn（无实质内容）
//! - 保留所有实质内容原样拼接（**非摘要**）
//! - 索引文档同步合并（钩子去重 + 合并）
//!
//! ## 月级评分淘汰（Turn 级高价值片段保留）
//!
//! 4 个周级记忆文件按 3 维加权评分（[`crate::score::DefaultScorer`]）：
//! - 选最高分的周级文件作为**主记忆**
//! - 其余 3 个周级文件按分数从高到低排序
//! - 从次高分的周级文件中挑选**高价值 Turn**（importance > 50 或含 ToolCall/Thinking 标签）
//! - 将高价值 Turn 追加到主记忆文件
//! - 索引文档同步合并
//!
//! ## 架构
//!
//! [`Compactor`] 持有 `Arc<dyn Storage>`，与 [`Archiver`] / [`Retriever`] 一致。
//! 构造时绑定 session_id 和 project_id，全封装周期任务。

use crate::model::{ArchivePeriod, IndexDocument, MemoryFile, MessageTurn, Summary, Tag};
use crate::score::{AsyncScorer, Scorer};
use crate::storage::Storage;
use std::sync::Arc;

/// 寒暄/无信息量文本列表（小写匹配，内置默认词典）
const CHITCHAT_PATTERNS: &[&str] = &[
    "你好", "您好", "早上好", "下午好", "晚上好", "嗨", "哈喽", "hi", "hello",
    "谢谢", "感谢", "thanks", "thank you", "多谢",
    "好的", "好的,", "好的。", "ok", "okay", "嗯", "嗯嗯", "嗯哼",
    "再见", "拜拜", "bye",
    "收到", "明白", "了解", "清楚了", "知道了",
    "是的", "对的", "没错",
];

/// 周期任务执行器
///
/// 持有 [`Storage`] 引用，全封装周级合并和月级淘汰流程。
pub struct Compactor {
    /// 存储后端
    storage: Arc<dyn Storage>,
    /// 评分器（月级淘汰用，同步启发式）
    scorer: Box<dyn Scorer>,
    /// 会话 ID
    session_id: String,
    /// 项目 ID（可选）
    project_id: Option<String>,
    /// 寒暄词典（v2.16 IMP-04：可注入自定义词典）
    ///
    /// 为空时使用内置 `CHITCHAT_PATTERNS`，非空时与内置词典合并匹配。
    /// 通过 [`Compactor::with_chitchat_patterns`] 注入。
    chitchat_patterns: Vec<String>,
    /// 异步评分器（v2.16 IMP-03：可选 LLM 评分注入）
    ///
    /// 注入后在 `monthly_evict` 中优先使用，支持 LLM topic_relevance 维度。
    /// 为 None 时退化为同步 `scorer`（纯启发式 3 维）。
    /// 推荐注入 [`crate::score::HybridScorer`] 以组合启发式 + LLM 评分。
    async_scorer: Option<Arc<dyn AsyncScorer>>,
}

/// 寒暄判定核心逻辑（自由函数，供测试直接调用）
///
/// 判定规则（满足任一即视为寒暄）：
/// 1. user_message.text 去空格后长度 ≤ 10，且匹配寒暄模式（内置 + 自定义）
/// 2. user_message 和 llm_message 都无 text，且都无 attachments/tool_calls/thinking
/// 3. user_message.text 去空格后长度 ≤ 3（如「嗯」「哦」「好」）
///
/// v2.16 IMP-04：`custom_patterns` 与内置 `CHITCHAT_PATTERNS` 合并匹配（非替换）。
fn is_chitchat_with_patterns(turn: &MessageTurn, custom_patterns: &[String]) -> bool {
    // 规则 2：完全空内容的 turn
    let user_empty = turn.user_message.text.is_none()
        && turn.user_message.attachments.is_empty()
        && turn.user_message.tool_calls.is_empty()
        && turn.user_message.thinking.is_none();
    let llm_empty = turn.llm_message.text.is_none()
        && turn.llm_message.attachments.is_empty()
        && turn.llm_message.tool_calls.is_empty()
        && turn.llm_message.thinking.is_none();
    if user_empty && llm_empty {
        return true;
    }

    // 规则 3：极短用户消息（≤3 字符）
    if let Some(text) = &turn.user_message.text {
        let trimmed = text.trim();
        if trimmed.chars().count() <= 3 {
            return true;
        }
    }

    // 规则 1：匹配寒暄模式（内置 + 自定义）
    if let Some(text) = &turn.user_message.text {
        let lower = text.trim().to_lowercase();
        if lower.chars().count() <= 10 {
            // 先匹配内置词典
            for pattern in CHITCHAT_PATTERNS {
                if lower == *pattern || lower.starts_with(pattern) {
                    return true;
                }
            }
            // 再匹配自定义词典
            for pattern in custom_patterns {
                let p = pattern.trim().to_lowercase();
                if lower == p || lower.starts_with(&p) {
                    return true;
                }
            }
        }
    }

    false
}

impl Compactor {
    /// 创建新的周期任务执行器
    ///
    /// - `storage`：存储后端
    /// - `scorer`：评分器（月级淘汰用，可用 [`crate::score::DefaultScorer`]）
    /// - `session_id`：当前会话 ID
    /// - `project_id`：项目 ID（可选）
    pub fn new(
        storage: Arc<dyn Storage>,
        scorer: Box<dyn Scorer>,
        session_id: impl Into<String>,
        project_id: Option<String>,
    ) -> Self {
        Self {
            storage,
            scorer,
            session_id: session_id.into(),
            project_id,
            chitchat_patterns: Vec::new(),
            async_scorer: None,
        }
    }

    /// 注入自定义寒暄词典（v2.16 IMP-04）
    ///
    /// 传入的词典会与内置 `CHITCHAT_PATTERNS` **合并**使用（非替换），
    /// 用于扩展寒暄剥离规则。
    ///
    /// # 示例
    ///
    /// ```ignore
    /// let compactor = Compactor::new(storage, scorer, "sess", None)
    ///     .with_chitchat_patterns(vec!["辛苦了".into(), "麻烦了".into()]);
    /// ```
    pub fn with_chitchat_patterns(mut self, patterns: Vec<String>) -> Self {
        self.chitchat_patterns = patterns;
        self
    }

    /// 注入异步评分器（v2.16 IMP-03）
    ///
    /// 注入后 `monthly_evict` 将优先使用此异步评分器（支持 LLM topic_relevance 维度）。
    /// 异步评分失败时降级为同步启发式评分。
    ///
    /// 推荐注入 [`crate::score::HybridScorer`] 以组合启发式 3 维 + LLM 1 维评分。
    ///
    /// # 示例
    ///
    /// ```ignore
    /// use hippocampus_core::score::{HybridScorer, ScoreWeights};
    /// let hybrid = HybridScorer::new(Box::new(http_llm_scorer), ScoreWeights::default());
    /// let compactor = Compactor::new(storage, scorer, "sess", None)
    ///     .with_async_scorer(Arc::new(hybrid));
    /// ```
    pub fn with_async_scorer(mut self, scorer: Arc<dyn AsyncScorer>) -> Self {
        self.async_scorer = Some(scorer);
        self
    }

    /// 判断 turn 是否为寒暄/无信息量（实例方法，注入自定义词典）
    ///
    /// 委托给自由函数 [`is_chitchat_with_patterns`]，合并内置词典与 `self.chitchat_patterns`。
    fn is_chitchat(&self, turn: &MessageTurn) -> bool {
        is_chitchat_with_patterns(turn, &self.chitchat_patterns)
    }

    /// 周级合并：7 个天级文件无损去重合并为 1 个周级文件
    ///
    /// 流程：
    /// 1. 读取所有 daily 记忆文件
    /// 2. 逐文件过滤寒暄/无信息量 turn
    /// 3. 合并所有 turns 到一个新的 MemoryFile（period=Weekly）
    /// 4. 写入 Storage 得到 weekly 路径
    /// 5. 合并所有 daily 索引文档的钩子到 weekly 索引
    /// 6. 返回合并后的 MemoryFile 和合并后的 IndexDocument
    ///
    /// **注意**：调用方应确保传入的 daily 文件确实属于当前周。
    /// 原始 daily 文件不会被删除（保留供回溯），调用方可根据策略决定是否清理。
    pub async fn weekly_merge(&self) -> crate::Result<(MemoryFile, IndexDocument)> {
        // 1. 读取所有 daily 记忆文件路径
        let daily_paths = self
            .storage
            .list_memories(&self.session_id, self.project_id.as_deref(), ArchivePeriod::Daily)
            .await?;

        if daily_paths.is_empty() {
            return Err(crate::Error::Storage(format!(
                "周级合并失败: 会话 {} 无 daily 记忆文件",
                self.session_id
            )));
        }

        // 2. 读取所有 daily MemoryFile，过滤寒暄 turn，合并 turns
        let mut all_turns = Vec::new();
        let mut daily_files = Vec::new();
        let mut removed_count = 0usize;

        for path in &daily_paths {
            let file = self.storage.read_memory(path).await?;
            for turn in &file.turns {
                if self.is_chitchat(turn) {
                    removed_count += 1;
                } else {
                    all_turns.push(turn.clone());
                }
            }
            daily_files.push(file);
        }

        tracing::info!(
            session_id = %self.session_id,
            daily_count = daily_paths.len(),
            total_turns_before = all_turns.len() + removed_count,
            removed_chitchat = removed_count,
            remaining_turns = all_turns.len(),
            "周级合并: 已剥离寒暄/无信息量 turn"
        );

        if all_turns.is_empty() {
            return Err(crate::Error::Storage(format!(
                "周级合并失败: 会话 {} 所有 turn 均为寒暄/无信息量",
                self.session_id
            )));
        }

        // 3. 生成合并后的 MemoryFile（Weekly）
        let merged_memory = MemoryFile::new(
            self.session_id.clone(),
            self.project_id.clone(),
            all_turns,
            ArchivePeriod::Weekly,
        );

        // 4. 写入 Storage
        let memory_path = self.storage.write_memory(&merged_memory).await?;

        // 5. 合并所有 daily 索引文档的钩子到 weekly 索引
        // 读取 daily 索引文档
        let daily_index = self
            .storage
            .read_index(&self.session_id, self.project_id.as_deref(), ArchivePeriod::Daily)
            .await?;

        // 创建 weekly 索引文档
        let mut weekly_index =
            IndexDocument::new(self.session_id.clone(), self.project_id.clone(), ArchivePeriod::Weekly);

        if let Some(daily_doc) = daily_index {
            // v2.4: 为 weekly 钩子生成 richer Summary（启发式）
            // 合并所有 daily 钩子的标题作为 abstract_text
            // 提取所有 daily 钩子的标签作为 key_entities
            let daily_titles: Vec<String> = daily_doc
                .hooks
                .iter()
                .map(|h| h.summary.title.clone())
                .collect();
            let abstract_text = if daily_titles.is_empty() {
                None
            } else {
                Some(format!("本周合并了 {} 个日级记忆：{}", daily_titles.len(), daily_titles.join("；")))
            };

            // key_entities：从标签中提取实体（去重）
            let mut entities: Vec<String> = Vec::new();
            for hook in &daily_doc.hooks {
                for tag in &hook.tags {
                    let tag_str = tag.to_string();
                    if !entities.contains(&tag_str) {
                        entities.push(tag_str);
                    }
                }
            }

            // key_facts：从 daily 钩子标题中提取（每个标题作为一条事实）
            let key_facts: Vec<String> = daily_doc
                .hooks
                .iter()
                .map(|h| h.summary.title.clone())
                .collect();

            for hook in &daily_doc.hooks {
                // 钩子重新生成，指向新的 weekly 记忆文件
                let mut new_hook = hook.clone();
                new_hook.memory_id = memory_path.clone();
                new_hook.period = ArchivePeriod::Weekly;

                // v2.4: 升级 Summary 为 richer 版本（启发式）
                new_hook.summary = Summary {
                    title: format!("周度合并（{} 个记忆）", daily_doc.hooks.len()),
                    abstract_text: abstract_text.clone(),
                    key_facts: key_facts.clone(),
                    key_entities: entities.clone(),
                    clue_anchors: Vec::new(), // 月级才有
                };

                weekly_index.add_hook(new_hook);
            }
        }

        // 写入 weekly 索引
        self.storage.write_index(&weekly_index).await?;

        tracing::info!(
            memory_id = %merged_memory.id,
            total_turns = merged_memory.turns.len(),
            total_tokens = merged_memory.total_tokens,
            hooks = weekly_index.hooks.len(),
            "周级合并完成: 已写入 weekly 记忆文件和索引"
        );

        Ok((merged_memory, weekly_index))
    }

    /// 月级评分淘汰：4 个周级文件 → 1 个主记忆 + 高价值 Turn
    ///
    /// 流程：
    /// 1. 读取所有 weekly 记忆文件
    /// 2. 用 Scorer 对每个 weekly 文件评分
    /// 3. 选最高分的作为主记忆
    /// 4. 其余按分数从高到低排序
    /// 5. 从次高分的文件中挑选高价值 Turn：
    ///    - importance > 50 的 turn
    ///    - 或含 ToolCall / Thinking / AgentTool 标签的 turn
    /// 6. 将高价值 Turn 追加到主记忆文件
    /// 7. 写入 Storage 得到 monthly 路径
    /// 8. 合并所有 weekly 索引文档的钩子到 monthly 索引
    /// 9. 返回合并后的 MemoryFile 和 IndexDocument
    pub async fn monthly_evict(&self) -> crate::Result<(MemoryFile, IndexDocument)> {
        // 1. 读取所有 weekly 记忆文件路径
        let weekly_paths = self
            .storage
            .list_memories(&self.session_id, self.project_id.as_deref(), ArchivePeriod::Weekly)
            .await?;

        if weekly_paths.is_empty() {
            return Err(crate::Error::Storage(format!(
                "月级淘汰失败: 会话 {} 无 weekly 记忆文件",
                self.session_id
            )));
        }

        // 2. 读取所有 weekly MemoryFile 并评分
        let mut weekly_files: Vec<MemoryFile> = Vec::new();
        for path in &weekly_paths {
            let file = self.storage.read_memory(path).await?;
            weekly_files.push(file);
        }

        // 评分并按分数从高到低排序
        // v2.16 IMP-03：若注入了 async_scorer，优先使用异步评分（支持 LLM topic_relevance 维）
        let mut scored: Vec<(MemoryFile, f64)> = Vec::with_capacity(weekly_files.len());
        for f in weekly_files {
            let score = if let Some(async_scorer) = &self.async_scorer {
                match async_scorer.score(&f).await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            memory_id = %f.id,
                            "异步评分失败，降级为同步启发式评分"
                        );
                        self.scorer.score(&f)
                    }
                }
            } else {
                self.scorer.score(&f)
            };
            scored.push((f, score));
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        tracing::info!(
            session_id = %self.session_id,
            weekly_count = scored.len(),
            scores = ?scored.iter().map(|(_, s)| *s).collect::<Vec<_>>(),
            "月级淘汰: 周级文件评分完成"
        );

        // 3. 选最高分的作为主记忆
        let (mut main_memory, main_score) = scored.remove(0);
        let main_id = main_memory.id;

        tracing::info!(
            main_memory_id = %main_id,
            main_score = main_score,
            "月级淘汰: 选定主记忆文件"
        );

        // 4-5. 从其余文件中挑选高价值 Turn
        let mut high_value_turns = Vec::new();
        for (file, score) in &scored {
            for turn in &file.turns {
                if Self::is_high_value_turn(turn) {
                    high_value_turns.push(turn.clone());
                }
            }
            tracing::info!(
                file_id = %file.id,
                score = score,
                "月级淘汰: 已扫描高价值 Turn"
            );
        }

        // 6. 追加高价值 Turn 到主记忆
        let added_count = high_value_turns.len();
        for turn in high_value_turns {
            main_memory.turns.push(turn);
        }
        // 重新计算 total_tokens 和 tags 并集（手动去重，Tag 含 Other(String) 不支持 Ord）
        main_memory.total_tokens = main_memory.turns.iter().map(|t| t.token_count).sum();
        let mut all_tags: Vec<Tag> = Vec::new();
        for turn in &main_memory.turns {
            for tag in &turn.tags {
                if !all_tags.contains(tag) {
                    all_tags.push(tag.clone());
                }
            }
        }
        main_memory.tags = all_tags;
        main_memory.period = ArchivePeriod::Monthly;

        // 7. 写入 Storage
        let memory_path = self.storage.write_memory(&main_memory).await?;

        // 8. 合并所有 weekly 索引文档的钩子到 monthly 索引
        let weekly_index = self
            .storage
            .read_index(&self.session_id, self.project_id.as_deref(), ArchivePeriod::Weekly)
            .await?;

        let mut monthly_index = IndexDocument::new(
            self.session_id.clone(),
            self.project_id.clone(),
            ArchivePeriod::Monthly,
        );

        if let Some(weekly_doc) = weekly_index {
            for hook in &weekly_doc.hooks {
                // 钩子重新生成，指向新的 monthly 记忆文件
                let mut new_hook = hook.clone();
                new_hook.memory_id = memory_path.clone();
                new_hook.period = ArchivePeriod::Monthly;
                monthly_index.add_hook(new_hook);
            }
        }

        // 写入 monthly 索引
        self.storage.write_index(&monthly_index).await?;

        tracing::info!(
            memory_id = %main_memory.id,
            total_turns = main_memory.turns.len(),
            total_tokens = main_memory.total_tokens,
            high_value_added = added_count,
            hooks = monthly_index.hooks.len(),
            "月级淘汰完成: 已写入 monthly 记忆文件和索引"
        );

        Ok((main_memory, monthly_index))
    }

    /// 判断 turn 是否为高价值（月级淘汰保留标准）
    ///
    /// 满足任一条件即视为高价值：
    /// 1. 含 ToolCall 标签（工具调用信息）
    /// 2. 含 Thinking 标签（思考过程）
    /// 3. 含 AgentTool 标签（Agent 工具使用记录）
    /// 4. 含 CodeBlock 标签（代码块，技术信息）
    /// 5. 含 FileAttachment / Image / Video 标签（附件信息）
    fn is_high_value_turn(turn: &MessageTurn) -> bool {
        // 检查标签
        let valuable_tags = [
            Tag::ToolCall,
            Tag::Thinking,
            Tag::AgentTool,
            Tag::CodeBlock,
            Tag::FileAttachment,
            Tag::Image,
            Tag::Video,
        ];
        for tag in &turn.tags {
            if valuable_tags.contains(tag) {
                return true;
            }
        }
        false
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MessageContent, Tag};
    use crate::score::DefaultScorer;
    use crate::storage::LocalStorage;
    use chrono::Utc;
    use tempfile::TempDir;
    use uuid::Uuid;

    /// 构造测试用 MessageTurn
    fn make_turn(user_text: &str, llm_text: &str, token_count: usize, tags: Vec<Tag>) -> MessageTurn {
        MessageTurn {
            id: Uuid::new_v4(),
            user_message: MessageContent {
                text: Some(user_text.into()),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                thinking: None,
            },
            llm_message: MessageContent {
                text: Some(llm_text.into()),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                thinking: None,
            },
            tags,
            timestamp: Utc::now(),
            token_count,
        }
    }

    /// 默认构造的 turn（带 Text + CodeBlock 标签）
    fn make_normal_turn(user_text: &str, token_count: usize) -> MessageTurn {
        make_turn(user_text, "LLM 回复", token_count, vec![Tag::Text, Tag::CodeBlock])
    }

    #[test]
    fn test_is_chitchat_greeting() {
        let turn = make_turn("你好", "你好！有什么可以帮你的吗？", 10, vec![Tag::Text]);
        assert!(is_chitchat_with_patterns(&turn, &[]));
    }

    #[test]
    fn test_is_chitchat_thanks() {
        let turn = make_turn("谢谢", "不客气！", 10, vec![Tag::Text]);
        assert!(is_chitchat_with_patterns(&turn, &[]));
    }

    #[test]
    fn test_is_chitchat_ok() {
        let turn = make_turn("好的", "好的，我开始执行", 10, vec![Tag::Text]);
        assert!(is_chitchat_with_patterns(&turn, &[]));
    }

    #[test]
    fn test_is_chitchat_short_response() {
        // 极短用户消息（≤3 字符）
        let turn = make_turn("嗯", "明白", 5, vec![Tag::Text]);
        assert!(is_chitchat_with_patterns(&turn, &[]));
    }

    #[test]
    fn test_is_chitchat_empty_content() {
        let mut turn = make_turn("hello", "hi", 5, vec![Tag::Text]);
        turn.user_message.text = None;
        turn.llm_message.text = None;
        assert!(is_chitchat_with_patterns(&turn, &[]));
    }

    #[test]
    fn test_is_chitchat_normal_content() {
        let turn = make_turn(
            "帮我设计一个 Rust 记忆库的架构",
            "好的，我建议采用三层架构...",
            100,
            vec![Tag::Text, Tag::CodeBlock],
        );
        assert!(!is_chitchat_with_patterns(&turn, &[]));
    }

    #[test]
    fn test_is_chitchat_with_tool_call() {
        // 含工具调用标签的 turn 不应被视为寒暄
        let turn = make_turn("好的", "执行搜索", 20, vec![Tag::Text, Tag::ToolCall]);
        // 虽然用户消息是「好的」，但 llm 含工具调用
        // 注意：寒暄判定只看 user_message.text 的匹配
        // 但规则 3（极短用户消息 ≤3 字符）会触发
        assert!(is_chitchat_with_patterns(&turn, &[])); // 「好的」是 2 字符，触发规则 3
    }

    #[test]
    fn test_is_chitchat_with_custom_patterns() {
        // v2.16 IMP-04：自定义词典扩展测试
        // 注意：测试词需 >3 字符，避免触发规则 3（极短用户消息 ≤3 字符）
        let turn = make_turn("辛苦你了", "应该的", 10, vec![Tag::Text]);
        // 内置词典不含「辛苦你了」，且为 4 字符不触发规则 3，默认不匹配
        assert!(!is_chitchat_with_patterns(&turn, &[]));
        // 注入自定义词典后匹配
        assert!(is_chitchat_with_patterns(&turn, &["辛苦你了".into()]));
    }

    #[test]
    fn test_is_high_value_turn_with_tool_call() {
        let turn = make_turn("查询", "调用搜索工具", 30, vec![Tag::Text, Tag::ToolCall]);
        assert!(Compactor::is_high_value_turn(&turn));
    }

    #[test]
    fn test_is_high_value_turn_with_thinking() {
        let turn = make_turn("思考", "推理过程", 30, vec![Tag::Text, Tag::Thinking]);
        assert!(Compactor::is_high_value_turn(&turn));
    }

    #[test]
    fn test_is_high_value_turn_with_code() {
        let turn = make_turn("实现", "代码", 30, vec![Tag::Text, Tag::CodeBlock]);
        assert!(Compactor::is_high_value_turn(&turn));
    }

    #[test]
    fn test_is_high_value_turn_normal() {
        let turn = make_turn("普通对话", "回复", 30, vec![Tag::Text]);
        assert!(!Compactor::is_high_value_turn(&turn));
    }

    #[tokio::test]
    async fn test_weekly_merge_strips_chitchat() {
        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));

        // 通过 Archiver 归档一个含寒暄的 daily 文件
        use crate::archive::Archiver;
        use crate::model::ArchiveConfig;
        let mut archiver = Archiver::new(
            ArchiveConfig {
                token_threshold: 100,
                force_truncate_limit: 150,
                wait_for_turn_completion: true,
            },
            storage.clone(),
            "sess-weekly-1",
            None,
        );

        // 推入混合 turn（含寒暄）
        archiver.push_turn(make_turn("你好", "你好！", 10, vec![Tag::Text]));
        archiver.push_turn(make_normal_turn("设计架构", 60));
        archiver.push_turn(make_turn("谢谢", "不客气", 10, vec![Tag::Text]));
        archiver.push_turn(make_normal_turn("实现 Storage", 50));
        archiver.archive().await.unwrap();

        // 执行周级合并
        let scorer: Box<dyn Scorer> = Box::new(DefaultScorer::new());
        let compactor = Compactor::new(storage.clone(), scorer, "sess-weekly-1", None);
        let (merged, index) = compactor.weekly_merge().await.unwrap();

        // 验证寒暄被剥离
        assert_eq!(merged.turns.len(), 2); // 只剩 2 个 normal turn
        assert!(merged.turns[0].user_message.text.as_ref().unwrap().contains("设计架构"));
        assert!(merged.turns[1].user_message.text.as_ref().unwrap().contains("实现 Storage"));
        assert_eq!(merged.period, ArchivePeriod::Weekly);
        assert_eq!(merged.total_tokens, 110);

        // 验证索引同步合并
        assert_eq!(index.hooks.len(), 1); // daily 索引有 1 个钩子
        assert_eq!(index.period, ArchivePeriod::Weekly);
    }

    #[tokio::test]
    async fn test_weekly_merge_empty_fails() {
        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));
        let scorer: Box<dyn Scorer> = Box::new(DefaultScorer::new());
        let compactor = Compactor::new(storage, scorer, "nonexistent", None);

        let result = compactor.weekly_merge().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_monthly_evict_picks_highest_score() {
        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));

        // 通过 Archiver 归档多个 weekly 文件（直接写入 weekly 目录）
        use crate::archive::Archiver;
        use crate::model::ArchiveConfig;
        let mut archiver = Archiver::new(
            ArchiveConfig {
                token_threshold: 100,
                force_truncate_limit: 150,
                wait_for_turn_completion: true,
            },
            storage.clone(),
            "sess-monthly-1",
            None,
        );

        // 第一次归档（含高价值 turn）
        archiver.push_turn(make_turn(
            "设计架构",
            "开始设计",
            60,
            vec![Tag::Text, Tag::CodeBlock],
        ));
        archiver.push_turn(make_normal_turn("继续", 50));
        archiver.archive().await.unwrap();

        // 第二次归档（含工具调用）
        archiver.push_turn(make_turn(
            "查询资料",
            "调用搜索",
            60,
            vec![Tag::Text, Tag::ToolCall],
        ));
        archiver.push_turn(make_normal_turn("分析", 50));
        archiver.archive().await.unwrap();

        // 先做周级合并
        let scorer: Box<dyn Scorer> = Box::new(DefaultScorer::new());
        let compactor = Compactor::new(storage.clone(), scorer, "sess-monthly-1", None);
        let (_weekly_memory, _) = compactor.weekly_merge().await.unwrap();

        // 手动标记第一个 weekly 文件的 importance 为高
        // 读取 weekly 文件，设置 importance，写回
        let weekly_paths = storage
            .list_memories("sess-monthly-1", None, ArchivePeriod::Weekly)
            .await
            .unwrap();
        assert_eq!(weekly_paths.len(), 1);

        let mut weekly_file = storage.read_memory(&weekly_paths[0]).await.unwrap();
        weekly_file.importance = 80; // 高重要性
        weekly_file.access_count = 5; // 高访问
        // 直接覆盖写入
        storage.write_memory(&weekly_file).await.unwrap();

        // 执行月级淘汰
        let (main_memory, monthly_index) = compactor.monthly_evict().await.unwrap();

        // 验证主记忆文件
        assert_eq!(main_memory.period, ArchivePeriod::Monthly);
        assert!(main_memory.turns.len() >= 2); // 至少保留主记忆的 turns

        // 验证索引同步合并（daily 索引有 2 个钩子，weekly 合并后也有 2 个，monthly 同步迁移）
        assert_eq!(monthly_index.hooks.len(), 2);
        assert_eq!(monthly_index.period, ArchivePeriod::Monthly);
    }

    #[tokio::test]
    async fn test_monthly_evict_no_weekly_fails() {
        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));
        let scorer: Box<dyn Scorer> = Box::new(DefaultScorer::new());
        let compactor = Compactor::new(storage, scorer, "nonexistent", None);

        let result = compactor.monthly_evict().await;
        assert!(result.is_err());
    }
}
