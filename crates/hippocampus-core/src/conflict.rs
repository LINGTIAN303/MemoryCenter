//! # 记忆冲突检测（v2.6 批次 8）
//!
//! 在记忆迭代更新（`update_memory`）时检测新旧事实之间的冲突，
//! 让 Agent 能识别「用户立场反转」「事实矛盾」等情况，而非盲目追加。
//!
//! ## 设计参考
//!
//! - **BeliefShift 基准**：衡量 Agent 识别跨会话矛盾立场的能力
//! - **Kumiho / 信念修正（Belief Revision）**：形式化语义，修正过去判断而不丢失历史
//!
//! ## 架构（可插拔 trait，类比 [`crate::score::Scorer`]）
//!
//! ```text
//! update 请求 → ConflictDetector.detect(update, &existing_memory) → ConflictReport
//!                                                                   ↓
//! MemoryUpdateRecord.conflicts ← Vec<ConflictRecord> ← 持久化到记忆文件
//! ```
//!
//! - [`HeuristicDetector`](crate::heuristic::HeuristicDetector)：默认纯算法实现（无 LLM 依赖）
//! - [`NoopDetector`]：空实现，不做任何检测
//!
//! ## 冲突维度（三维度）
//!
//! 1. **自我矛盾（SelfContradict）**：同一批 update 内 added 与 deprecated 包含相同/相似事实
//! 2. **直接矛盾（DirectContradict）**：added_facts 与现有 key_facts 语义相反（反义词匹配）
//! 3. **立场反转（StanceReversal）**：added_facts 与历史 updates 的 added_facts 直接冲突

use crate::model::{MemoryFile, MemoryUpdate};
use crate::semantic::Embedder;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ============================================================================
// 数据结构
// ============================================================================

/// 冲突类型
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictKind {
    /// 自我矛盾：同一批 update 内 added 与 deprecated 包含相同/相似事实
    SelfContradict,
    /// 直接矛盾：added_facts 与现有 key_facts 语义相反（反义词匹配）
    DirectContradict,
    /// 立场反转：added_facts 与历史 updates 的 added_facts 直接冲突
    StanceReversal,
}

/// 冲突严重级别
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// 信息性（如无效 deprecate，留待未来扩展）
    Info,
    /// 警告（可能矛盾，如立场反转）
    Warning,
    /// 严重（明确矛盾，如自我矛盾、直接反义）
    Critical,
}

/// 单条冲突记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictRecord {
    /// 冲突类型
    pub kind: ConflictKind,
    /// 严重级别
    pub severity: Severity,
    /// 中文描述（人类可读）
    pub description: String,
    /// 冲突的已有事实（DirectContradict / StanceReversal 时有值）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub existing_fact: Option<String>,
    /// 新事实（触发冲突的 update 中的事实）
    pub new_fact: String,
}

/// 冲突检测报告
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConflictReport {
    /// 检测到的所有冲突记录
    pub conflicts: Vec<ConflictRecord>,
}

impl ConflictReport {
    /// 创建空报告
    pub fn empty() -> Self {
        Self::default()
    }

    /// 是否无冲突
    pub fn is_clean(&self) -> bool {
        self.conflicts.is_empty()
    }

    /// 冲突总数
    pub fn count(&self) -> usize {
        self.conflicts.len()
    }

    /// 是否存在 Critical 级别冲突
    pub fn has_critical(&self) -> bool {
        self.conflicts
            .iter()
            .any(|c| c.severity == Severity::Critical)
    }

    /// 按严重级别筛选
    pub fn by_severity(&self, severity: Severity) -> Vec<&ConflictRecord> {
        self.conflicts
            .iter()
            .filter(|c| c.severity == severity)
            .collect()
    }

    /// 追加一条冲突记录
    pub fn push(&mut self, record: ConflictRecord) {
        self.conflicts.push(record);
    }
}

// ============================================================================
// ConflictDetector trait
// ============================================================================

/// 记忆冲突检测器 trait（可插拔）
///
/// 实现方提供具体的冲突检测算法：
/// - [`HeuristicDetector`](crate::heuristic::HeuristicDetector)：启发式纯算法（默认）
/// - [`NoopDetector`]：空实现（不检测）
///
/// ## 调用时机
///
/// 在 `Storage::update_memory` **之前**同步调用：
///
/// ```text,ignore
/// let memory = storage.read_memory(&memory_id).await?;
/// let report = detector.detect(&update, &memory).await;
/// storage.update_memory_with_conflicts(&memory_id, update, report.conflicts).await?;
/// ```
///
/// ## 设计原则
///
/// - **仅记录不阻止**：即使检测到 Critical 冲突，也不阻止更新（保留历史，交由上层 LLM 决策）
/// - **无副作用**：detect 方法不修改输入数据
/// - **可插拔**：通过 trait 注入，Storage 层不感知具体实现
#[async_trait]
pub trait ConflictDetector: Send + Sync {
    /// 检测 `update` 与 `existing_memory` 之间的冲突
    ///
    /// ## 参数
    /// - `update`：待应用的更新（added/revised/deprecated facts）
    /// - `existing_memory`：现有的记忆文件（包含 turns + 历史 updates）
    ///
    /// ## 返回
    /// 冲突检测报告（即使无冲突也返回空报告，不返回错误）
    async fn detect(
        &self,
        update: &MemoryUpdate,
        existing_memory: &MemoryFile,
    ) -> ConflictReport;
}

// ============================================================================
// NoopDetector（默认空实现）
// ============================================================================

/// 空实现（不做任何冲突检测）
///
/// 用于未配置检测器时的默认行为，或测试中需要跳过检测的场景。
#[derive(Debug, Default, Clone)]
pub struct NoopDetector;

impl NoopDetector {
    /// 创建空检测器
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ConflictDetector for NoopDetector {
    async fn detect(
        &self,
        _update: &MemoryUpdate,
        _existing_memory: &MemoryFile,
    ) -> ConflictReport {
        ConflictReport::empty()
    }
}

// ============================================================================
// HybridDetector（v2.11 串联检测器）
// ============================================================================

/// 语义去重模式（v2.15 新增）
///
/// 控制 [`HybridDetector`] 在合并启发式 + LLM 报告时使用的相似度算法。
///
/// ## 模式对比
///
/// | 模式 | 算法 | 精度 | 性能 | 依赖 |
/// |------|------|------|------|------|
/// | [`DedupMode::Char`] | 字符集合 Jaccard（v2.14 默认） | 低 | 最快 | 无 |
/// | [`DedupMode::Word`] | 多语言词级 Jaccard（中/拉丁/混合） | 中 | 快 | 无 |
/// | [`DedupMode::Embedding`] | 嵌入余弦相似度 | 高 | 慢（网络） | `Embedder` |
///
/// ## 降级策略（v2.15）
///
/// `Embedding` 模式下，若未注入 `Embedder` 或嵌入失败，自动降级到 `Word` 模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupMode {
    /// 字符集合 Jaccard（v2.14 默认行为，单字符集合，无顺序）
    ///
    /// 纯 std 实现，最快但精度最低（"不喜欢" vs "欢不喜" 相似度 = 1.0）。
    Char,
    /// 词级 Jaccard（多语言：中文 char_bigrams，拉丁 word_tokens，混合加权）
    ///
    /// 纯 std 实现，比 Char 精度更高（保留顺序信息），无外部依赖。
    Word,
    /// 嵌入余弦相似度（需注入 [`Embedder`]，失败降级到 [`DedupMode::Word`]）
    ///
    /// 精度最高（语义级），但需要网络调用 Embedder API。
    Embedding,
}

impl Default for DedupMode {
    fn default() -> Self {
        Self::Char // 向后兼容 v2.14 行为
    }
}

/// 混合冲突检测器（v2.11，v2.12 精确去重，v2.14 语义去重，v2.15 多模式去重）
///
/// 串联两个检测器：先跑启发式（快速、无网络依赖），
/// 再跑 LLM（语义级补充），合并两份报告。
///
/// ## 设计
///
/// - **降级策略**：LLM 失败时返回空报告（与 `HttpLlmDetector` 行为一致），
///   启发式结果仍然保留
/// - **去重（v2.12）**：基于 `(kind, new_fact)` 元组精确去重。启发式报告全部保留，
///   LLM 报告中与启发式 `(kind, new_fact)` 完全相同的冲突不重复加入
/// - **语义去重（v2.14）**：在精确去重基础上，增加字符 Jaccard 相似度比较。
///   当两个冲突的 `kind` 相同且 `new_fact` 相似度 >= `dedup_threshold` 时，视为重复。
///   默认阈值 0.7（中文短句经验值），可通过 [`with_dedup_threshold`] 配置
/// - **多模式去重（v2.15 新增）**：[`DedupMode`] 枚举支持 Char/Word/Embedding 三种模式，
///   可通过 [`with_dedup_mode`] 或 [`with_embedder`] 配置。默认 `Char` 向后兼容
/// - **使用场景**：同时配置了启发式 + LLM 检测器时使用
///
/// ## 语义去重示例
///
/// - heuristic: `DirectContradict(new_fact="用户不喜欢咖啡")`
/// - LLM: `DirectContradict(new_fact="用户不再喜欢咖啡了")`
/// - `Char` 模式相似度 ≈ 0.78 > 0.7 → 视为重复，只保留 heuristic 的 1 条
///
/// ## 示例
///
/// ```rust,ignore
/// use hippocampus_core::conflict::{ConflictDetector, HybridDetector, DedupMode};
/// use hippocampus_core::heuristic::HeuristicDetector;
/// // use hippocampus_llm::HttpLlmDetector;
///
/// let heuristic = std::sync::Arc::new(HeuristicDetector::new());
/// let llm = std::sync::Arc::new(HttpLlmDetector::new(config));
/// // 默认 Char 模式 + 阈值 0.7（v2.14 行为）
/// let hybrid = HybridDetector::new(heuristic, llm);
/// // 自定义阈值
/// let hybrid = HybridDetector::with_dedup_threshold(heuristic, llm, 0.8);
/// // 词级 Jaccard 模式（v2.15）
/// let hybrid = HybridDetector::with_dedup_mode(heuristic, llm, 0.8, DedupMode::Word);
/// // 嵌入余弦模式（v2.15，需注入 Embedder）
/// let hybrid = HybridDetector::with_embedder(heuristic, llm, 0.85, embedder);
/// let report = hybrid.detect(&update, &memory).await;
/// ```
#[derive(Clone)]
pub struct HybridDetector {
    /// 启发式检测器（通常为 `HeuristicDetector`）
    heuristic: Arc<dyn ConflictDetector>,
    /// LLM 检测器（通常为 `HttpLlmDetector`）
    llm: Arc<dyn ConflictDetector>,
    /// 语义去重阈值（v2.14 新增）
    ///
    /// 当两个冲突的 `kind` 相同且 `new_fact` 相似度 >= 此阈值时，视为重复。
    /// - `0.0`：禁用语义去重（仅精确匹配，退化为 v2.12 行为）
    /// - `1.0`：严格要求完全相同（等价于精确匹配）
    /// - 默认 `0.7`：中文短句近义词替换场景经验值
    dedup_threshold: f64,
    /// 语义去重模式（v2.15 新增，默认 `Char` 向后兼容）
    dedup_mode: DedupMode,
    /// 可选 Embedder（v2.15 新增，`Embedding` 模式必需，其他模式忽略）
    embedder: Option<Arc<dyn Embedder>>,
}

