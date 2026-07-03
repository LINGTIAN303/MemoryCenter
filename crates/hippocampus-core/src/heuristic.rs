//! # 启发式冲突检测器（v2.6 批次 8）
//!
//! 默认的纯算法冲突检测实现，无 LLM 依赖。
//!
//! ## 三维度检测
//!
//! 1. **自我矛盾（SelfContradict）**：同一批 update 内 added 与 deprecated 包含相同/相似事实
//! 2. **直接矛盾（DirectContradict）**：added_facts 与历史事实集通过反义词词典匹配到对立
//! 3. **立场反转（StanceReversal）**：deprecated_facts 与历史 added_facts 精确/相似匹配
//!
//! ## 反义词词典
//!
//! 内置约 30 对中文常见反义词（喜好/是非/状态/程度/存在等），
//! 检测时对事实文本分词后查找反义词对。
//!
//! ## 相似度算法
//!
//! - **精确匹配**：normalize 后完全相等（去空白 + 小写）
//! - **包含匹配**：一方包含另一方（短串作为子串）
//! - **Jaccard 相似度**：分词后 token 集合的 Jaccard 系数 ≥ 0.6

use crate::conflict::{ConflictDetector, ConflictKind, ConflictRecord, ConflictReport, Severity};
use crate::model::{MemoryFile, MemoryUpdate};
use async_trait::async_trait;
use std::collections::HashSet;

// ============================================================================
// 反义词词典
// ============================================================================

/// 中文反义词对（约 30 对）
///
/// 每对 (a, b) 表示 a 与 b 语义相反。
/// 检测时若 added_fact 含 a，existing_fact 含 b（或反之），则判定为直接矛盾。
const ANTONYM_PAIRS: &[(&str, &str)] = &[
    // 喜好
    ("喜欢", "不喜欢"),
    ("爱", "恨"),
    ("支持", "反对"),
    ("赞同", "反对"),
    ("赞成", "反对"),
    ("偏好", "排斥"),
    // 是非
    ("是", "不是"),
    ("对", "错"),
    ("正确", "错误"),
    ("真", "假"),
    ("有效", "无效"),
    ("合法", "非法"),
    // 状态
    ("开", "关"),
    ("启用", "禁用"),
    ("启动", "停止"),
    ("开始", "结束"),
    ("运行", "停止"),
    ("连接", "断开"),
    // 程度
    ("增加", "减少"),
    ("上升", "下降"),
    ("变大", "变小"),
    ("增强", "减弱"),
    ("加速", "减速"),
    // 存在
    ("有", "没有"),
    ("存在", "不存在"),
    ("包含", "不包含"),
    // 其他
    ("成功", "失败"),
    ("允许", "禁止"),
    ("接受", "拒绝"),
    ("同意", "拒绝"),
    ("肯定", "否定"),
];

// ============================================================================
// HeuristicDetector
// ============================================================================

/// 启发式冲突检测器（默认实现，纯算法）
///
/// 通过反义词词典 + 字符串相似度检测三维度冲突，无 LLM 依赖。
///
/// ## 使用
///
/// ```rust,ignore
/// use hippocampus_core::conflict::ConflictDetector;
/// use hippocampus_core::heuristic::HeuristicDetector;
///
/// let detector = HeuristicDetector::new();
/// let report = detector.detect(&update, &memory).await;
/// ```
#[derive(Debug, Default, Clone)]
pub struct HeuristicDetector;

impl HeuristicDetector {
    /// 创建启发式检测器
    pub fn new() -> Self {
        Self::default()
    }

    /// 维度 1：自我矛盾检测
    ///
    /// 同一批 update 内 added_facts 与 deprecated_facts 包含相同/相似事实。
    /// 严重级别：Critical（明确矛盾）
    fn detect_self_contradiction(update: &MemoryUpdate, report: &mut ConflictReport) {
        for added in &update.added_facts {
            for deprecated in &update.deprecated_facts {
                let sim = similarity(added, deprecated);
                if sim >= 0.8 {
                    report.push(ConflictRecord {
                        kind: ConflictKind::SelfContradict,
                        severity: Severity::Critical,
                        description: format!(
                            "同一批更新中既添加又废弃相似事实（相似度 {:.0}%）",
                            sim * 100.0
                        ),
                        existing_fact: Some(deprecated.clone()),
                        new_fact: added.clone(),
                    });
                }
            }
        }
    }

