//! # 检索模块
//!
//! 混合检索机制：摘要钩子注入 + tool 主动检索。
//!
//! ## 两种检索模式
//!
//! 1. **摘要钩子注入**：将索引钩子的摘要信息（标题+标签+时间戳）注入到
//!    system prompt，让 LLM 知道"有哪些记忆"，轻量
//! 2. **Tool 主动检索**：LLM 根据需要通过 tool 调用检索详细记忆文件，
//!    返回完整上下文
//!
//! ## 分层钩子设计
//!
//! - [`IndexHook`] 包含完整信息
//! - [`SummaryView`] 是轻量摘要视图（用于注入 system prompt）
//! - 详细检索返回完整 [`MemoryFile`]
//!
//! ## 摘要来源
//!
//! 每次调用 [`Retriever::get_summaries`] 时实时从 [`Storage`] 读取所有周期
//! （daily/weekly/monthly）的索引文档，提取所有钩子转为摘要视图。
//! 保证与存储的一致性。

use crate::model::{ArchivePeriod, IndexHook, MemoryFile};
use crate::storage::Storage;
use std::sync::Arc;

/// 摘要视图（用于注入 system prompt）
///
/// v2.4 升级：暴露完整结构化摘要字段，支持分级渲染。
/// - 日级：仅 title（启发式生成）
/// - 周级：title + abstract_text + key_facts + key_entities（LLM 生成）
/// - 月级：全字段含 clue_anchors（LLM 生成）
#[derive(Debug, Clone, serde::Serialize)]
pub struct SummaryView {
    /// 钩子 ID（UUID 字符串形式）
    pub hook_id: String,
    /// 指向的记忆文件 ID（LocalStorage 为路径，SQLite 为 UUID）
    pub memory_id: String,
    /// 摘要标题（从 IndexHook.summary.title 提取，向后兼容）
    pub summary_title: String,
    /// 抽象摘要（2-3 句话，提炼主题；日级为 None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abstract_text: Option<String>,
    /// 关键事实（事实级别，可被直接引用；日级为空）
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub key_facts: Vec<String>,
    /// 关键实体（人名/项目名/技术名词等；日级为空）
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub key_entities: Vec<String>,
    /// 线索锚点（用于检索匹配的关键词；月级才有）
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub clue_anchors: Vec<String>,
    /// 标签集合（中文显示，通过 Tag Display 转换）
    pub tags: Vec<String>,
    /// 归档时间（RFC3339）
    pub archived_at: String,
    /// 周期层级（daily/weekly/monthly）
    pub period: String,
    /// Token 数
    pub token_count: usize,
    /// 是否为高级摘要（含 abstract 或 key_facts）
    #[serde(skip)]
    pub is_rich: bool,
}

impl From<&IndexHook> for SummaryView {
    fn from(hook: &IndexHook) -> Self {
        Self {
            hook_id: hook.id.to_string(),
            memory_id: hook.memory_id.clone(),
            summary_title: hook.summary.title.clone(),
            abstract_text: hook.summary.abstract_text.clone(),
            key_facts: hook.summary.key_facts.clone(),
            key_entities: hook.summary.key_entities.clone(),
            clue_anchors: hook.summary.clue_anchors.clone(),
            tags: hook.tags.iter().map(|t| t.to_string()).collect(),
            archived_at: hook.archived_at.to_rfc3339(),
            period: hook.period.as_dir_name().to_string(),
            token_count: hook.token_count,
            is_rich: hook.summary.is_rich(),
        }
    }
}

// ========================================================================
// v2.16 批次 1：IMP-06 + IMP-07 相关性评分辅助函数
// ========================================================================