impl HybridDetector {
    /// 创建混合检测器（默认 Char 模式 + 语义去重阈值 0.7）
    ///
    /// 向后兼容 v2.14 行为。
    ///
    /// ## 参数
    ///
    /// - `heuristic`：启发式检测器（先执行，无网络依赖）
    /// - `llm`：LLM 检测器（后执行，失败时返回空报告不阻塞）
    pub fn new(
        heuristic: Arc<dyn ConflictDetector>,
        llm: Arc<dyn ConflictDetector>,
    ) -> Self {
        Self::with_dedup_threshold(heuristic, llm, 0.7)
    }

    /// 创建混合检测器（自定义语义去重阈值，Char 模式，v2.14 新增）
    ///
    /// 向后兼容 v2.14 行为（`dedup_mode = Char`，`embedder = None`）。
    ///
    /// ## 参数
    ///
    /// - `heuristic`：启发式检测器
    /// - `llm`：LLM 检测器
    /// - `dedup_threshold`：语义去重阈值，自动 clamp 到 `[0.0, 1.0]`
    ///   - `0.0`：禁用语义去重（仅精确匹配）
    ///   - `1.0`：严格精确匹配
    ///   - 推荐 `0.6 ~ 0.8`：平衡去重效果与误判风险
    pub fn with_dedup_threshold(
        heuristic: Arc<dyn ConflictDetector>,
        llm: Arc<dyn ConflictDetector>,
        dedup_threshold: f64,
    ) -> Self {
        Self {
            heuristic,
            llm,
            dedup_threshold: dedup_threshold.clamp(0.0, 1.0),
            dedup_mode: DedupMode::default(),
            embedder: None,
        }
    }

    /// 创建混合检测器（自定义去重模式 + 阈值，v2.15 新增）
    ///
    /// ## 参数
    ///
    /// - `heuristic`：启发式检测器
    /// - `llm`：LLM 检测器
    /// - `dedup_threshold`：语义去重阈值，自动 clamp 到 `[0.0, 1.0]`
    /// - `dedup_mode`：去重模式（Char/Word/Embedding）
    ///
    /// ## 注意
    ///
    /// 若 `dedup_mode = Embedding` 但未通过 [`with_embedder`] 注入 Embedder，
    /// 运行时自动降级到 `Word` 模式。
    pub fn with_dedup_mode(
        heuristic: Arc<dyn ConflictDetector>,
        llm: Arc<dyn ConflictDetector>,
        dedup_threshold: f64,
        dedup_mode: DedupMode,
    ) -> Self {
        Self {
            heuristic,
            llm,
            dedup_threshold: dedup_threshold.clamp(0.0, 1.0),
            dedup_mode,
            embedder: None,
        }
    }

    /// 创建混合检测器（注入 Embedder，自动启用 Embedding 模式，v2.15 新增）
    ///
    /// ## 参数
    ///
    /// - `heuristic`：启发式检测器
    /// - `llm`：LLM 检测器
    /// - `dedup_threshold`：语义去重阈值，自动 clamp 到 `[0.0, 1.0]`
    ///   - Embedding 模式推荐 `0.80 ~ 0.90`（嵌入向量相似度阈值通常高于字符 Jaccard）
    /// - `embedder`：Embedder 实例（如 `HttpEmbedder`）
    ///
    /// ## 降级策略
    ///
    /// - Embedder 调用失败（网络/未配置）→ 降级到 `Word` 模式
    /// - `Word` 模式失败不会发生（纯算法）
    pub fn with_embedder(
        heuristic: Arc<dyn ConflictDetector>,
        llm: Arc<dyn ConflictDetector>,
        dedup_threshold: f64,
        embedder: Arc<dyn Embedder>,
    ) -> Self {
        Self {
            heuristic,
            llm,
            dedup_threshold: dedup_threshold.clamp(0.0, 1.0),
            dedup_mode: DedupMode::Embedding,
            embedder: Some(embedder),
        }
    }

    /// 启发式检测器引用（用于测试与诊断）
    pub fn heuristic(&self) -> &Arc<dyn ConflictDetector> {
        &self.heuristic
    }

    /// LLM 检测器引用（用于测试与诊断）
    pub fn llm(&self) -> &Arc<dyn ConflictDetector> {
        &self.llm
    }

    /// 语义去重阈值（v2.14 新增）
    pub fn dedup_threshold(&self) -> f64 {
        self.dedup_threshold
    }

    /// 语义去重模式（v2.15 新增）
    pub fn dedup_mode(&self) -> DedupMode {
        self.dedup_mode
    }

    /// Embedder 引用（v2.15 新增，未注入返回 None）
    pub fn embedder(&self) -> Option<&Arc<dyn Embedder>> {
        self.embedder.as_ref()
    }

    /// 判断 `conflict` 是否与 `existing` 列表中的某条冲突语义重复
    /// （v2.14 新增，v2.15 多模式 + async 化）
    ///
    /// 判定规则：
    /// 1. `kind` 必须相同（不同 kind 不去重）
    /// 2. `new_fact` 精确匹配（v2.12 兼容，快速路径）
    /// 3. `new_fact` 相似度 >= `dedup_threshold`（v2.14 语义去重，v2.15 多模式）
    ///    - `Char`：字符集合 Jaccard
    ///    - `Word`：多语言词级 Jaccard
    ///    - `Embedding`：嵌入余弦，失败降级到 `Word`
    async fn is_semantically_duplicate(
        &self,
        conflict: &ConflictRecord,
        existing: &[ConflictRecord],
    ) -> bool {
        for existing_conflict in existing {
            // 1. kind 必须相同
            if conflict.kind != existing_conflict.kind {
                continue;
            }
            // 2. 精确匹配（快速路径，兼容 v2.12）
            if conflict.new_fact == existing_conflict.new_fact {
                return true;
            }
            // 3. 语义相似度比较（v2.14，仅在阈值 > 0 时启用）
            if self.dedup_threshold > 0.0 {
                let sim = self.compute_similarity(&conflict.new_fact, &existing_conflict.new_fact).await;
                if sim >= self.dedup_threshold {
                    return true;
                }
            }
        }
        false
    }

    /// 查找重复的现有冲突索引（v2.28 字段级 merge 前置）
    ///
    /// 返回 `Some(idx)` 表示 `conflict` 与 `report.conflicts[idx]` 语义重复，
    /// `None` 表示无重复（应直接 push）。
    async fn find_duplicate_index(
        &self,
        conflict: &ConflictRecord,
        existing: &[ConflictRecord],
    ) -> Option<usize> {
        for (idx, existing_conflict) in existing.iter().enumerate() {
            if conflict.kind != existing_conflict.kind {
                continue;
            }
            if conflict.new_fact == existing_conflict.new_fact {
                return Some(idx);
            }
            if self.dedup_threshold > 0.0 {
                let sim = self
                    .compute_similarity(&conflict.new_fact, &existing_conflict.new_fact)
                    .await;
                if sim >= self.dedup_threshold {
                    return Some(idx);
                }
            }
        }
        None
    }