    /// 维度 2：直接矛盾检测
    ///
    /// added_facts 与历史事实集通过反义词词典匹配到对立。
    /// 严重级别：Critical（明确矛盾）
    fn detect_direct_contradiction(
        update: &MemoryUpdate,
        historical_facts: &[String],
        report: &mut ConflictReport,
    ) {
        for added in &update.added_facts {
            for existing in historical_facts {
                if let Some(antonym_kind) = check_antonym(added, existing) {
                    report.push(ConflictRecord {
                        kind: ConflictKind::DirectContradict,
                        severity: Severity::Critical,
                        description: format!(
                            "新事实与已有事实构成直接矛盾（{}）",
                            antonym_kind
                        ),
                        existing_fact: Some(existing.clone()),
                        new_fact: added.clone(),
                    });
                }
            }
        }
    }

    /// 维度 3：立场反转检测
    ///
    /// deprecated_facts 与历史 added_facts 精确/相似匹配（相似度 ≥ 0.6）。
    /// 严重级别：Warning（可能是有意修正，需关注）
    fn detect_stance_reversal(
        update: &MemoryUpdate,
        historical_added: &[String],
        report: &mut ConflictReport,
    ) {
        for deprecated in &update.deprecated_facts {
            for existing in historical_added {
                let sim = similarity(deprecated, existing);
                if sim >= 0.6 {
                    report.push(ConflictRecord {
                        kind: ConflictKind::StanceReversal,
                        severity: Severity::Warning,
                        description: format!(
                            "废弃的事实与历史已添加事实高度相似（相似度 {:.0}%），可能是立场反转",
                            sim * 100.0
                        ),
                        existing_fact: Some(existing.clone()),
                        new_fact: deprecated.clone(),
                    });
                }
            }
        }
    }

    /// 从 MemoryFile 中提取历史事实集
    ///
    /// 汇总所有历史 MemoryUpdateRecord 中的 added_facts，
    /// 作为"已有事实"的近似集合。
    fn extract_historical_facts(memory: &MemoryFile) -> Vec<String> {
        let mut facts: Vec<String> = Vec::new();
        for record in &memory.updates {
            facts.extend(record.update.added_facts.iter().cloned());
        }
        facts
    }
}

#[async_trait]
impl ConflictDetector for HeuristicDetector {
    async fn detect(
        &self,
        update: &MemoryUpdate,
        existing_memory: &MemoryFile,
    ) -> ConflictReport {
        let mut report = ConflictReport::empty();

        // 维度 1：自我矛盾（update 内部 added vs deprecated）
        Self::detect_self_contradiction(update, &mut report);

        // 提取历史事实集
        let historical_facts = Self::extract_historical_facts(existing_memory);

        // 维度 2：直接矛盾（added vs 历史事实，反义词匹配）
        Self::detect_direct_contradiction(update, &historical_facts, &mut report);

        // 维度 3：立场反转（deprecated vs 历史 added，相似度匹配）
        Self::detect_stance_reversal(update, &historical_facts, &mut report);

        report
    }
}

// ============================================================================
// 辅助函数：相似度 + 反义词匹配
// ============================================================================

/// 字符串归一化（去首尾空白 + 转小写）
fn normalize(s: &str) -> String {
    s.trim().to_lowercase()
}

/// 计算两个事实的相似度（0.0 ~ 1.0）
///
/// 三级匹配：
/// 1. 精确匹配（归一化后相等）→ 1.0
/// 2. 包含匹配（短串是长串的子串）→ 0.9
/// 3. Jaccard 相似度（字符 bigram 集合）→ 0.0 ~ 1.0
fn similarity(a: &str, b: &str) -> f64 {
    let na = normalize(a);
    let nb = normalize(b);

    if na.is_empty() || nb.is_empty() {
        return 0.0;
    }

    // 1. 精确匹配
    if na == nb {
        return 1.0;
    }

    // 2. 包含匹配（短串作为子串）
    let (short, long) = if na.len() <= nb.len() {
        (&na, &nb)
    } else {
        (&nb, &na)
    };
    if long.contains(short) {
        return 0.9;
    }

    // 3. Jaccard 相似度（字符 bigram）
    let bigrams_a = char_bigrams(&na);
    let bigrams_b = char_bigrams(&nb);
    jaccard(&bigrams_a, &bigrams_b)
}

/// 生成字符串的字符 bigram 集合
///
/// 例如 "abc" → {"ab", "bc"}
fn char_bigrams(s: &str) -> HashSet<String> {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() < 2 {
        let mut set = HashSet::new();
        if !s.is_empty() {
            set.insert(s.to_string());
        }
        return set;
    }
    (0..chars.len() - 1)
        .map(|i| format!("{}{}", chars[i], chars[i + 1]))
        .collect()
}