/// 计算摘要视图与查询词的相关性分数
///
/// `terms` 为已小写化的查询词列表（按空白拆分）。函数对每个 term 在摘要的
/// 各字段中做大小写不敏感子串匹配，命中则加权累加。
///
/// 权重设计（与字段信息密度成正比）：
/// - `summary_title`：3（标题最浓缩）
/// - `tags`：2（人工/启发式标注，语义性强）
/// - `key_entities`：2（实体名高辨识度）
/// - `clue_anchors`：2（检索锚点，专为匹配设计）
/// - `key_facts`：1（事实陈述，可能较长，子串命中权重略低）
/// - `abstract_text`：1（摘要文本，同上）
///
/// 返回所有 term 在所有字段命中后的加权总分。`terms` 为空返回 0。
fn relevance_score(summary: &SummaryView, terms: &[String]) -> usize {
    if terms.is_empty() {
        return 0;
    }

    let mut score = 0usize;

    // 预处理各字段为小写，避免重复分配
    let title_lower = summary.summary_title.to_lowercase();
    let abs_lower = summary
        .abstract_text
        .as_ref()
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let tags_lower: Vec<String> = summary.tags.iter().map(|t| t.to_lowercase()).collect();
    let entities_lower: Vec<String> =
        summary.key_entities.iter().map(|e| e.to_lowercase()).collect();
    let facts_lower: Vec<String> = summary.key_facts.iter().map(|f| f.to_lowercase()).collect();
    let anchors_lower: Vec<String> =
        summary.clue_anchors.iter().map(|a| a.to_lowercase()).collect();

    for term in terms {
        // title（权重 3）
        if title_lower.contains(term) {
            score += 3;
        }
        // tags（权重 2，命中任一即可）
        if tags_lower.iter().any(|t| t.contains(term)) {
            score += 2;
        }
        // key_entities（权重 2）
        if entities_lower.iter().any(|e| e.contains(term)) {
            score += 2;
        }
        // clue_anchors（权重 2）
        if anchors_lower.iter().any(|a| a.contains(term)) {
            score += 2;
        }
        // key_facts（权重 1）
        if facts_lower.iter().any(|f| f.contains(term)) {
            score += 1;
        }
        // abstract_text（权重 1）
        if !abs_lower.is_empty() && abs_lower.contains(term) {
            score += 1;
        }
    }

    score
}

/// 周期层级排序辅助函数
///
/// 返回 daily=0, weekly=1, monthly=2，其他=3。
/// 用于在相关性渲染中保持周期分组的自然顺序。
fn period_order(period: &str) -> u8 {
    match period {
        "daily" => 0,
        "weekly" => 1,
        "monthly" => 2,
        _ => 3,
    }
}

/// 检索器
///
/// 持有 [`Storage`] 引用，从存储实时读取索引文档和记忆文件。
pub struct Retriever {
    /// 存储后端
    storage: Arc<dyn Storage>,
    /// 会话 ID
    session_id: String,
    /// 项目 ID（可选）
    project_id: Option<String>,
}

impl Retriever {
    /// 创建新的检索器
    pub fn new(
        storage: Arc<dyn Storage>,
        session_id: impl Into<String>,
        project_id: Option<String>,
    ) -> Self {
        Self {
            storage,
            session_id: session_id.into(),
            project_id,
        }
    }

    /// 获取所有周期的摘要视图（用于注入 system prompt）
    ///
    /// 实时从 Storage 读取 daily/weekly/monthly 三个周期的索引文档，
    /// 合并所有钩子转为摘要视图。
    pub async fn get_summaries(&self) -> crate::Result<Vec<SummaryView>> {
        let mut all_summaries = Vec::new();

        for period in ArchivePeriod::all() {
            // v2.4: 有 project_id 时走 project 级聚合索引（跨 session 共享）
            // 无 project_id 时走 session 级索引（隔离）
            let doc = if let Some(pid) = &self.project_id {
                self.storage.read_project_index(pid, period).await?
            } else {
                self.storage
                    .read_index(&self.session_id, None, period)
                    .await?
            };

            if let Some(doc) = doc {
                for hook in &doc.hooks {
                    all_summaries.push(SummaryView::from(hook));
                }
            }
        }

        // 按归档时间排序（旧→新）
        all_summaries.sort_by(|a, b| a.archived_at.cmp(&b.archived_at));

        Ok(all_summaries)
    }