    /// 字段级合并冲突记录（v2.28 新增）
    ///
    /// 当 LLM 与启发式检测到"同一冲突"（kind 相同 + new_fact 匹配）时，
    /// 不再"二选一丢弃 LLM 版本"，而是字段级合并：
    ///
    /// | 字段 | 合并规则 | 理由 |
    /// |------|---------|------|
    /// | `kind` | 相同（前提） | 已通过去重判定 |
    /// | `new_fact` | 保留现有（启发式） | 已通过去重判定 |
    /// | `severity` | `max(existing, incoming)` | 取更严重的级别，避免低估风险 |
    /// | `description` | 优先 incoming（LLM，非空时） | LLM 描述更语义化 |
    /// | `existing_fact` | 优先 incoming（LLM，Some 时） | LLM 可能引用更准确的历史事实 |
    ///
    /// ## 参数
    ///
    /// - `existing`：报告中已有的冲突记录（启发式版本，将被原地修改）
    /// - `incoming`：LLM 报告中的冲突记录（增量信息来源，不被修改）
    fn merge_conflict_fields(existing: &mut ConflictRecord, incoming: &ConflictRecord) {
        // severity：取更严重的（Severity derive Ord，Critical > Warning > Info）
        if incoming.severity > existing.severity {
            existing.severity = incoming.severity;
        }

        // description：优先 LLM（非空且更长时替换，避免空字符串覆盖）
        if !incoming.description.is_empty()
            && incoming.description.len() > existing.description.len()
        {
            existing.description = incoming.description.clone();
        }

        // existing_fact：优先 LLM（Some 时替换，None 不覆盖）
        if incoming.existing_fact.is_some() {
            existing.existing_fact = incoming.existing_fact.clone();
        }
    }

    /// 根据当前 `dedup_mode` 计算相似度（v2.15 新增）
    ///
    /// 内部辅助方法，封装三种模式的分支与降级逻辑。
    async fn compute_similarity(&self, a: &str, b: &str) -> f64 {
        match self.dedup_mode {
            DedupMode::Char => similarity_char(a, b),
            DedupMode::Word => similarity_word(a, b),
            DedupMode::Embedding => {
                if let Some(embedder) = &self.embedder {
                    match similarity_embedding(embedder, a, b).await {
                        Some(sim) => sim,
                        None => similarity_word(a, b), // 嵌入失败降级
                    }
                } else {
                    // Embedding 模式但未注入 Embedder，降级到 Word
                    similarity_word(a, b)
                }
            }
        }
    }
}

/// 计算两个字符串的字符 Jaccard 相似度（v2.14 新增，v2.15 重命名为 similarity_char）
///
/// 基于字符集合的 Jaccard 系数：`|A ∩ B| / |A ∪ B|`，范围 `[0.0, 1.0]`。
///
/// - `1.0`：完全相同（字符集合相同）
/// - `0.0`：完全不同（无共同字符）
///
/// ## 特点
///
/// - 纯 std 实现，无外部依赖
/// - 对中文短句效果可接受（字符级覆盖度高时判定为相似）
/// - 局限：只看字符集合，不看顺序（"不喜欢" vs "欢不喜" 相似度 = 1.0）
///   但冲突去重场景下，顺序不同但字符相同的情况极少，影响可忽略
fn similarity_char(a: &str, b: &str) -> f64 {
    // 空字符串无相似度（避免 0/0 未定义，且语义上空内容无可比较性）
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    let set_a: std::collections::HashSet<char> = a.chars().collect();
    let set_b: std::collections::HashSet<char> = b.chars().collect();
    let intersection = set_a.intersection(&set_b).count();
    let union = set_a.union(&set_b).count();
    if union == 0 {
        return 0.0;
    }
    intersection as f64 / union as f64
}

/// 计算两个字符串的词级 Jaccard 相似度（v2.15 新增）
///
/// 多语言分支（参考 [`crate::heuristic`] 的 `detect_language` 逻辑）：
///
/// - **中文**（CJK 占比 ≥ 60%）：字符 bigram 集合 Jaccard，保留顺序信息
///   - "用户不喜欢咖啡" → {"用户", "户不", "不喜", "喜欢", "欢咖", "咖啡"}
/// - **拉丁**（CJK 占比 ≤ 40%）：词级集合 Jaccard，转小写
///   - "I love coffee" → {"i", "love", "coffee"}
/// - **混合**（40% < CJK < 60%）：按 CJK 比例加权
///   - `avg_cjk * cn_sim + (1 - avg_cjk) * word_sim`
///
/// ## 与 [`similarity_char`] 的差异
///
/// | 维度 | similarity_char | similarity_word |
/// |------|-----------------|-----------------|
/// | 中文粒度 | 单字符集合 | 字符 bigram（保留顺序） |
/// | 拉丁粒度 | 单字符集合 | 词级集合 |
/// | 顺序敏感性 | 无 | 部分（bigram 保留局部顺序） |
///
/// ## 示例
///
/// - `similarity_word("不喜欢", "欢不喜")` ≈ 0.0（bigram 完全不同）
/// - `similarity_char("不喜欢", "欢不喜")` = 1.0（字符集合相同）
fn similarity_word(a: &str, b: &str) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    let cjk_a = cjk_ratio(a);
    let cjk_b = cjk_ratio(b);
    let avg_cjk = (cjk_a + cjk_b) / 2.0;

    if avg_cjk >= 0.6 {
        // 中文：字符 bigram Jaccard
        let bigrams_a = char_bigrams(a);
        let bigrams_b = char_bigrams(b);
        jaccard(&bigrams_a, &bigrams_b)
    } else if avg_cjk <= 0.4 {
        // 拉丁：词级 Jaccard
        let words_a = word_tokens(a);
        let words_b = word_tokens(b);
        jaccard(&words_a, &words_b)
    } else {
        // 混合：按 CJK 比例动态加权
        let cn_sim = jaccard(&char_bigrams(a), &char_bigrams(b));
        let word_sim = jaccard(&word_tokens(a), &word_tokens(b));
        avg_cjk * cn_sim + (1.0 - avg_cjk) * word_sim
    }
}

/// 计算两个字符串的嵌入余弦相似度（v2.15 新增）
///
/// 调用 [`Embedder::embed_batch`] 批量嵌入两个文本，返回余弦相似度。
///
/// ## 返回
///
/// - `Some(f64)`：成功，返回 `[-1.0, 1.0]` 范围的相似度
/// - `None`：失败（网络错误 / API 错误 / 向量维度不一致 / 数量不匹配）
///
/// ## 降级
///
/// 调用方应在 `None` 时降级到 [`similarity_word`]，避免阻塞去重流程。
async fn similarity_embedding(
    embedder: &Arc<dyn Embedder>,
    a: &str,
    b: &str,
) -> Option<f64> {
    let vecs = embedder.embed_batch(&[a, b]).await.ok()?;
    if vecs.len() != 2 {
        return None;
    }
    let sim = crate::vector::cosine_similarity(&vecs[0], &vecs[1]);
    Some(sim as f64)
}

// ============================================================================
// 词级相似度辅助函数（v2.15 新增，与 heuristic.rs 保持独立实现避免跨模块耦合）
// ============================================================================

/// 计算 CJK 字符占比（0.0 ~ 1.0）
fn cjk_ratio(s: &str) -> f64 {
    let total = s.chars().filter(|c| !c.is_whitespace()).count();
    if total == 0 {
        return 0.0;
    }
    let cjk = s
        .chars()
        .filter(|c| {
            let code = *c as u32;
            (0x4E00..=0x9FFF).contains(&code) || (0x3400..=0x4DBF).contains(&code)
        })
        .count();
    cjk as f64 / total as f64
}

/// 生成字符 bigram 集合（中文场景使用，保留局部顺序）
///
/// 例如 "abc" → {"ab", "bc"}
fn char_bigrams(s: &str) -> std::collections::HashSet<String> {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() < 2 {
        let mut set = std::collections::HashSet::new();
        if !s.is_empty() {
            set.insert(s.to_string());
        }
        return set;
    }
    (0..chars.len() - 1)
        .map(|i| format!("{}{}", chars[i], chars[i + 1]))
        .collect()
}

/// 拉丁字母词级分词（按空格 + 标点切分，转小写）
///
/// 例如 "I love coffee" → {"i", "love", "coffee"}
fn word_tokens(s: &str) -> std::collections::HashSet<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

/// 计算两个 HashSet 的 Jaccard 相似度
///
/// `J(A, B) = |A ∩ B| / |A ∪ B|`
fn jaccard<T: std::hash::Hash + Eq>(
    a: &std::collections::HashSet<T>,
    b: &std::collections::HashSet<T>,
) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(b).count();
    let union = a.union(b).count();
    if union == 0 {
        return 0.0;
    }
    intersection as f64 / union as f64
}

#[async_trait]
impl ConflictDetector for HybridDetector {
    async fn detect(
        &self,
        update: &MemoryUpdate,
        existing_memory: &MemoryFile,
    ) -> ConflictReport {
        // 1. 先跑启发式（快速、无网络依赖）
        let mut report = self.heuristic.detect(update, existing_memory).await;

        // 2. 再跑 LLM（语义级补充，失败时返回空报告不阻塞）
        let llm_report = self.llm.detect(update, existing_memory).await;

        // 3. 合并报告：语义去重 + 字段级 merge（v2.28）
        //    - LLM 报告中与启发式冲突（kind 相同 + new_fact 匹配/相似）：
        //      字段级 merge（severity 取 max / description / existing_fact 优先 LLM）
        //    - LLM 报告中独有的冲突：直接 push
        //    - LLM 报告为空时（降级或无冲突）不影响启发式结果
        //    - v2.15：相似度比较 async（Embedding 模式需 await）
        //    - v2.28：从"二选一丢弃 LLM"升级为"字段级 merge"
        for conflict in llm_report.conflicts {
            match self.find_duplicate_index(&conflict, &report.conflicts).await {
                Some(idx) => {
                    // 语义重复：字段级 merge（LLM 增量信息合并到启发式版本）
                    Self::merge_conflict_fields(&mut report.conflicts[idx], &conflict);
                }
                None => {
                    // 独有冲突：直接 push
                    report.push(conflict);
                }
            }
        }

        report
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ArchivePeriod, MessageContent, MessageTurn};
    use chrono::Utc;
    use uuid::Uuid;