/// 计算 Jaccard 相似度
///
/// `J(A, B) = |A ∩ B| / |A ∪ B|`
fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
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

/// 事实的极性（针对某反义词对）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Polarity {
    /// 正面（含 pos 但不含 neg）
    Positive,
    /// 负面（含 neg）
    Negative,
    /// 中性（都不含，或同时含 pos 和 neg 但 neg 优先）
    Neutral,
}

/// 判定事实相对某反义词对的极性
///
/// 优先匹配更长的 neg（如"不喜欢"优先于"喜欢"），
/// 避免"不喜欢"被误判为"喜欢"。
fn polarity(s: &str, pos: &str, neg: &str) -> Polarity {
    // 优先检查 neg（通常更长，如"不喜欢"包含"喜欢"）
    if s.contains(neg) {
        return Polarity::Negative;
    }
    if s.contains(pos) {
        return Polarity::Positive;
    }
    Polarity::Neutral
}

/// 检查两个事实是否构成反义词对立
///
/// 返回 `Some(描述)` 表示构成对立，`None` 表示无对立。
fn check_antonym(a: &str, b: &str) -> Option<&'static str> {
    let na = normalize(a);
    let nb = normalize(b);

    for (pos, neg) in ANTONYM_PAIRS {
        let pa = polarity(&na, pos, neg);
        let pb = polarity(&nb, pos, neg);

        // 一正一负 → 对立
        if (pa == Polarity::Positive && pb == Polarity::Negative)
            || (pa == Polarity::Negative && pb == Polarity::Positive)
        {
            return Some("反义词对立");
        }
    }
    None
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ArchivePeriod, MemoryUpdateRecord, MessageContent, MessageTurn};
    use chrono::Utc;
    use uuid::Uuid;

    /// 构造测试用 MemoryFile
    fn make_memory_with_updates(updates: Vec<MemoryUpdate>) -> MemoryFile {
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

        let update_records: Vec<MemoryUpdateRecord> = updates
            .into_iter()
            .map(|u| MemoryUpdateRecord {
                updated_at: Utc::now(),
                update: u,
                conflicts: vec![],
            })
            .collect();

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
            updates: update_records,
        }
    }

    // ------------------------------------------------------------------------
    // 相似度函数测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_similarity_exact_match() {
        assert_eq!(similarity("用户喜欢咖啡", "用户喜欢咖啡"), 1.0);
        assert_eq!(similarity("Hello", "hello"), 1.0);
        assert_eq!(similarity("  spaced  ", "spaced"), 1.0);
    }

    #[test]
    fn test_similarity_contains() {
        assert_eq!(similarity("用户喜欢咖啡", "喜欢咖啡"), 0.9);
        assert_eq!(similarity("启用", "系统启用日志"), 0.9);
    }

    #[test]
    fn test_similarity_jaccard() {
        let sim = similarity("用户喜欢喝咖啡", "用户喜欢喝茶");
        assert!(sim > 0.4 && sim < 0.9, "Jaccard 相似度应在 0.4~0.9 之间，实际: {}", sim);
    }

    #[test]
    fn test_similarity_empty() {
        assert_eq!(similarity("", "abc"), 0.0);
        assert_eq!(similarity("abc", ""), 0.0);
        assert_eq!(similarity("", ""), 0.0);
    }

    #[test]
    fn test_char_bigrams() {
        let bg = char_bigrams("abcd");
        assert_eq!(bg.len(), 3);
        assert!(bg.contains("ab"));
        assert!(bg.contains("bc"));
        assert!(bg.contains("cd"));
    }

    #[test]
    fn test_jaccard_disjoint() {
        let a: HashSet<String> = ["ab", "cd"].iter().map(|s| s.to_string()).collect();
        let b: HashSet<String> = ["ef", "gh"].iter().map(|s| s.to_string()).collect();
        assert_eq!(jaccard(&a, &b), 0.0);
    }

    #[test]
    fn test_jaccard_identical() {
        let a: HashSet<String> = ["ab", "cd"].iter().map(|s| s.to_string()).collect();
        assert_eq!(jaccard(&a, &a), 1.0);
    }

    // ------------------------------------------------------------------------
    // 反义词匹配测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_antonym_like_dislike() {
        assert!(check_antonym("用户喜欢咖啡", "用户不喜欢咖啡").is_some());
        assert!(check_antonym("不喜欢咖啡", "喜欢咖啡").is_some());
    }

    #[test]
    fn test_antonym_enable_disable() {
        assert!(check_antonym("启用功能", "禁用功能").is_some());
    }

    #[test]
    fn test_antonym_no_match() {
        assert!(check_antonym("用户喜欢咖啡", "用户喜欢茶").is_none());
        assert!(check_antonym("今天天气不错", "明天可能下雨").is_none());
    }

    #[test]
    fn test_antonym_same_polarity_no_conflict() {
        // 两个都包含"不喜欢"（同负面）不应判定为对立
        assert!(check_antonym("用户不喜欢咖啡", "不喜欢咖啡的口味").is_none());
        // 两个都包含"喜欢"（同正面）不应判定为对立
        assert!(check_antonym("用户喜欢咖啡", "喜欢咖啡的口味").is_none());
    }

    // ------------------------------------------------------------------------
    // HeuristicDetector 三维度测试
    // ------------------------------------------------------------------------

    #[tokio::test]
    async fn test_detect_self_contradiction() {
        let detector = HeuristicDetector::new();
        let memory = make_memory_with_updates(vec![]);

        // added 和 deprecated 包含相同事实
        let update = MemoryUpdate::new()
            .add_fact("用户喜欢咖啡")
            .deprecate_fact("用户喜欢咖啡");

        let report = detector.detect(&update, &memory).await;
        assert_eq!(report.count(), 1);
        assert_eq!(report.conflicts[0].kind, ConflictKind::SelfContradict);
        assert_eq!(report.conflicts[0].severity, Severity::Critical);
    }

    #[tokio::test]
    async fn test_detect_self_contradiction_similar() {
        let detector = HeuristicDetector::new();
        let memory = make_memory_with_updates(vec![]);

        // added 和 deprecated 相似但不完全相同（Jaccard ≥ 0.8）
        let update = MemoryUpdate::new()
            .add_fact("用户喜欢喝咖啡")
            .deprecate_fact("用户喜欢喝咖啡豆");

        let report = detector.detect(&update, &memory).await;
        assert!(
            !report.is_clean(),
            "高相似度事实应检测到自我矛盾"
        );
        assert_eq!(report.conflicts[0].kind, ConflictKind::SelfContradict);
    }

    #[tokio::test]
    async fn test_detect_self_contradiction_no_match() {
        let detector = HeuristicDetector::new();
        let memory = make_memory_with_updates(vec![]);

        let update = MemoryUpdate::new()
            .add_fact("用户喜欢咖啡")
            .deprecate_fact("系统启用日志");

        let report = detector.detect(&update, &memory).await;
        assert!(report.is_clean(), "不相似的事实不应判定为自我矛盾");
    }

    #[tokio::test]
    async fn test_detect_direct_contradiction() {
        let detector = HeuristicDetector::new();

        // 历史事实：用户喜欢咖啡
        let historical = MemoryUpdate::new().add_fact("用户喜欢咖啡");
        let memory = make_memory_with_updates(vec![historical]);

        // 新事实：用户不喜欢咖啡
        let update = MemoryUpdate::new().add_fact("用户不喜欢咖啡");

        let report = detector.detect(&update, &memory).await;
        assert_eq!(report.count(), 1);
        assert_eq!(report.conflicts[0].kind, ConflictKind::DirectContradict);
        assert_eq!(report.conflicts[0].severity, Severity::Critical);
        assert_eq!(
            report.conflicts[0].existing_fact.as_deref(),
            Some("用户喜欢咖啡")
        );
    }

    #[tokio::test]
    async fn test_detect_direct_contradiction_enable_disable() {
        let detector = HeuristicDetector::new();

        let historical = MemoryUpdate::new().add_fact("系统启用日志记录");
        let memory = make_memory_with_updates(vec![historical]);

        let update = MemoryUpdate::new().add_fact("系统禁用日志记录");

        let report = detector.detect(&update, &memory).await;
        assert_eq!(report.count(), 1);
        assert_eq!(report.conflicts[0].kind, ConflictKind::DirectContradict);
    }

    #[tokio::test]
    async fn test_detect_direct_contradiction_no_history() {
        let detector = HeuristicDetector::new();
        let memory = make_memory_with_updates(vec![]);

        // 无历史事实，不应检测到直接矛盾
        let update = MemoryUpdate::new().add_fact("用户不喜欢咖啡");
        let report = detector.detect(&update, &memory).await;
        assert!(report.is_clean());
    }

    #[tokio::test]
    async fn test_detect_stance_reversal() {
        let detector = HeuristicDetector::new();

        // 历史已添加：用户偏好深色主题
        let historical = MemoryUpdate::new().add_fact("用户偏好深色主题");
        let memory = make_memory_with_updates(vec![historical]);

        // 现在废弃：用户偏好深色主题
        let update = MemoryUpdate::new().deprecate_fact("用户偏好深色主题");

        let report = detector.detect(&update, &memory).await;
        assert_eq!(report.count(), 1);
        assert_eq!(report.conflicts[0].kind, ConflictKind::StanceReversal);
        assert_eq!(report.conflicts[0].severity, Severity::Warning);
    }

    #[tokio::test]
    async fn test_detect_stance_reversal_similar() {
        let detector = HeuristicDetector::new();

        let historical = MemoryUpdate::new().add_fact("用户使用 Python 编程");
        let memory = make_memory_with_updates(vec![historical]);

        // 相似但不完全相同
        let update = MemoryUpdate::new().deprecate_fact("用户使用 Python 编程语言");

        let report = detector.detect(&update, &memory).await;
        assert!(!report.is_clean(), "高相似度应触发立场反转");
        assert_eq!(report.conflicts[0].kind, ConflictKind::StanceReversal);
    }

    // ------------------------------------------------------------------------
    // 综合测试
    // ------------------------------------------------------------------------

    #[tokio::test]
    async fn test_detect_multiple_conflicts() {
        let detector = HeuristicDetector::new();

        // 历史：喜欢咖啡 + 启用日志
        let historical = MemoryUpdate::new()
            .add_fact("用户喜欢咖啡")
            .add_fact("系统启用日志");
        let memory = make_memory_with_updates(vec![historical]);

        // 新 update：不喜欢咖啡（直接矛盾）+ 废弃启用日志（立场反转）
        let update = MemoryUpdate::new()
            .add_fact("用户不喜欢咖啡")
            .deprecate_fact("系统启用日志");

        let report = detector.detect(&update, &memory).await;
        assert!(report.count() >= 2, "应至少检测到 2 个冲突，实际: {}", report.count());

        let has_direct = report
            .conflicts
            .iter()
            .any(|c| c.kind == ConflictKind::DirectContradict);
        let has_reversal = report
            .conflicts
            .iter()
            .any(|c| c.kind == ConflictKind::StanceReversal);
        assert!(has_direct, "应检测到直接矛盾");
        assert!(has_reversal, "应检测到立场反转");
    }

    #[tokio::test]
    async fn test_detect_clean_update() {
        let detector = HeuristicDetector::new();

        let historical = MemoryUpdate::new().add_fact("用户喜欢咖啡");
        let memory = make_memory_with_updates(vec![historical]);

        // 无冲突的新事实
        let update = MemoryUpdate::new()
            .add_fact("用户住在上海")
            .revise_fact("用户喜欢咖啡和茶");

        let report = detector.detect(&update, &memory).await;
        assert!(report.is_clean(), "无冲突的更新不应产生冲突报告");
    }

    #[tokio::test]
    async fn test_detect_empty_update() {
        let detector = HeuristicDetector::new();
        let memory = make_memory_with_updates(vec![
            MemoryUpdate::new().add_fact("用户喜欢咖啡"),
        ]);

        let update = MemoryUpdate::new();
        let report = detector.detect(&update, &memory).await;
        assert!(report.is_clean());
    }

    #[tokio::test]
    async fn test_has_critical_flag() {
        let detector = HeuristicDetector::new();
        let memory = make_memory_with_updates(vec![]);

        // 自我矛盾 → Critical
        let update = MemoryUpdate::new()
            .add_fact("test")
            .deprecate_fact("test");

        let report = detector.detect(&update, &memory).await;
        assert!(report.has_critical());
    }

    #[tokio::test]
    async fn test_extract_historical_facts() {
        let memory = make_memory_with_updates(vec![
            MemoryUpdate::new().add_fact("事实A").add_fact("事实B"),
            MemoryUpdate::new().add_fact("事实C").revise_fact("修正D"),
        ]);

        let facts = HeuristicDetector::extract_historical_facts(&memory);
        assert_eq!(facts.len(), 3);
        assert!(facts.contains(&"事实A".to_string()));
        assert!(facts.contains(&"事实B".to_string()));
        assert!(facts.contains(&"事实C".to_string()));
        // revised_facts 不应被包含
        assert!(!facts.contains(&"修正D".to_string()));
    }
}