    /// 渲染摘要视图为 system prompt 文本（v2.4 分级渲染）
    ///
    /// **分级渲染策略**：
    /// - 日级（daily）：仅标题 + 标签（轻量，避免上下文膨胀）
    /// - 周级（weekly）：标题 + abstract + key_facts + key_entities（结构化摘要）
    /// - 月级（monthly）：全字段含 clue_anchors（最详细）
    ///
    /// **高价值片段自动展开**：
    /// - 含 ToolCall/Thinking/CodeBlock 等标签的 daily 钩子，自动展开 key_facts（若有）
    /// - 高价值判定：tags 含 "工具调用"/"思考过程"/"代码块"/"文件附件"/"图片"/"视频"
    ///
    /// 格式：按周期分组，每个钩子按层级展示。
    /// 若无任何记忆，返回空字符串。
    pub async fn render_to_system_prompt(&self) -> crate::Result<String> {
        let summaries = self.get_summaries().await?;

        if summaries.is_empty() {
            return Ok(String::new());
        }

        let mut out = String::from("# 可用记忆索引\n\n");
        out.push_str("以下是可用的历史记忆摘要，可直接基于此信息回答用户问题：\n\n");

        // 高价值标签集合（自动展开判定）
        const HIGH_VALUE_TAGS: &[&str] = &[
            "工具调用",
            "思考过程",
            "代码块",
            "文件附件",
            "图片",
            "视频",
        ];

        // 按周期分组
        for period in ArchivePeriod::all() {
            let period_name = period.as_dir_name();
            let hooks: Vec<&SummaryView> = summaries
                .iter()
                .filter(|s| s.period == period_name)
                .collect();

            if hooks.is_empty() {
                continue;
            }

            let period_label = match period {
                ArchivePeriod::Daily => "近期记忆",
                ArchivePeriod::Weekly => "周度记忆",
                ArchivePeriod::Monthly => "月度记忆",
            };
            out.push_str(&format!("## {}（{}）\n\n", period_label, period_name));

            for s in hooks {
                let tags_str = if s.tags.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", s.tags.join(", "))
                };
                out.push_str(&format!(
                    "- **{}**{}（{} tokens, at {}）\n",
                    s.summary_title, tags_str, s.token_count, s.archived_at
                ));
                out.push_str(&format!("  - 记忆 ID: `{}`\n", s.hook_id));

                // 分级渲染：根据周期层级展示不同详细度
                let should_expand = match period {
                    ArchivePeriod::Daily => {
                        // 日级：仅高价值片段展开 key_facts
                        s.tags.iter().any(|t| HIGH_VALUE_TAGS.contains(&t.as_str()))
                            && !s.key_facts.is_empty()
                    }
                    ArchivePeriod::Weekly => s.is_rich,
                    ArchivePeriod::Monthly => true, // 月级全展开
                };

                if should_expand {
                    if let Some(abs) = &s.abstract_text {
                        out.push_str(&format!("  - 摘要：{}\n", abs));
                    }
                    if !s.key_facts.is_empty() {
                        out.push_str("  - 关键事实：\n");
                        for fact in &s.key_facts {
                            out.push_str(&format!("    - {}\n", fact));
                        }
                    }
                    if !s.key_entities.is_empty() {
                        out.push_str(&format!("  - 关键实体：{}\n", s.key_entities.join(", ")));
                    }
                    if !s.clue_anchors.is_empty() {
                        out.push_str(&format!("  - 线索锚点：{}\n", s.clue_anchors.join(", ")));
                    }
                }
            }
            out.push('\n');
        }