    /// 构造测试用 MemoryFile
    fn make_test_memory() -> MemoryFile {
        let turn = MessageTurn {
            id: Uuid::new_v4(),
            user_message: MessageContent {
                text: Some("用户消息".to_string()),
                attachments: vec![],
                tool_calls: vec![],
                thinking: None,
            },
            llm_message: MessageContent {
                text: Some("助手回复".to_string()),
                attachments: vec![],
                tool_calls: vec![],
                thinking: None,
            },
            tags: vec![],
            timestamp: Utc::now(),
            token_count: 100,
        };

        MemoryFile {
            id: Uuid::new_v4(),
            schema_version: 1,
            archived_at: Utc::now(),
            session_id: "test-sess".to_string(),
            project_id: None,
            turns: vec![turn],
            tags: vec![],
            total_tokens: 100,
            truncated: false,
            period: ArchivePeriod::Daily,
            access_count: 0,
            importance: 0,
            updates: vec![],
        }
    }

    #[test]
    fn test_conflict_report_empty() {
        let report = ConflictReport::empty();
        assert!(report.is_clean());
        assert_eq!(report.count(), 0);
        assert!(!report.has_critical());
    }

    #[test]
    fn test_conflict_report_push_and_query() {
        let mut report = ConflictReport::empty();
        report.push(ConflictRecord {
            kind: ConflictKind::SelfContradict,
            severity: Severity::Critical,
            description: "测试冲突".to_string(),
            existing_fact: None,
            new_fact: "fact A".to_string(),
        });
        report.push(ConflictRecord {
            kind: ConflictKind::StanceReversal,
            severity: Severity::Warning,
            description: "立场反转".to_string(),
            existing_fact: Some("旧立场".to_string()),
            new_fact: "新立场".to_string(),
        });

        assert!(!report.is_clean());
        assert_eq!(report.count(), 2);
        assert!(report.has_critical());
        assert_eq!(report.by_severity(Severity::Critical).len(), 1);
        assert_eq!(report.by_severity(Severity::Warning).len(), 1);
        assert_eq!(report.by_severity(Severity::Info).len(), 0);
    }

    #[test]
    fn test_severity_ordering() {
        assert!(Severity::Critical > Severity::Warning);
        assert!(Severity::Warning > Severity::Info);
        assert!(Severity::Critical > Severity::Info);
    }

    #[tokio::test]
    async fn test_noop_detector_returns_empty() {
        let detector = NoopDetector::new();
        let memory = make_test_memory();
        let update = MemoryUpdate::new().add_fact("新事实");
        let report = detector.detect(&update, &memory).await;
        assert!(report.is_clean());
    }

    #[test]
    fn test_conflict_record_serialization() {
        let record = ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Critical,
            description: "用户先说喜欢，后说不喜欢".to_string(),
            existing_fact: Some("用户喜欢咖啡".to_string()),
            new_fact: "用户不喜欢咖啡".to_string(),
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("direct_contradict"));
        assert!(json.contains("critical"));
        assert!(json.contains("用户喜欢咖啡"));

        // 反序列化往返
        let restored: ConflictRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.kind, ConflictKind::DirectContradict);
        assert_eq!(restored.severity, Severity::Critical);
        assert_eq!(restored.new_fact, "用户不喜欢咖啡");
    }

    #[test]
    fn test_conflict_report_serialization_skip_none() {
        let record = ConflictRecord {
            kind: ConflictKind::SelfContradict,
            severity: Severity::Critical,
            description: "自我矛盾".to_string(),
            existing_fact: None,
            new_fact: "fact".to_string(),
        };
        let json = serde_json::to_string(&record).unwrap();
        // existing_fact 为 None 时应被跳过
        assert!(!json.contains("existing_fact"));
    }

    // ========================================================================
    // v2.11：HybridDetector 测试
    // ========================================================================

    /// Mock 检测器：返回预设的 ConflictReport
    ///
    /// 用于模拟 LLM 检测器（成功/失败降级/返回特定冲突），
    /// 避免在单元测试中发起真实 HTTP 请求。
    struct MockDetector {
        report: ConflictReport,
    }

    impl MockDetector {
        fn new(report: ConflictReport) -> Self {
            Self { report }
        }

        fn empty() -> Self {
            Self::new(ConflictReport::empty())
        }

        fn single_critical() -> Self {
            let mut report = ConflictReport::empty();
            report.push(ConflictRecord {
                kind: ConflictKind::DirectContradict,
                severity: Severity::Critical,
                description: "LLM 检测到语义矛盾".to_string(),
                existing_fact: Some("旧事实".to_string()),
                new_fact: "新事实".to_string(),
            });
            Self::new(report)
        }

        fn single_warning() -> Self {
            let mut report = ConflictReport::empty();
            report.push(ConflictRecord {
                kind: ConflictKind::StanceReversal,
                severity: Severity::Warning,
                description: "LLM 检测到立场反转".to_string(),
                existing_fact: Some("旧立场".to_string()),
                new_fact: "新立场".to_string(),
            });
            Self::new(report)
        }
    }

    #[async_trait]
    impl ConflictDetector for MockDetector {
        async fn detect(
            &self,
            _update: &MemoryUpdate,
            _existing_memory: &MemoryFile,
        ) -> ConflictReport {
            // 克隆预设报告返回
            self.report.clone()
        }
    }

    /// 构造一个 Heuristic 检测到 1 条 Critical 冲突的 update + memory 组合
    ///
    /// 场景：历史已添加"用户喜欢咖啡"，本次 update 添加"用户不喜欢咖啡"
    fn make_heuristic_contradiction_case() -> (MemoryUpdate, MemoryFile) {
        let mut memory = make_test_memory();
        // 历史已添加"用户喜欢咖啡"
        memory.updates.push(crate::model::MemoryUpdateRecord {
            updated_at: chrono::Utc::now(),
            update: MemoryUpdate::new().add_fact("用户喜欢咖啡"),
            conflicts: vec![],
        });
        // 本次 update 添加"用户不喜欢咖啡"（与历史直接矛盾）
        let update = MemoryUpdate::new().add_fact("用户不喜欢咖啡");
        (update, memory)
    }

    #[tokio::test]
    async fn test_hybrid_detector_merges_both_reports() {
        // heuristic 返回 1 条 Critical + LLM 返回 1 条 Warning → 合并后 2 条
        let heuristic: Arc<dyn ConflictDetector> =
            Arc::new(crate::heuristic::HeuristicDetector::new());
        let llm: Arc<dyn ConflictDetector> =
            Arc::new(MockDetector::single_warning());
        let hybrid = HybridDetector::new(heuristic, llm);

        let (update, memory) = make_heuristic_contradiction_case();
        let report = hybrid.detect(&update, &memory).await;

        // heuristic 检测到 1 条 DirectContradict（Critical）
        // LLM 检测到 1 条 StanceReversal（Warning）
        assert_eq!(
            report.count(),
            2,
            "合并后应有 2 条冲突，实际: {}",
            report.count()
        );
        assert!(
            report.has_critical(),
            "应存在 Critical 级别冲突（来自 heuristic）"
        );
    }

    #[tokio::test]
    async fn test_hybrid_detector_llm_empty_keeps_heuristic() {
        // 模拟 LLM 失败降级（返回空报告）→ 启发式结果仍保留
        let heuristic: Arc<dyn ConflictDetector> =
            Arc::new(crate::heuristic::HeuristicDetector::new());
        let llm: Arc<dyn ConflictDetector> = Arc::new(MockDetector::empty());
        let hybrid = HybridDetector::new(heuristic, llm);

        let (update, memory) = make_heuristic_contradiction_case();
        let report = hybrid.detect(&update, &memory).await;

        // LLM 降级为空，只剩 heuristic 的 1 条 DirectContradict
        assert_eq!(
            report.count(),
            1,
            "LLM 降级为空时应保留 heuristic 的 1 条冲突"
        );
        assert!(report.has_critical());
        // 唯一一条应是 DirectContradict（来自 heuristic）
        assert_eq!(
            report.conflicts[0].kind,
            ConflictKind::DirectContradict
        );
    }

    #[tokio::test]
    async fn test_hybrid_detector_both_empty() {
        // 两者都返回空 → 空报告
        let heuristic: Arc<dyn ConflictDetector> =
            Arc::new(crate::heuristic::HeuristicDetector::new());
        let llm: Arc<dyn ConflictDetector> = Arc::new(MockDetector::empty());
        let hybrid = HybridDetector::new(heuristic, llm);

        // 无冲突的 update（添加一个无关事实）
        let memory = make_test_memory();
        let update = MemoryUpdate::new().add_fact("用户住在上海");
        let report = hybrid.detect(&update, &memory).await;

        assert!(report.is_clean(), "无冲突场景应返回空报告");
        assert!(!report.has_critical());
    }

    #[tokio::test]
    async fn test_hybrid_detector_both_noop() {
        // 两个 NoopDetector 串联 → 永远空报告
        let heuristic: Arc<dyn ConflictDetector> = Arc::new(NoopDetector::new());
        let llm: Arc<dyn ConflictDetector> = Arc::new(NoopDetector::new());
        let hybrid = HybridDetector::new(heuristic, llm);

        let (update, memory) = make_heuristic_contradiction_case();
        let report = hybrid.detect(&update, &memory).await;

        assert!(report.is_clean());
    }

    #[tokio::test]
    async fn test_hybrid_detector_accessor_methods() {
        // 验证 heuristic() / llm() 访问器
        let heuristic: Arc<dyn ConflictDetector> =
            Arc::new(crate::heuristic::HeuristicDetector::new());
        let llm: Arc<dyn ConflictDetector> = Arc::new(MockDetector::single_critical());
        let hybrid = HybridDetector::new(heuristic, llm);

        // 通过访问器获取引用并调用 detect
        let memory = make_test_memory();
        let update = MemoryUpdate::new().add_fact("测试");
        let h_report = hybrid.heuristic().detect(&update, &memory).await;
        let l_report = hybrid.llm().detect(&update, &memory).await;

        // heuristic 对此场景应无冲突，Mock single_critical 应有 1 条
        assert!(h_report.is_clean());
        assert_eq!(l_report.count(), 1);
    }

    #[tokio::test]
    async fn test_hybrid_detector_preserves_severity_ordering() {
        // heuristic 检测到 Warning + LLM 检测到 Critical → 合并后 has_critical=true
        // 使用 NoopDetector 作为 heuristic（不产生冲突），LLM 提供 Critical
        let heuristic: Arc<dyn ConflictDetector> = Arc::new(NoopDetector::new());
        let llm: Arc<dyn ConflictDetector> = Arc::new(MockDetector::single_critical());
        let hybrid = HybridDetector::new(heuristic, llm);

        let memory = make_test_memory();
        let update = MemoryUpdate::new().add_fact("测试");
        let report = hybrid.detect(&update, &memory).await;

        assert_eq!(report.count(), 1);
        assert!(report.has_critical());
        assert_eq!(report.by_severity(Severity::Critical).len(), 1);
    }

    // ========================================================================
    // v2.12：去重优化测试
    // ========================================================================

    #[tokio::test]
    async fn test_hybrid_detector_dedup_same_kind_new_fact() {
        // v2.12：基于 (kind, new_fact) 元组去重
        // heuristic 检测到 DirectContradict(new_fact="用户不喜欢咖啡")
        // LLM 也报告 DirectContradict(new_fact="用户不喜欢咖啡") → 应去重，只保留 1 条
        let heuristic: Arc<dyn ConflictDetector> =
            Arc::new(crate::heuristic::HeuristicDetector::new());

        // 构造 LLM mock，返回与 heuristic 相同 (kind, new_fact) 的冲突
        let mut llm_report = ConflictReport::empty();
        llm_report.push(ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Critical,
            description: "LLM 也检测到直接矛盾".to_string(),
            existing_fact: Some("用户喜欢咖啡".to_string()),
            new_fact: "用户不喜欢咖啡".to_string(), // 与 heuristic 相同
        });
        let llm: Arc<dyn ConflictDetector> = Arc::new(MockDetector::new(llm_report));

        let hybrid = HybridDetector::new(heuristic, llm);

        let (update, memory) = make_heuristic_contradiction_case();
        let report = hybrid.detect(&update, &memory).await;

        // 去重后应只剩 heuristic 的 1 条（LLM 的重复条目被跳过）
        assert_eq!(
            report.count(),
            1,
            "相同 (kind, new_fact) 的冲突应去重，实际: {}",
            report.count()
        );
        assert!(report.has_critical());
    }

    #[tokio::test]
    async fn test_hybrid_detector_no_dedup_different_kind() {
        // v2.12：kind 不同则不去重（即使 new_fact 相同）
        // heuristic 检测到 DirectContradict(new_fact="用户不喜欢咖啡")
        // LLM 报告 StanceReversal(new_fact="用户不喜欢咖啡") → kind 不同，不去重，保留 2 条
        let heuristic: Arc<dyn ConflictDetector> =
            Arc::new(crate::heuristic::HeuristicDetector::new());

        let mut llm_report = ConflictReport::empty();
        llm_report.push(ConflictRecord {
            kind: ConflictKind::StanceReversal, // 不同 kind
            severity: Severity::Warning,
            description: "LLM 认为是立场反转".to_string(),
            existing_fact: Some("用户喜欢咖啡".to_string()),
            new_fact: "用户不喜欢咖啡".to_string(), // 相同 new_fact
        });
        let llm: Arc<dyn ConflictDetector> = Arc::new(MockDetector::new(llm_report));

        let hybrid = HybridDetector::new(heuristic, llm);

        let (update, memory) = make_heuristic_contradiction_case();
        let report = hybrid.detect(&update, &memory).await;

        assert_eq!(
            report.count(),
            2,
            "kind 不同时不应去重，实际: {}",
            report.count()
        );
        // 应同时存在 Critical（heuristic）和 Warning（LLM）
        assert!(report.has_critical());
        assert_eq!(report.by_severity(Severity::Warning).len(), 1);
    }

    #[tokio::test]
    async fn test_hybrid_detector_dedup_multiple_llm_duplicates() {
        // v2.12：LLM 报告多条，部分与启发式重复，部分为新冲突
        // heuristic 检测到 1 条 DirectContradict(new_fact="用户不喜欢咖啡")
        // LLM 报告 3 条：
        //   ① DirectContradict(new_fact="用户不喜欢咖啡") → 重复，去重
        //   ② StanceReversal(new_fact="用户不喜欢咖啡") → kind 不同，保留
        //   ③ DirectContradict(new_fact="用户讨厌咖啡") → new_fact 不同，保留
        // 最终：heuristic 1 + LLM 2 = 3 条
        let heuristic: Arc<dyn ConflictDetector> =
            Arc::new(crate::heuristic::HeuristicDetector::new());

        let mut llm_report = ConflictReport::empty();
        // ① 与 heuristic 完全重复
        llm_report.push(ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Critical,
            description: "LLM 重复检测".to_string(),
            existing_fact: Some("用户喜欢咖啡".to_string()),
            new_fact: "用户不喜欢咖啡".to_string(),
        });
        // ② kind 不同
        llm_report.push(ConflictRecord {
            kind: ConflictKind::StanceReversal,
            severity: Severity::Warning,
            description: "LLM 立场反转".to_string(),
            existing_fact: Some("用户喜欢咖啡".to_string()),
            new_fact: "用户不喜欢咖啡".to_string(),
        });
        // ③ new_fact 不同
        llm_report.push(ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Critical,
            description: "LLM 另一处矛盾".to_string(),
            existing_fact: Some("用户喜欢咖啡".to_string()),
            new_fact: "用户讨厌咖啡".to_string(),
        });
        let llm: Arc<dyn ConflictDetector> = Arc::new(MockDetector::new(llm_report));

        let hybrid = HybridDetector::new(heuristic, llm);

        let (update, memory) = make_heuristic_contradiction_case();
        let report = hybrid.detect(&update, &memory).await;

        // heuristic 1 + LLM 保留 2（①去重，②③保留）= 3 条
        assert_eq!(
            report.count(),
            3,
            "应去重 1 条重复，保留 3 条，实际: {}",
            report.count()
        );
        assert!(report.has_critical());
    }

    // ========================================================================
    // v2.14 新增：语义去重测试
    // ========================================================================

    #[test]
    fn test_similarity_identical() {
        // 完全相同 → 1.0
        assert_eq!(similarity_char("用户不喜欢咖啡", "用户不喜欢咖啡"), 1.0);
    }

    #[test]
    fn test_similarity_completely_different() {
        // 完全不同（无共同字符）→ 0.0
        assert_eq!(similarity_char("abc", "xyz"), 0.0);
    }

    #[test]
    fn test_similarity_partial_overlap() {
        // 部分重叠："用户不喜欢咖啡" vs "用户不再喜欢咖啡了"
        // set_a = {用,户,不,喜,欢,咖,啡} = 7
        // set_b = {用,户,不,再,喜,欢,了,咖,啡} = 9
        // intersection = {用,户,不,喜,欢,咖,啡} = 7
        // union = 9
        // jaccard = 7/9 ≈ 0.778
        let sim = similarity_char("用户不喜欢咖啡", "用户不再喜欢咖啡了");
        assert!(
            sim > 0.77 && sim < 0.78,
            "相似度应在 0.778 附近，实际: {}",
            sim
        );
    }

    #[test]
    fn test_similarity_empty_strings() {
        assert_eq!(similarity_char("", ""), 0.0);
        assert_eq!(similarity_char("", "abc"), 0.0);
    }

    #[test]
    fn test_hybrid_detector_dedup_threshold_default() {
        // v2.14：默认阈值 0.7
        let heuristic: Arc<dyn ConflictDetector> = Arc::new(NoopDetector::new());
        let llm: Arc<dyn ConflictDetector> = Arc::new(NoopDetector::new());
        let hybrid = HybridDetector::new(heuristic, llm);
        assert_eq!(hybrid.dedup_threshold(), 0.7);
    }

    #[test]
    fn test_hybrid_detector_dedup_threshold_clamp() {
        // v2.14：阈值自动 clamp 到 [0.0, 1.0]
        let heuristic: Arc<dyn ConflictDetector> = Arc::new(NoopDetector::new());
        let llm: Arc<dyn ConflictDetector> = Arc::new(NoopDetector::new());

        let hybrid_neg = HybridDetector::with_dedup_threshold(heuristic.clone(), llm.clone(), -0.5);
        assert_eq!(hybrid_neg.dedup_threshold(), 0.0, "负值应 clamp 到 0.0");

        let hybrid_over = HybridDetector::with_dedup_threshold(heuristic.clone(), llm.clone(), 1.5);
        assert_eq!(hybrid_over.dedup_threshold(), 1.0, "超过 1.0 应 clamp 到 1.0");

        let hybrid_normal = HybridDetector::with_dedup_threshold(heuristic, llm, 0.85);
        assert_eq!(hybrid_normal.dedup_threshold(), 0.85);
    }

    #[tokio::test]
    async fn test_hybrid_detector_semantic_dedup_similar_new_fact() {
        // v2.14：语义去重 - 相似 new_fact 应被去重
        // heuristic: DirectContradict("用户不喜欢咖啡")
        // LLM: DirectContradict("用户不再喜欢咖啡了") → 相似度 ≈ 0.778 > 0.7 → 去重
        let heuristic: Arc<dyn ConflictDetector> =
            Arc::new(crate::heuristic::HeuristicDetector::new());

        let mut llm_report = ConflictReport::empty();
        llm_report.push(ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Critical,
            description: "LLM 语义级重复检测".to_string(),
            existing_fact: Some("用户喜欢咖啡".to_string()),
            new_fact: "用户不再喜欢咖啡了".to_string(), // 与 heuristic 相似但非精确匹配
        });
        let llm: Arc<dyn ConflictDetector> = Arc::new(MockDetector::new(llm_report));

        // 默认阈值 0.7
        let hybrid = HybridDetector::new(heuristic, llm);

        let (update, memory) = make_heuristic_contradiction_case();
        let report = hybrid.detect(&update, &memory).await;

        // heuristic 1 + LLM 去重 1 = 1 条
        assert_eq!(
            report.count(),
            1,
            "相似 new_fact（相似度 > 0.7）应被语义去重，实际: {}",
            report.count()
        );
        assert!(report.has_critical());
    }

    #[tokio::test]
    async fn test_hybrid_detector_semantic_dedup_threshold_zero_disables() {
        // v2.14：阈值 0.0 禁用语义去重（仅精确匹配）
        // 相同场景：heuristic "用户不喜欢咖啡" vs LLM "用户不再喜欢咖啡了"
        // 阈值 0.0 时不做相似度比较，且非精确匹配 → 不去重，保留 2 条
        let heuristic: Arc<dyn ConflictDetector> =
            Arc::new(crate::heuristic::HeuristicDetector::new());

        let mut llm_report = ConflictReport::empty();
        llm_report.push(ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Critical,
            description: "LLM 语义级检测".to_string(),
            existing_fact: Some("用户喜欢咖啡".to_string()),
            new_fact: "用户不再喜欢咖啡了".to_string(),
        });
        let llm: Arc<dyn ConflictDetector> = Arc::new(MockDetector::new(llm_report));

        // 阈值 0.0 禁用语义去重
        let hybrid = HybridDetector::with_dedup_threshold(heuristic, llm, 0.0);

        let (update, memory) = make_heuristic_contradiction_case();
        let report = hybrid.detect(&update, &memory).await;

        // 阈值 0.0 + 非精确匹配 → 不去重，保留 2 条
        assert_eq!(
            report.count(),
            2,
            "阈值 0.0 禁用语义去重，非精确匹配应保留 2 条，实际: {}",
            report.count()
        );
    }

    #[tokio::test]
    async fn test_hybrid_detector_semantic_dedup_threshold_one_strict() {
        // v2.14：阈值 1.0 退化为精确匹配
        // 相同场景：相似度 0.778 < 1.0 → 不去重，保留 2 条
        let heuristic: Arc<dyn ConflictDetector> =
            Arc::new(crate::heuristic::HeuristicDetector::new());

        let mut llm_report = ConflictReport::empty();
        llm_report.push(ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Critical,
            description: "LLM 语义级检测".to_string(),
            existing_fact: Some("用户喜欢咖啡".to_string()),
            new_fact: "用户不再喜欢咖啡了".to_string(),
        });
        let llm: Arc<dyn ConflictDetector> = Arc::new(MockDetector::new(llm_report));

        // 阈值 1.0 严格精确匹配
        let hybrid = HybridDetector::with_dedup_threshold(heuristic, llm, 1.0);

        let (update, memory) = make_heuristic_contradiction_case();
        let report = hybrid.detect(&update, &memory).await;

        // 阈值 1.0 + 非精确匹配 → 不去重，保留 2 条
        assert_eq!(
            report.count(),
            2,
            "阈值 1.0 退化为精确匹配，非精确匹配应保留 2 条，实际: {}",
            report.count()
        );
    }

    #[tokio::test]
    async fn test_hybrid_detector_semantic_dedup_high_threshold_keeps_dissimilar() {
        // v2.14：高阈值 0.9 时，相似度 0.778 < 0.9 → 不去重
        let heuristic: Arc<dyn ConflictDetector> =
            Arc::new(crate::heuristic::HeuristicDetector::new());

        let mut llm_report = ConflictReport::empty();
        llm_report.push(ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Critical,
            description: "LLM 语义级检测".to_string(),
            existing_fact: Some("用户喜欢咖啡".to_string()),
            new_fact: "用户不再喜欢咖啡了".to_string(),
        });
        let llm: Arc<dyn ConflictDetector> = Arc::new(MockDetector::new(llm_report));

        let hybrid = HybridDetector::with_dedup_threshold(heuristic, llm, 0.9);

        let (update, memory) = make_heuristic_contradiction_case();
        let report = hybrid.detect(&update, &memory).await;

        assert_eq!(
            report.count(),
            2,
            "阈值 0.9 时相似度 0.778 < 0.9，不应去重，实际: {}",
            report.count()
        );
    }

    #[tokio::test]
    async fn test_hybrid_detector_semantic_dedup_low_threshold_catches_more() {
        // v2.14：低阈值 0.4 时，"用户不喜欢咖啡" vs "用户讨厌咖啡"（相似度 ≈ 0.444）应被去重
        let heuristic: Arc<dyn ConflictDetector> =
            Arc::new(crate::heuristic::HeuristicDetector::new());

        let mut llm_report = ConflictReport::empty();
        llm_report.push(ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Critical,
            description: "LLM 另一种表述".to_string(),
            existing_fact: Some("用户喜欢咖啡".to_string()),
            new_fact: "用户讨厌咖啡".to_string(), // 相似度 ≈ 0.444
        });
        let llm: Arc<dyn ConflictDetector> = Arc::new(MockDetector::new(llm_report));

        // 低阈值 0.4：0.444 > 0.4 → 去重
        let hybrid = HybridDetector::with_dedup_threshold(heuristic, llm, 0.4);

        let (update, memory) = make_heuristic_contradiction_case();
        let report = hybrid.detect(&update, &memory).await;

        assert_eq!(
            report.count(),
            1,
            "阈值 0.4 时相似度 0.444 > 0.4，应去重，实际: {}",
            report.count()
        );
    }

    #[tokio::test]
    async fn test_hybrid_detector_semantic_dedup_preserves_different_kind() {
        // v2.14：语义去重不影响不同 kind 的冲突
        // heuristic: DirectContradict("用户不喜欢咖啡")
        // LLM: StanceReversal("用户不再喜欢咖啡了") → kind 不同，即使相似度高也不去重
        let heuristic: Arc<dyn ConflictDetector> =
            Arc::new(crate::heuristic::HeuristicDetector::new());

        let mut llm_report = ConflictReport::empty();
        llm_report.push(ConflictRecord {
            kind: ConflictKind::StanceReversal, // 不同 kind
            severity: Severity::Warning,
            description: "LLM 立场反转".to_string(),
            existing_fact: Some("用户喜欢咖啡".to_string()),
            new_fact: "用户不再喜欢咖啡了".to_string(), // 相似度高但 kind 不同
        });
        let llm: Arc<dyn ConflictDetector> = Arc::new(MockDetector::new(llm_report));

        let hybrid = HybridDetector::new(heuristic, llm);

        let (update, memory) = make_heuristic_contradiction_case();
        let report = hybrid.detect(&update, &memory).await;

        // kind 不同 → 不去重，保留 2 条
        assert_eq!(
            report.count(),
            2,
            "kind 不同时即使相似度高也不应去重，实际: {}",
            report.count()
        );
    }

    // ========================================================================
    // v2.15 新增：多模式语义去重测试
    // ========================================================================

    #[test]
    fn test_dedup_mode_default_is_char() {
        // v2.15：DedupMode 默认值为 Char（向后兼容 v2.14）
        assert_eq!(DedupMode::default(), DedupMode::Char);
    }

    #[test]
    fn test_with_dedup_mode_sets_mode() {
        // v2.15：with_dedup_mode 正确设置模式
        let heuristic: Arc<dyn ConflictDetector> = Arc::new(NoopDetector::new());
        let llm: Arc<dyn ConflictDetector> = Arc::new(NoopDetector::new());

        let hybrid_word = HybridDetector::with_dedup_mode(heuristic.clone(), llm.clone(), 0.7, DedupMode::Word);
        assert_eq!(hybrid_word.dedup_mode(), DedupMode::Word);
        assert!(hybrid_word.embedder().is_none(), "Word 模式不应注入 Embedder");

        let hybrid_emb = HybridDetector::with_dedup_mode(heuristic, llm, 0.85, DedupMode::Embedding);
        assert_eq!(hybrid_emb.dedup_mode(), DedupMode::Embedding);
        assert!(hybrid_emb.embedder().is_none(), "Embedding 模式未注入时 embedder 应为 None");
    }

    #[test]
    fn test_with_embedder_sets_embedding_mode() {
        // v2.15：with_embedder 自动启用 Embedding 模式
        let heuristic: Arc<dyn ConflictDetector> = Arc::new(NoopDetector::new());
        let llm: Arc<dyn ConflictDetector> = Arc::new(NoopDetector::new());
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new_failing());

        let hybrid = HybridDetector::with_embedder(heuristic, llm, 0.85, embedder);
        assert_eq!(hybrid.dedup_mode(), DedupMode::Embedding);
        assert!(hybrid.embedder().is_some(), "with_embedder 应注入 Embedder");
    }

    #[test]
    fn test_similarity_word_chinese_bigram() {
        // v2.15：中文词级相似度（char_bigrams Jaccard）
        // "用户喜欢咖啡" → {用户, 户喜, 喜欢, 欢咖, 咖啡}
        // "用户喜欢茶叶" → {用户, 户喜, 喜欢, 欢茶, 茶叶}
        // intersection = {用户, 户喜, 喜欢} = 3
        // union = 7
        // jaccard = 3/7 ≈ 0.429
        let sim = similarity_word("用户喜欢咖啡", "用户喜欢茶叶");
        assert!(
            sim > 0.42 && sim < 0.44,
            "中文 bigram 相似度应在 0.429 附近，实际: {}",
            sim
        );
    }

    #[test]
    fn test_similarity_word_latin_tokens() {
        // v2.15：拉丁词级相似度（word_tokens Jaccard）
        // "I love coffee" → {i, love, coffee}
        // "I love tea" → {i, love, tea}
        // intersection = {i, love} = 2
        // union = 4
        // jaccard = 0.5
        let sim = similarity_word("I love coffee", "I love tea");
        assert!(
            sim > 0.49 && sim < 0.51,
            "拉丁词级相似度应在 0.5 附近，实际: {}",
            sim
        );
    }

    #[test]
    fn test_similarity_word_order_sensitive() {
        // v2.15：词级相似度保留顺序信息（与 similarity_char 的关键差异）
        // "不喜欢" → {不喜, 喜欢}
        // "欢不喜" → {欢不, 不喜}
        // intersection = {不喜} = 1
        // union = 3
        // jaccard = 1/3 ≈ 0.333
        let sim_word = similarity_word("不喜欢", "欢不喜");
        let sim_char = similarity_char("不喜欢", "欢不喜");
        assert!(
            sim_word < sim_char,
            "Word 模式相似度 {} 应低于 Char 模式 {}（顺序不同应被识别）",
            sim_word,
            sim_char
        );
        assert_eq!(sim_char, 1.0, "Char 模式不区分顺序，字符集合相同应为 1.0");
        assert!(
            sim_word > 0.0 && sim_word < 0.5,
            "Word 模式应识别顺序差异，相似度 {} 应在 (0, 0.5) 区间",
            sim_word
        );
    }

    #[tokio::test]
    async fn test_hybrid_detector_word_mode_dedup() {
        // v2.15：Word 模式语义去重
        // heuristic: DirectContradict("用户不喜欢咖啡")
        // LLM: DirectContradict("用户不再喜欢咖啡了")
        // Word 模式相似度（char_bigrams）：
        //   "用户不喜欢咖啡" → {用户, 户不, 不喜, 喜欢, 欢咖, 咖啡} = 6
        //   "用户不再喜欢咖啡了" → {用户, 户不, 不再, 再喜, 喜欢, 欢咖, 咖啡, 啡了} = 8
        //   intersection = {用户, 户不, 不喜(无), 喜欢, 欢咖, 咖啡} = 5
        //   union = 9
        //   jaccard = 5/9 ≈ 0.556
        // 阈值 0.4：0.556 > 0.4 → 去重
        let heuristic: Arc<dyn ConflictDetector> =
            Arc::new(crate::heuristic::HeuristicDetector::new());

        let mut llm_report = ConflictReport::empty();
        llm_report.push(ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Critical,
            description: "LLM 另一种表述".to_string(),
            existing_fact: Some("用户喜欢咖啡".to_string()),
            new_fact: "用户不再喜欢咖啡了".to_string(),
        });
        let llm: Arc<dyn ConflictDetector> = Arc::new(MockDetector::new(llm_report));

        let hybrid = HybridDetector::with_dedup_mode(
            heuristic,
            llm,
            0.4,
            DedupMode::Word,
        );

        let (update, memory) = make_heuristic_contradiction_case();
        let report = hybrid.detect(&update, &memory).await;

        assert_eq!(
            report.count(),
            1,
            "Word 模式阈值 0.4 时相似度 ≈ 0.556 > 0.4，应去重，实际: {}",
            report.count()
        );
    }

    #[tokio::test]
    async fn test_hybrid_detector_embedding_mode_success() {
        // v2.15：Embedding 模式成功场景
        // MockEmbedder 返回高相似度向量 → 去重生效
        let heuristic: Arc<dyn ConflictDetector> =
            Arc::new(crate::heuristic::HeuristicDetector::new());

        let mut llm_report = ConflictReport::empty();
        llm_report.push(ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Critical,
            description: "LLM 语义级重复".to_string(),
            existing_fact: Some("用户喜欢咖啡".to_string()),
            new_fact: "用户不再喜欢咖啡了".to_string(),
        });
        let llm: Arc<dyn ConflictDetector> = Arc::new(MockDetector::new(llm_report));

        // MockEmbedder 返回相同向量（cosine = 1.0）
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new_identical());
        let hybrid = HybridDetector::with_embedder(heuristic, llm, 0.85, embedder);

        let (update, memory) = make_heuristic_contradiction_case();
        let report = hybrid.detect(&update, &memory).await;

        assert_eq!(
            report.count(),
            1,
            "Embedding 模式成功时 cosine=1.0 > 0.85，应去重，实际: {}",
            report.count()
        );
    }

    #[tokio::test]
    async fn test_hybrid_detector_embedding_mode_fallback_on_failure() {
        // v2.15：Embedding 模式降级场景
        // MockEmbedder 失败 → 降级到 Word 模式
        // 验证：降级后仍能用 Word 模式去重（相似度 0.556 > 阈值 0.4）
        let heuristic: Arc<dyn ConflictDetector> =
            Arc::new(crate::heuristic::HeuristicDetector::new());

        let mut llm_report = ConflictReport::empty();
        llm_report.push(ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Critical,
            description: "LLM 语义级重复".to_string(),
            existing_fact: Some("用户喜欢咖啡".to_string()),
            new_fact: "用户不再喜欢咖啡了".to_string(),
        });
        let llm: Arc<dyn ConflictDetector> = Arc::new(MockDetector::new(llm_report));

        // MockEmbedder 失败 → 降级到 Word
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new_failing());
        let hybrid = HybridDetector::with_embedder(heuristic, llm, 0.4, embedder);

        let (update, memory) = make_heuristic_contradiction_case();
        let report = hybrid.detect(&update, &memory).await;

        // 降级到 Word 后相似度 0.556 > 0.4 → 去重
        assert_eq!(
            report.count(),
            1,
            "Embedding 失败降级到 Word 后，相似度 0.556 > 0.4 应去重，实际: {}",
            report.count()
        );
    }

    #[tokio::test]
    async fn test_hybrid_detector_embedding_mode_without_embedder_falls_back() {
        // v2.15：Embedding 模式但未注入 Embedder → 降级到 Word
        let heuristic: Arc<dyn ConflictDetector> =
            Arc::new(crate::heuristic::HeuristicDetector::new());

        let mut llm_report = ConflictReport::empty();
        llm_report.push(ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Critical,
            description: "LLM 语义级重复".to_string(),
            existing_fact: Some("用户喜欢咖啡".to_string()),
            new_fact: "用户不再喜欢咖啡了".to_string(),
        });
        let llm: Arc<dyn ConflictDetector> = Arc::new(MockDetector::new(llm_report));

        // with_dedup_mode 设 Embedding 但不注入 embedder
        let hybrid = HybridDetector::with_dedup_mode(
            heuristic,
            llm,
            0.4,
            DedupMode::Embedding,
        );

        let (update, memory) = make_heuristic_contradiction_case();
        let report = hybrid.detect(&update, &memory).await;

        // 未注入 Embedder → 降级到 Word → 0.556 > 0.4 → 去重
        assert_eq!(
            report.count(),
            1,
            "Embedding 模式未注入 Embedder 应降级到 Word，相似度 0.556 > 0.4 应去重，实际: {}",
            report.count()
        );
    }

    /// Mock Embedder（v2.15 测试辅助）
    ///
    /// 两种模式：
    /// - `Identical`：所有文本返回相同向量（cosine = 1.0）
    /// - `Failing`：embed_batch 返回 Err（模拟网络/API 错误）
    enum MockEmbedder {
        Identical,
        Failing,
    }

    impl MockEmbedder {
        fn new_identical() -> Self {
            Self::Identical
        }
        fn new_failing() -> Self {
            Self::Failing
        }
    }

    #[async_trait::async_trait]
    impl Embedder for MockEmbedder {
        fn dim(&self) -> usize {
            3
        }

        async fn embed(&self, _text: &str) -> crate::Result<Vec<f32>> {
            match self {
                Self::Identical => Ok(vec![1.0, 0.0, 0.0]),
                Self::Failing => Err(crate::Error::Storage("MockEmbedder 故意失败".into())),
            }
        }

        async fn embed_batch(&self, texts: &[&str]) -> crate::Result<Vec<Vec<f32>>> {
            match self {
                Self::Identical => {
                    let mut results = Vec::with_capacity(texts.len());
                    for _ in 0..texts.len() {
                        results.push(vec![1.0, 0.0, 0.0]);
                    }
                    Ok(results)
                }
                Self::Failing => Err(crate::Error::Storage("MockEmbedder batch 故意失败".into())),
            }
        }
    }
}