        Ok(out)
    }

    // ========================================================================
    // v2.16 批次 1：IMP-06 + IMP-07 相关性检索
    // ========================================================================

    /// 按关键词预筛选钩子（IMP-06）
    ///
    /// 从所有周期的摘要视图中筛选出与 `keyword` 相关的钩子。
    /// 匹配字段（大小写不敏感，子串匹配）：
    /// - `summary_title`（权重 3）
    /// - `tags`（权重 2）
    /// - `key_entities`（权重 2）
    /// - `clue_anchors`（权重 2）
    /// - `key_facts`（权重 1）
    /// - `abstract_text`（权重 1）
    ///
    /// 返回的钩子按相关性分数降序排列（同分按归档时间升序）。
    /// 无匹配时返回空 Vec。
    ///
    /// ## 用途
    ///
    /// 供 LLM tool 调用前的预筛选：当钩子数量过多时，先用关键词缩小范围，
    /// 再决定是否 retrieve 完整记忆文件。
    pub async fn filter_hooks_by_keyword(&self, keyword: &str) -> crate::Result<Vec<SummaryView>> {
        let summaries = self.get_summaries().await?;
        let filtered = self.filter_and_sort_by_relevance(summaries, keyword);
        Ok(filtered)
    }

    /// 渲染 system prompt（按查询相关性排序，IMP-07）
    ///
    /// 与 [`Retriever::render_to_system_prompt`] 行为一致，但每个周期内的钩子
    /// 按 `query` 相关性降序排列，使 LLM 优先看到最相关的记忆。
    ///
    /// - `query` 为空时退化为 [`Retriever::render_to_system_prompt`]（按时间排序）
    /// - 相关性分数为 0 的钩子仍会展示（排在末尾），保证 LLM 能看到全部可用记忆
    pub async fn render_to_system_prompt_with_query(&self, query: &str) -> crate::Result<String> {
        let mut summaries = self.get_summaries().await?;

        if summaries.is_empty() {
            return Ok(String::new());
        }

        // query 为空时退化为时间排序
        if query.trim().is_empty() {
            return self.render_to_system_prompt().await;
        }

        // 按周期分组，组内按相关性排序
        // 先按 period 分组，每组内按 relevance_score 降序
        let query_lower = query.to_lowercase();
        let terms: Vec<String> = query_lower
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();

        // 对每个 summary 计算分数并附加
        let mut scored: Vec<(SummaryView, usize)> = summaries
            .into_iter()
            .map(|s| {
                let score = relevance_score(&s, &terms);
                (s, score)
            })
            .collect();

        // 按 period 分组排序：先按 period 顺序（daily/weekly/monthly），组内按 score 降序，同分按时间升序
        scored.sort_by(|a, b| {
            let pa = period_order(&a.0.period);
            let pb = period_order(&b.0.period);
            pa.cmp(&pb)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| a.0.archived_at.cmp(&b.0.archived_at))
        });

        let mut out = String::from("# 可用记忆索引\n\n");
        out.push_str("以下是可用的历史记忆摘要（按与查询的相关性排序）：\n\n");

        const HIGH_VALUE_TAGS: &[&str] = &[
            "工具调用",
            "思考过程",
            "代码块",
            "文件附件",
            "图片",
            "视频",
        ];

        // 按 period 分组渲染
        let mut current_period: Option<&str> = None;
        for (s, score) in &scored {
            // 检测 period 切换
            if current_period != Some(&s.period) {
                current_period = Some(&s.period);
                let period_label = match s.period.as_str() {
                    "daily" => "近期记忆",
                    "weekly" => "周度记忆",
                    "monthly" => "月度记忆",
                    other => other,
                };
                out.push_str(&format!("## {}（{}）\n\n", period_label, s.period));
            }

            let tags_str = if s.tags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", s.tags.join(", "))
            };
            // 相关性标记：score > 0 时显示
            let relevance_marker = if *score > 0 {
                format!("（相关性: {}）", score)
            } else {
                String::new()
            };
            out.push_str(&format!(
                "- **{}**{}{}（{} tokens, at {}）\n",
                s.summary_title, tags_str, relevance_marker, s.token_count, s.archived_at
            ));
            out.push_str(&format!("  - 记忆 ID: `{}`\n", s.hook_id));

            // 分级渲染策略（与 render_to_system_prompt 一致）
            let should_expand = match s.period.as_str() {
                "daily" => {
                    s.tags.iter().any(|t| HIGH_VALUE_TAGS.contains(&t.as_str()))
                        && !s.key_facts.is_empty()
                }
                "weekly" => s.is_rich,
                "monthly" => true,
                _ => false,
            };

            if should_expand {
                if let Some(abs) = &s.abstract_text {
                    out.push_str(&format!("  - 摘要：{}\n", abs));
                }
                if !s.key_facts.is_empty() {
                    out.push_str("  - 关键事实：\n");
                    for fact in &s.key_facts {
                        out.push_str(&format!("    - {}\n", fact));
                    }
                }
                if !s.key_entities.is_empty() {
                    out.push_str(&format!("  - 关键实体：{}\n", s.key_entities.join(", ")));
                }
                if !s.clue_anchors.is_empty() {
                    out.push_str(&format!("  - 线索锚点：{}\n", s.clue_anchors.join(", ")));
                }
            }
        }
        out.push('\n');

        Ok(out)
    }

    /// 内部辅助：对摘要列表按关键词筛选并按相关性排序
    ///
    /// 返回相关性 > 0 的钩子，按分数降序（同分按时间升序）。
    fn filter_and_sort_by_relevance(
        &self,
        summaries: Vec<SummaryView>,
        keyword: &str,
    ) -> Vec<SummaryView> {
        if keyword.trim().is_empty() {
            return summaries;
        }

        let keyword_lower = keyword.to_lowercase();
        let terms: Vec<String> = keyword_lower
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();

        let mut scored: Vec<(SummaryView, usize)> = summaries
            .into_iter()
            .map(|s| {
                let score = relevance_score(&s, &terms);
                (s, score)
            })
            .filter(|(_, score)| *score > 0)
            .collect();

        // 按分数降序，同分按时间升序
        scored.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| a.0.archived_at.cmp(&b.0.archived_at))
        });

        scored.into_iter().map(|(s, _)| s).collect()
    }

    /// 按钩子 ID 检索完整记忆文件（tool 调用入口）
    ///
    /// 流程：
    /// 1. 从所有周期的索引文档中查找对应 hook_id
    /// 2. 获取该钩子指向的 memory_id
    /// 3. 从 Storage 读取完整 MemoryFile
    /// 4. v2.16（IMP-01）：异步自增 access_count（失败容忍，不影响主流程）
    pub async fn retrieve_memory(&self, hook_id: &str) -> crate::Result<MemoryFile> {
        // 在所有周期中查找钩子
        for period in ArchivePeriod::all() {
            if let Some(doc) = self
                .storage
                .read_index(&self.session_id, self.project_id.as_deref(), period)
                .await?
            {
                for hook in &doc.hooks {
                    if hook.id.to_string() == hook_id {
                        // 找到钩子，读取对应的记忆文件
                        let memory = self.storage.read_memory(&hook.memory_id).await?;

                        // v2.16 IMP-01：自增 access_count（失败容忍）
                        // 错误仅记录日志，不影响 retrieve 主流程
                        if let Err(e) = self.storage.update_access_count(&hook.memory_id).await {
                            tracing::warn!(
                                hook_id = %hook_id,
                                memory_id = %hook.memory_id,
                                error = %e,
                                "access_count 自增失败（不影响 retrieve 主流程）"
                            );
                        }

                        return Ok(memory);
                    }
                }
            }
        }

        Err(crate::Error::Index(format!(
            "未找到钩子 ID: {}",
            hook_id
        )))
    }

    /// 按 hook_id 查找对应的 memory_id（v2.4 批次 3 新增）
    ///
    /// 用于 update_memory 场景：先通过 hook_id 定位到 memory_id，
    /// 再调用 Storage::update_memory 执行更新。
    ///
    /// 返回 None 表示未找到对应钩子。
    pub async fn find_memory_id_by_hook(&self, hook_id: &str) -> Option<String> {
        for period in ArchivePeriod::all() {
            if let Ok(Some(doc)) = self
                .storage
                .read_index(&self.session_id, self.project_id.as_deref(), period)
                .await
            {
                for hook in &doc.hooks {
                    if hook.id.to_string() == hook_id {
                        return Some(hook.memory_id.clone());
                    }
                }
            }
        }
        None
    }

    /// 按 session + period 获取索引文档（高级接口）
    ///
    /// 供调用方需要直接操作 IndexDocument 时使用。
    pub async fn get_index_document(
        &self,
        period: ArchivePeriod,
    ) -> crate::Result<Option<crate::model::IndexDocument>> {
        self.storage
            .read_index(&self.session_id, self.project_id.as_deref(), period)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::Archiver;
    use crate::model::{ArchiveConfig, MessageContent, MessageTurn, Tag};
    use crate::storage::LocalStorage;
    use chrono::Utc;
    use tempfile::TempDir;
    use uuid::Uuid;

    /// 构造测试用 MessageTurn
    fn make_turn(text: &str, token_count: usize) -> MessageTurn {
        MessageTurn {
            id: Uuid::new_v4(),
            user_message: MessageContent {
                text: Some(text.into()),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                thinking: None,
            },
            llm_message: MessageContent {
                text: Some("LLM 回复".into()),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                thinking: None,
            },
            tags: vec![Tag::Text, Tag::CodeBlock],
            timestamp: Utc::now(),
            token_count,
        }
    }

    #[tokio::test]
    async fn test_retriever_empty_summaries() {
        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));
        let retriever = Retriever::new(storage, "sess-empty", None);

        let summaries = retriever.get_summaries().await.unwrap();
        assert!(summaries.is_empty());

        let prompt = retriever.render_to_system_prompt().await.unwrap();
        assert!(prompt.is_empty());
    }

    #[tokio::test]
    async fn test_retriever_after_archive() {
        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));

        // 用 Archiver 归档一次
        let config = ArchiveConfig {
            token_threshold: 100,
            force_truncate_limit: 150,
            wait_for_turn_completion: true,
        };
        let mut archiver = Archiver::new(config, storage.clone(), "sess-r1", None);
        archiver.push_turn(make_turn("第一次对话内容", 60));
        archiver.push_turn(make_turn("第二次对话内容", 50));
        let (_memory, hook) = archiver.archive().await.unwrap();

        // 用 Retriever 检索
        let retriever = Retriever::new(storage.clone(), "sess-r1", None);
        let summaries = retriever.get_summaries().await.unwrap();
        assert_eq!(summaries.len(), 1);

        let s = &summaries[0];
        assert_eq!(s.hook_id, hook.id.to_string());
        // v2.4: memory_id 是相对路径（如 sessions/sess-r1/daily/xxx.json），不是 UUID
        assert!(s.memory_id.starts_with("sessions/sess-r1/daily/"));
        assert!(s.memory_id.ends_with(".json"));
        assert!(s.summary_title.contains("第一次对话内容"));
        assert!(s.tags.contains(&"文本消息".to_string()));
        assert!(s.tags.contains(&"代码块".to_string()));
        assert_eq!(s.period, "daily");
        assert_eq!(s.token_count, 110);
    }

    #[tokio::test]
    async fn test_retriever_render_prompt() {
        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));

        let config = ArchiveConfig {
            token_threshold: 100,
            force_truncate_limit: 150,
            wait_for_turn_completion: true,
        };
        let mut archiver = Archiver::new(config, storage.clone(), "sess-r2", None);
        archiver.push_turn(make_turn("讨论记忆库设计", 60));
        archiver.push_turn(make_turn("确定三级周期", 50));
        archiver.archive().await.unwrap();

        let retriever = Retriever::new(storage, "sess-r2", None);
        let prompt = retriever.render_to_system_prompt().await.unwrap();

        assert!(prompt.contains("# 可用记忆索引"));
        assert!(prompt.contains("## 近期记忆（daily）"));
        assert!(prompt.contains("讨论记忆库设计"));
        assert!(prompt.contains("文本消息"));
        assert!(prompt.contains("代码块"));
    }

    #[tokio::test]
    async fn test_retriever_retrieve_memory() {
        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));

        let config = ArchiveConfig {
            token_threshold: 100,
            force_truncate_limit: 150,
            wait_for_turn_completion: true,
        };
        let mut archiver = Archiver::new(config, storage.clone(), "sess-r3", None);
        archiver.push_turn(make_turn("可检索的内容", 110));
        let (original_memory, hook) = archiver.archive().await.unwrap();

        let retriever = Retriever::new(storage, "sess-r3", None);

        // 按钩子 ID 检索
        let retrieved = retriever
            .retrieve_memory(&hook.id.to_string())
            .await
            .unwrap();

        assert_eq!(retrieved.id, original_memory.id);
        assert_eq!(retrieved.session_id, "sess-r3");
        assert_eq!(retrieved.turns.len(), 1);
        assert_eq!(retrieved.total_tokens, 110);
    }

    #[tokio::test]
    async fn test_retriever_retrieve_nonexistent() {
        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));
        let retriever = Retriever::new(storage, "sess-r4", None);

        let result = retriever.retrieve_memory("nonexistent-id").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_retriever_multiple_archives() {
        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));

        let config = ArchiveConfig {
            token_threshold: 100,
            force_truncate_limit: 150,
            wait_for_turn_completion: true,
        };
        let mut archiver = Archiver::new(config, storage.clone(), "sess-r5", None);

        // 归档 3 次
        let mut hooks = Vec::new();
        for i in 1..=3 {
            archiver.push_turn(make_turn(&format!("话题 {}", i), 60));
            archiver.push_turn(make_turn(&format!("续接 {}", i), 50));
            let (_, hook) = archiver.archive().await.unwrap();
            hooks.push(hook);
        }

        let retriever = Retriever::new(storage, "sess-r5", None);
        let summaries = retriever.get_summaries().await.unwrap();
        assert_eq!(summaries.len(), 3);

        // 验证按时间排序（旧→新）
        assert!(summaries[0].archived_at <= summaries[1].archived_at);
        assert!(summaries[1].archived_at <= summaries[2].archived_at);

        // 检索第二个记忆
        let retrieved = retriever
            .retrieve_memory(&hooks[1].id.to_string())
            .await
            .unwrap();
        assert!(retrieved.turns[0]
            .user_message
            .text
            .as_ref()
            .unwrap()
            .contains("话题 2"));
    }

    // ====================================================================
    // v2.16 批次 1 测试：IMP-01 / IMP-06 / IMP-07
    // ====================================================================

    #[tokio::test]
    async fn test_imp01_access_count_increments_on_retrieve() {
        // IMP-01：retrieve 成功后 access_count 应自增
        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));

        let config = ArchiveConfig {
            token_threshold: 100,
            force_truncate_limit: 150,
            wait_for_turn_completion: true,
        };
        let mut archiver = Archiver::new(config, storage.clone(), "sess-acc", None);
        archiver.push_turn(make_turn("访问计数测试", 110));
        let (_, hook) = archiver.archive().await.unwrap();

        // retrieve 前：access_count 应为 0
        let memory_id = hook.memory_id.clone();
        let before = storage.read_memory(&memory_id).await.unwrap();
        assert_eq!(before.access_count, 0);

        // 执行 retrieve
        let retriever = Retriever::new(storage.clone(), "sess-acc", None);
        let _ = retriever
            .retrieve_memory(&hook.id.to_string())
            .await
            .unwrap();

        // retrieve 后：access_count 应为 1
        let after = storage.read_memory(&memory_id).await.unwrap();
        assert_eq!(after.access_count, 1);

        // 再次 retrieve，access_count 应为 2
        let _ = retriever
            .retrieve_memory(&hook.id.to_string())
            .await
            .unwrap();
        let after2 = storage.read_memory(&memory_id).await.unwrap();
        assert_eq!(after2.access_count, 2);
    }

    #[test]
    fn test_imp06_relevance_score_basic() {
        // 直接测试相关性评分函数
        let summary = SummaryView {
            hook_id: "h1".into(),
            memory_id: "m1".into(),
            summary_title: "Rust 记忆库设计".into(),
            abstract_text: Some("讨论了三级索引周期".into()),
            key_facts: vec!["采用 daily/weekly/monthly 三级".into()],
            key_entities: vec!["Rust".into(), "Hippocampus".into()],
            clue_anchors: vec!["归档".into()],
            tags: vec!["代码块".into(), "文本消息".into()],
            archived_at: "2026-07-04T10:00:00Z".into(),
            period: "daily".into(),
            token_count: 100,
            is_rich: true,
        };

        // "rust" 命中 title(3) + entities(2) = 5
        let terms = vec!["rust".into()];
        assert_eq!(relevance_score(&summary, &terms), 5);

        // "记忆库" 命中 title(3) = 3
        let terms = vec!["记忆库".into()];
        assert_eq!(relevance_score(&summary, &terms), 3);

        // "归档" 命中 clue_anchors(2) = 2
        let terms = vec!["归档".into()];
        assert_eq!(relevance_score(&summary, &terms), 2);

        // "不存在词" 不命中 = 0
        let terms = vec!["不存在词".into()];
        assert_eq!(relevance_score(&summary, &terms), 0);

        // 多词："rust 记忆库" = 5 + 3 = 8
        let terms = vec!["rust".into(), "记忆库".into()];
        assert_eq!(relevance_score(&summary, &terms), 8);

        // 空词列表 = 0
        let terms: Vec<String> = vec![];
        assert_eq!(relevance_score(&summary, &terms), 0);
    }

    #[tokio::test]
    async fn test_imp06_filter_hooks_by_keyword() {
        // IMP-06：按关键词筛选钩子
        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));

        let config = ArchiveConfig {
            token_threshold: 100,
            force_truncate_limit: 150,
            wait_for_turn_completion: true,
        };
        let mut archiver = Archiver::new(config, storage.clone(), "sess-filter", None);

        // 归档 3 个不同主题的记忆
        archiver.push_turn(make_turn("讨论 Rust 记忆库设计", 110));
        archiver.archive().await.unwrap();

        archiver.push_turn(make_turn("Vue 前端开发", 110));
        archiver.archive().await.unwrap();

        archiver.push_turn(make_turn("Rust 异步运行时", 110));
        archiver.archive().await.unwrap();

        let retriever = Retriever::new(storage, "sess-filter", None);

        // 筛选 "Rust"：应返回 2 个（标题含 Rust 的）
        let filtered = retriever.filter_hooks_by_keyword("Rust").await.unwrap();
        assert_eq!(filtered.len(), 2);
        // 验证都包含 Rust
        for s in &filtered {
            assert!(
                s.summary_title.to_lowercase().contains("rust"),
                "标题应包含 Rust: {}",
                s.summary_title
            );
        }

        // 筛选 "Vue"：应返回 1 个
        let filtered = retriever.filter_hooks_by_keyword("Vue").await.unwrap();
        assert_eq!(filtered.len(), 1);

        // 筛选不存在的关键词：应返回 0 个
        let filtered = retriever
            .filter_hooks_by_keyword("Python")
            .await
            .unwrap();
        assert!(filtered.is_empty());

        // 空关键词：返回全部（不过滤）
        let filtered = retriever.filter_hooks_by_keyword("").await.unwrap();
        assert_eq!(filtered.len(), 3);
    }

    #[tokio::test]
    async fn test_imp07_render_with_query_relevance_order() {
        // IMP-07：按查询相关性排序渲染
        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));

        let config = ArchiveConfig {
            token_threshold: 100,
            force_truncate_limit: 150,
            wait_for_turn_completion: true,
        };
        let mut archiver = Archiver::new(config, storage.clone(), "sess-rel", None);

        // 归档 3 个记忆，其中 2 个含 "Rust"
        archiver.push_turn(make_turn("Vue 前端开发", 110));
        archiver.archive().await.unwrap();

        archiver.push_turn(make_turn("Rust 记忆库设计", 110));
        archiver.archive().await.unwrap();

        archiver.push_turn(make_turn("Rust 异步运行时", 110));
        archiver.archive().await.unwrap();

        let retriever = Retriever::new(storage, "sess-rel", None);

        // 用 "Rust" 查询渲染
        let prompt = retriever
            .render_to_system_prompt_with_query("Rust")
            .await
            .unwrap();

        // 应包含所有 3 个记忆（相关性为 0 的也展示，排在末尾）
        assert!(prompt.contains("Vue 前端开发"));
        assert!(prompt.contains("Rust 记忆库设计"));
        assert!(prompt.contains("Rust 异步运行时"));

        // 相关性标记应出现
        assert!(prompt.contains("相关性:"));

        // Vue 的相关性标记不应出现（分数为 0）
        // 验证 "Rust" 相关项排在 "Vue" 之前
        let rust_pos = prompt
            .find("Rust 记忆库设计")
            .or_else(|| prompt.find("Rust 异步运行时"))
            .unwrap_or(usize::MAX);
        let vue_pos = prompt.find("Vue 前端开发").unwrap_or(0);
        assert!(
            rust_pos < vue_pos,
            "Rust 相关记忆应排在 Vue 之前"
        );
    }

    #[tokio::test]
    async fn test_imp07_render_with_empty_query_degrades() {
        // IMP-07：空 query 退化为时间排序
        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));

        let config = ArchiveConfig {
            token_threshold: 100,
            force_truncate_limit: 150,
            wait_for_turn_completion: true,
        };
        let mut archiver = Archiver::new(config, storage.clone(), "sess-empty-q", None);
        archiver.push_turn(make_turn("测试内容", 110));
        archiver.archive().await.unwrap();

        let retriever = Retriever::new(storage, "sess-empty-q", None);

        // 空 query 应退化为 render_to_system_prompt
        let prompt_with_empty = retriever
            .render_to_system_prompt_with_query("")
            .await
            .unwrap();
        let prompt_normal = retriever.render_to_system_prompt().await.unwrap();

        assert_eq!(prompt_with_empty, prompt_normal);
        // 不应包含相关性标记
        assert!(!prompt_with_empty.contains("相关性:"));
    }
}