// ============================================================================
// v2.28 字段级 merge 单元测试
// ============================================================================

#[cfg(test)]
mod v2_28_merge_tests {
    use super::*;
    use crate::model::{ArchivePeriod, MessageContent, MessageTurn};
    use chrono::Utc;
    use uuid::Uuid;

    /// 构造测试用 MemoryFile（复用主测试模块的结构）
    fn make_test_memory() -> MemoryFile {
        let turn = MessageTurn {
            id: Uuid::new_v4(),
            user_message: MessageContent {
                text: Some("用户消息".to_string()),
                attachments: vec![],
                tool_calls: vec![],
                thinking: None,
            },
            llm_message: MessageContent {
                text: Some("助手回复".to_string()),
                attachments: vec![],
                tool_calls: vec![],
                thinking: None,
            },
            tags: vec![],
            timestamp: Utc::now(),
            token_count: 100,
        };
        MemoryFile {
            id: Uuid::new_v4(),
            schema_version: 1,
            archived_at: Utc::now(),
            session_id: "test-sess".to_string(),
            project_id: None,
            turns: vec![turn],
            tags: vec![],
            total_tokens: 100,
            truncated: false,
            period: ArchivePeriod::Daily,
            access_count: 0,
            importance: 0,
            updates: vec![],
        }
    }

    /// 简单 mock 检测器：返回预设报告
    struct MockDetector {
        report: ConflictReport,
    }
    impl MockDetector {
        fn new(report: ConflictReport) -> Self {
            Self { report }
        }
    }
    #[async_trait]
    impl ConflictDetector for MockDetector {
        async fn detect(
            &self,
            _update: &MemoryUpdate,
            _existing: &MemoryFile,
        ) -> ConflictReport {
            self.report.clone()
        }
    }

    #[test]
    fn test_merge_severity_takes_higher() {
        // 启发式 Warning + LLM Critical → 应取 Critical
        let mut existing = ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Warning,
            description: "启发式：反义词匹配".to_string(),
            existing_fact: Some("用户喜欢咖啡".to_string()),
            new_fact: "用户不喜欢咖啡".to_string(),
        };
        let incoming = ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Critical,
            description: "LLM：立场反转".to_string(),
            existing_fact: Some("用户明确表达喜欢咖啡".to_string()),
            new_fact: "用户不喜欢咖啡".to_string(),
        };
        HybridDetector::merge_conflict_fields(&mut existing, &incoming);
        assert_eq!(existing.severity, Severity::Critical);
    }

    #[test]
    fn test_merge_severity_keeps_higher_when_existing_is_higher() {
        // 启发式 Critical + LLM Warning → 应保留 Critical
        let mut existing = ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Critical,
            description: "启发式：反义词匹配".to_string(),
            existing_fact: None,
            new_fact: "用户不喜欢咖啡".to_string(),
        };
        let incoming = ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Warning,
            description: "LLM：可能矛盾".to_string(),
            existing_fact: Some("用户喜欢咖啡".to_string()),
            new_fact: "用户不喜欢咖啡".to_string(),
        };
        HybridDetector::merge_conflict_fields(&mut existing, &incoming);
        assert_eq!(existing.severity, Severity::Critical);
    }

    #[test]
    fn test_merge_description_prefers_llm_when_longer() {
        let mut existing = ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Critical,
            description: "短".to_string(),
            existing_fact: None,
            new_fact: "用户不喜欢咖啡".to_string(),
        };
        let incoming = ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Critical,
            description: "LLM 提供的更详细的语义化描述".to_string(),
            existing_fact: None,
            new_fact: "用户不喜欢咖啡".to_string(),
        };
        HybridDetector::merge_conflict_fields(&mut existing, &incoming);
        assert_eq!(existing.description, "LLM 提供的更详细的语义化描述");
    }

    #[test]
    fn test_merge_description_keeps_existing_when_llm_shorter() {
        let mut existing = ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Critical,
            description: "启发式提供了较长的描述".to_string(),
            existing_fact: None,
            new_fact: "用户不喜欢咖啡".to_string(),
        };
        let incoming = ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Critical,
            description: "短".to_string(),
            existing_fact: None,
            new_fact: "用户不喜欢咖啡".to_string(),
        };
        HybridDetector::merge_conflict_fields(&mut existing, &incoming);
        assert_eq!(existing.description, "启发式提供了较长的描述");
    }

    #[test]
    fn test_merge_existing_fact_prefers_llm_some() {
        // 启发式 None + LLM Some → 应取 LLM 的
        let mut existing = ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Critical,
            description: "测试".to_string(),
            existing_fact: None,
            new_fact: "用户不喜欢咖啡".to_string(),
        };
        let incoming = ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Critical,
            description: "测试".to_string(),
            existing_fact: Some("用户明确表达过喜欢咖啡".to_string()),
            new_fact: "用户不喜欢咖啡".to_string(),
        };
        HybridDetector::merge_conflict_fields(&mut existing, &incoming);
        assert_eq!(
            existing.existing_fact,
            Some("用户明确表达过喜欢咖啡".to_string())
        );
    }

    #[test]
    fn test_merge_existing_fact_keeps_existing_when_llm_none() {
        // 启发式 Some + LLM None → 应保留启发式的
        let mut existing = ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Critical,
            description: "测试".to_string(),
            existing_fact: Some("启发式引用的历史事实".to_string()),
            new_fact: "用户不喜欢咖啡".to_string(),
        };
        let incoming = ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Critical,
            description: "测试".to_string(),
            existing_fact: None,
            new_fact: "用户不喜欢咖啡".to_string(),
        };
        HybridDetector::merge_conflict_fields(&mut existing, &incoming);
        assert_eq!(
            existing.existing_fact,
            Some("启发式引用的历史事实".to_string())
        );
    }

    #[test]
    fn test_merge_empty_description_does_not_overwrite() {
        // LLM description 为空 → 不覆盖启发式
        let mut existing = ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Critical,
            description: "启发式描述".to_string(),
            existing_fact: None,
            new_fact: "用户不喜欢咖啡".to_string(),
        };
        let incoming = ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Critical,
            description: "".to_string(),
            existing_fact: None,
            new_fact: "用户不喜欢咖啡".to_string(),
        };
        HybridDetector::merge_conflict_fields(&mut existing, &incoming);
        assert_eq!(existing.description, "启发式描述");
    }

    /// v2.28 集成测试：HybridDetector 字段级 merge 完整流程
    #[tokio::test]
    async fn test_hybrid_detector_field_merge_integration() {
        // 启发式：Warning + 短描述 + 无 existing_fact
        let mut heuristic_report = ConflictReport::empty();
        heuristic_report.push(ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Warning,
            description: "反义词".to_string(),
            existing_fact: None,
            new_fact: "用户不喜欢咖啡".to_string(),
        });
        let heuristic: Arc<dyn ConflictDetector> = Arc::new(MockDetector::new(heuristic_report));

        // LLM：Critical + 长描述 + 有 existing_fact（同 kind + 同 new_fact → 触发字段级 merge）
        let mut llm_report = ConflictReport::empty();
        llm_report.push(ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Critical,
            description: "LLM 语义分析：用户立场明确反转".to_string(),
            existing_fact: Some("用户上周明确说喜欢咖啡".to_string()),
            new_fact: "用户不喜欢咖啡".to_string(),
        });
        let llm: Arc<dyn ConflictDetector> = Arc::new(MockDetector::new(llm_report));

        // dedup_threshold=0.0 → 仅精确匹配（new_fact 完全相同 → 重复 → 触发 merge）
        let hybrid = HybridDetector::with_dedup_threshold(heuristic, llm, 0.0);

        let update = MemoryUpdate::new().add_fact("用户不喜欢咖啡".to_string());
        let memory = make_test_memory();

        let report = hybrid.detect(&update, &memory).await;

        // 应该只有 1 条冲突（重复触发 merge，而非 push 两条）
        assert_eq!(report.count(), 1, "字段级 merge 后应只有 1 条冲突");
        // severity 应升级为 Critical（取 max）
        assert_eq!(report.conflicts[0].severity, Severity::Critical);
        // description 应为 LLM 的（更长）
        assert_eq!(
            report.conflicts[0].description,
            "LLM 语义分析：用户立场明确反转"
        );
        // existing_fact 应为 LLM 的 Some
        assert_eq!(
            report.conflicts[0].existing_fact,
            Some("用户上周明确说喜欢咖啡".to_string())
        );
    }

    /// v2.28 集成测试：LLM 独有冲突仍正常 push
    #[tokio::test]
    async fn test_hybrid_detector_llm_unique_conflict_still_pushed() {
        // 启发式：1 条 DirectContradict
        let mut heuristic_report = ConflictReport::empty();
        heuristic_report.push(ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Warning,
            description: "启发式检测".to_string(),
            existing_fact: None,
            new_fact: "用户不喜欢咖啡".to_string(),
        });
        let heuristic: Arc<dyn ConflictDetector> = Arc::new(MockDetector::new(heuristic_report));

        // LLM：1 条 StanceReversal（不同 kind → 不重复 → 直接 push）
        let mut llm_report = ConflictReport::empty();
        llm_report.push(ConflictRecord {
            kind: ConflictKind::StanceReversal,
            severity: Severity::Critical,
            description: "LLM 检测到立场反转".to_string(),
            existing_fact: Some("历史记录显示用户喜欢咖啡".to_string()),
            new_fact: "用户不喜欢咖啡".to_string(),
        });
        let llm: Arc<dyn ConflictDetector> = Arc::new(MockDetector::new(llm_report));

        let hybrid = HybridDetector::with_dedup_threshold(heuristic, llm, 0.0);

        let update = MemoryUpdate::new().add_fact("用户不喜欢咖啡".to_string());
        let memory = make_test_memory();

        let report = hybrid.detect(&update, &memory).await;

        // 应有 2 条冲突（启发式 1 + LLM 独有 1）
        assert_eq!(report.count(), 2, "独有冲突应直接 push，共 2 条");
    }
}
