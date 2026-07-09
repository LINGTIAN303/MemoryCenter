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

/// 反义词对（约 128 对：中文 96 + 英文 32）
///
/// 每对 (pos, neg) 表示 pos 与 neg 语义相反。
/// 检测时若 added_fact 含 pos，existing_fact 含 neg（或反之），则判定为直接矛盾。
/// polarity() 优先匹配更长的 neg（如"不喜欢"优先于"喜欢"），避免子串误判。
const ANTONYM_PAIRS: &[(&str, &str)] = &[
    // ========================================================================
    // 中文反义词（约 96 对）
    // ========================================================================

    // 喜好（7 对）
    ("喜欢", "不喜欢"),
    ("爱", "恨"),
    ("支持", "反对"),
    ("赞同", "反对"),
    ("赞成", "反对"),
    ("偏好", "排斥"),
    ("热爱", "憎恨"),
    // 情感（8 对）
    ("开心", "难过"),
    ("高兴", "悲伤"),
    ("满意", "失望"),
    ("兴奋", "沮丧"),
    ("感动", "冷漠"),
    ("愉快", "痛苦"),
    ("喜欢", "讨厌"),
    ("期待", "绝望"),
    // 评价（8 对）
    ("好", "坏"),
    ("优秀", "糟糕"),
    ("美丽", "丑陋"),
    ("聪明", "愚蠢"),
    ("勤奋", "懒惰"),
    ("勇敢", "懦弱"),
    ("善良", "邪恶"),
    ("诚实", "虚伪"),
    // 是非（6 对）
    ("是", "不是"),
    ("对", "错"),
    ("正确", "错误"),
    ("真", "假"),
    ("有效", "无效"),
    ("合法", "非法"),
    // 状态（11 对）
    ("开", "关"),
    ("启用", "禁用"),
    ("启动", "停止"),
    ("开始", "结束"),
    ("运行", "停止"),
    ("连接", "断开"),
    ("活", "死"),
    ("醒", "睡"),
    ("通", "断"),
    ("满", "空"),
    ("动", "静"),
    // 动作（10 对）
    ("来", "去"),
    ("进", "出"),
    ("上", "下"),
    ("前", "后"),
    ("推", "拉"),
    ("抓", "放"),
    ("拿", "给"),
    ("买", "卖"),
    ("借", "还"),
    ("收", "发"),
    ("升", "降"),
    // 程度（20 对）
    ("增加", "减少"),
    ("上升", "下降"),
    ("变大", "变小"),
    ("增强", "减弱"),
    ("加速", "减速"),
    ("多", "少"),
    ("大", "小"),
    ("高", "低"),
    ("长", "短"),
    ("深", "浅"),
    ("厚", "薄"),
    ("重", "轻"),
    ("快", "慢"),
    ("远", "近"),
    ("宽", "窄"),
    ("强", "弱"),
    ("粗", "细"),
    ("硬", "软"),
    ("松", "紧"),
    ("饱满", "干瘪"),
    // 存在（3 对）
    ("有", "没有"),
    ("存在", "不存在"),
    ("包含", "不包含"),
    // 自然（7 对）
    ("热", "冷"),
    ("暖", "凉"),
    ("亮", "暗"),
    ("明", "暗"),
    ("干", "湿"),
    ("新", "旧"),
    ("胜", "败"),
    // 时间年龄（3 对）
    ("早", "晚"),
    ("老", "少"),
    ("春", "秋"),
    // 态度（5 对）
    ("积极", "消极"),
    ("主动", "被动"),
    ("乐观", "悲观"),
    ("自信", "自卑"),
    ("谦虚", "傲慢"),
    // 逻辑关系（3 对）
    ("同", "异"),
    ("统一", "对立"),
    ("相同", "不同"),
    // 其他（5 对）
    ("成功", "失败"),
    ("允许", "禁止"),
    ("接受", "拒绝"),
    ("同意", "拒绝"),
    ("肯定", "否定"),

    // ========================================================================
    // 英文反义词（约 32 对）
    //
    // 注意：polarity() 用 contains 子串匹配，对英文有已知限制：
    // - 短词可能误匹配（如 "up" 在 "cup" 中）
    // - 带前缀反义词（dis-/un-/in-）会误判，故避免使用
    //   （如 "like/dislike" 不用，改用 "love/hate"）
    // 英文反义词作为补充，主要场景仍为中文。
    // ========================================================================
    ("love", "hate"),
    ("good", "bad"),
    ("big", "small"),
    ("fast", "slow"),
    ("strong", "weak"),
    ("brave", "cowardly"),
    ("smart", "stupid"),
    ("beautiful", "ugly"),
    ("happy", "sad"),
    ("hot", "cold"),
    ("light", "dark"),
    ("hard", "soft"),
    ("loose", "tight"),
    ("old", "new"),
    ("empty", "full"),
    ("busy", "idle"),
    ("dry", "wet"),
    ("long", "short"),
    ("high", "low"),
    ("up", "down"),
    ("left", "right"),
    ("forward", "backward"),
    ("win", "lose"),
    ("pass", "fail"),
    ("alive", "dead"),
    ("awake", "asleep"),
    ("allow", "forbid"),
    ("accept", "reject"),
    ("agree", "refuse"),
    ("yes", "no"),
    ("true", "false"),
    ("success", "failure"),
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
/// use memory_center_core::conflict::ConflictDetector;
/// use memory_center_core::heuristic::HeuristicDetector;
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
// 辅助函数：相似度 + 反义词匹配（v2.7 多语言优化）
// ============================================================================

/// 字符串归一化（去首尾空白 + 转小写）
fn normalize(s: &str) -> String {
    s.trim().to_lowercase()
}

/// 主语言类型（基于 Unicode 范围检测）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Language {
    /// 以 CJK 汉字为主
    Chinese,
    /// 以拉丁字母为主
    Latin,
    /// 混合（无明显主语言）
    Mixed,
}

/// 检测字符串的主语言
///
/// 基于 Unicode 范围统计字符比例：
/// - CJK 汉字（U+4E00~U+9FFF）→ 中文字符
/// - 拉丁字母（A-Z, a-z）→ 英文字符
/// - 其他字符（空格/数字/标点）→ 忽略
///
/// 判定规则：若某语言占比 ≥ 60%，则为主语言；否则为 Mixed
fn detect_language(s: &str) -> Language {
    let mut cjk_count = 0usize;
    let mut latin_count = 0usize;

    for ch in s.chars() {
        let code = ch as u32;
        // CJK 统一汉字范围（常用 + 扩展 A 区起步）
        if (0x4E00..=0x9FFF).contains(&code) || (0x3400..=0x4DBF).contains(&code) {
            cjk_count += 1;
        } else if ch.is_ascii_alphabetic() {
            latin_count += 1;
        }
    }

    let total = cjk_count + latin_count;
    if total == 0 {
        // 无可识别字符，默认按拉丁处理（数字/标点场景）
        return Language::Latin;
    }

    let cjk_ratio = cjk_count as f64 / total as f64;
    let latin_ratio = latin_count as f64 / total as f64;

    if cjk_ratio >= 0.6 {
        Language::Chinese
    } else if latin_ratio >= 0.6 {
        Language::Latin
    } else {
        Language::Mixed
    }
}

/// 计算两个事实的相似度（0.0 ~ 1.0）
///
/// 三级匹配：
/// 1. 精确匹配（归一化后相等）→ 1.0
/// 2. 包含匹配（短串是长串的子串）→ 0.9
/// 3. 语言感知 Jaccard 相似度 → 0.0 ~ 1.0
///    - 中文：字符 bigram Jaccard（向后兼容）
///    - 拉丁：词级 Jaccard + 字符 trigram Jaccard 加权（0.6 * 词级 + 0.4 * 字符级）
///    - 混合：双算法结果加权融合
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

    // 3. 语言感知 Jaccard
    // 两串语言可能不同（如中文事实 vs 英文事实），按"较长串"的语言为主
    let lang = if na.chars().count() >= nb.chars().count() {
        detect_language(&na)
    } else {
        detect_language(&nb)
    };

    match lang {
        Language::Chinese => {
            // 中文：字符 bigram Jaccard（向后兼容）
            let bigrams_a = char_bigrams(&na);
            let bigrams_b = char_bigrams(&nb);
            jaccard(&bigrams_a, &bigrams_b)
        }
        Language::Latin => {
            // 拉丁：词级 Jaccard（主权重 0.6）+ 字符 trigram Jaccard（辅权重 0.4）
            let words_a = word_tokens(&na);
            let words_b = word_tokens(&nb);
            let word_sim = jaccard(&words_a, &words_b);

            let trigrams_a = char_trigrams(&na);
            let trigrams_b = char_trigrams(&nb);
            let char_sim = jaccard(&trigrams_a, &trigrams_b);

            0.6 * word_sim + 0.4 * char_sim
        }
        Language::Mixed => {
            // 混合：中文 bigram + 拉丁词级，合并后 Jaccard
            let bg_a = char_bigrams(&na);
            let bg_b = char_bigrams(&nb);
            let cn_sim = jaccard(&bg_a, &bg_b);

            let words_a = word_tokens(&na);
            let words_b = word_tokens(&nb);
            let word_sim = jaccard(&words_a, &words_b);

            // 中文 bigram 与拉丁词级不可直接合并，取加权平均
            // 权重根据各自语言字符比例动态调整
            let cjk_ratio_a = cjk_ratio(&na);
            let cjk_ratio_b = cjk_ratio(&nb);
            let avg_cjk = (cjk_ratio_a + cjk_ratio_b) / 2.0;

            avg_cjk * cn_sim + (1.0 - avg_cjk) * word_sim
        }
    }
}

/// 计算 CJK 字符在字符串中的占比（0.0 ~ 1.0）
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

/// 生成字符串的字符 bigram 集合（中文场景使用）
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

/// 生成字符串的字符 trigram 集合（拉丁场景使用，捕捉词形变化）
///
/// 例如 "love" → {"lov", "ove"}
fn char_trigrams(s: &str) -> HashSet<String> {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() < 3 {
        let mut set = HashSet::new();
        if !s.is_empty() {
            set.insert(s.to_string());
        }
        return set;
    }
    (0..chars.len() - 2)
        .map(|i| format!("{}{}{}", chars[i], chars[i + 1], chars[i + 2]))
        .collect()
}

/// 拉丁字母词级分词（按空格 + 标点切分，转小写）
///
/// 例如 "I love coffee" → {"i", "love", "coffee"}
fn word_tokens(s: &str) -> HashSet<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
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
                file_changes: Vec::new(),
            },
            llm_message: MessageContent {
                text: Some("助手回复".to_string()),
                attachments: vec![],
                tool_calls: vec![],
                thinking: None,
                file_changes: Vec::new(),
            },
            tags: vec![],
            timestamp: Utc::now(),
            token_count: 100,
            stop_reason: None,
            cost: None,
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
    // v2.7 多语言相似度测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_detect_language_chinese() {
        assert_eq!(detect_language("用户喜欢咖啡"), Language::Chinese);
        assert_eq!(detect_language("今天天气不错"), Language::Chinese);
    }

    #[test]
    fn test_detect_language_latin() {
        assert_eq!(detect_language("I love coffee"), Language::Latin);
        assert_eq!(detect_language("Hello World"), Language::Latin);
    }

    #[test]
    fn test_detect_language_mixed() {
        // 中英接近均衡，无明显主语言（cjk=3, latin=4, cjk_ratio=0.43 < 0.6, latin_ratio=0.57 < 0.6）
        assert_eq!(detect_language("ab 咖啡 cd 茶"), Language::Mixed);
    }

    #[test]
    fn test_detect_language_empty_and_numbers() {
        // 无可识别字符 → 默认 Latin
        assert_eq!(detect_language("123 456"), Language::Latin);
        assert_eq!(detect_language(""), Language::Latin);
    }

    #[test]
    fn test_word_tokens_basic() {
        let tokens = word_tokens("I love coffee");
        assert!(tokens.contains("i"));
        assert!(tokens.contains("love"));
        assert!(tokens.contains("coffee"));
        assert_eq!(tokens.len(), 3);
    }

    #[test]
    fn test_word_tokens_with_punctuation() {
        let tokens = word_tokens("Hello, world! How are you?");
        assert!(tokens.contains("hello"));
        assert!(tokens.contains("world"));
        assert!(tokens.contains("how"));
        assert_eq!(tokens.len(), 5);
    }

    #[test]
    fn test_char_trigrams_basic() {
        let tg = char_trigrams("love");
        assert!(tg.contains("lov"));
        assert!(tg.contains("ove"));
        assert_eq!(tg.len(), 2);
    }

    #[test]
    fn test_char_trigrams_short() {
        // 少于 3 字符 → 整串作为单个 gram
        let tg = char_trigrams("ab");
        assert_eq!(tg.len(), 1);
        assert!(tg.contains("ab"));
    }

    #[test]
    fn test_cjk_ratio_pure_chinese() {
        assert!((cjk_ratio("用户喜欢咖啡") - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_cjk_ratio_pure_english() {
        assert!((cjk_ratio("I love coffee") - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_cjk_ratio_mixed() {
        let r = cjk_ratio("love 咖啡");
        // 6 非空字符中 2 个 CJK → 0.333
        assert!((r - 0.333).abs() < 0.01, "cjk_ratio 应为 0.333，实际: {}", r);
    }

    // ------------------------------------------------------------------------
    // 多语言相似度集成测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_similarity_english_word_level() {
        // 英文：词级匹配应优于字符 bigram
        // "love coffee" vs "like coffee"：词级 Jaccard = 1/3 ≈ 0.33
        // 旧版字符 bigram 仅 0.2 左右，新版应更高
        let sim = similarity("love coffee", "like coffee");
        assert!(sim > 0.2, "英文词级相似度应 > 0.2，实际: {}", sim);
    }

    #[test]
    fn test_similarity_english_exact_words() {
        // 完全相同的英文词 → 高相似度
        let sim = similarity("user loves coffee", "user loves coffee");
        assert_eq!(sim, 1.0);
    }

    #[test]
    fn test_similarity_english_shared_words() {
        // 共享 2/3 词 → 词级 Jaccard = 0.67，加权后应 > 0.4
        let sim = similarity("user loves coffee", "user hates coffee");
        assert!(sim > 0.4, "共享词相似度应 > 0.4，实际: {}", sim);
    }

    #[test]
    fn test_similarity_english_no_overlap() {
        // 完全不相关 → 接近 0
        let sim = similarity("hello world", "goodbye universe");
        assert!(sim < 0.2, "不相关英文相似度应 < 0.2，实际: {}", sim);
    }

    #[test]
    fn test_similarity_mixed_cn_en() {
        // 混合场景：中文为主 → 走中文 bigram 路径
        let sim = similarity("用户 love coffee", "用户 hate coffee");
        // "用户" bigram 匹配 + 英文词不匹配 → 中等相似度
        assert!(sim > 0.0 && sim < 0.9, "混合相似度应在 0~0.9，实际: {}", sim);
    }

    #[test]
    fn test_similarity_chinese_backward_compat() {
        // 中文场景行为应与旧版一致（字符 bigram Jaccard）
        let sim = similarity("用户喜欢喝咖啡", "用户喜欢喝茶");
        // 旧版 Jaccard 约 0.6（5 个 bigram 中 3 个相同）
        assert!(sim > 0.4 && sim < 0.9, "中文相似度应保持 0.4~0.9，实际: {}", sim);
    }

    #[test]
    fn test_similarity_case_insensitive_english() {
        // 大小写归一化
        assert_eq!(similarity("Hello World", "hello world"), 1.0);
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

    // ------------------------------------------------------------------------
    // v2.11: 扩展反义词词典测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_antonym_chinese_emotion() {
        // 情感类
        assert!(check_antonym("用户开心", "用户难过").is_some());
        assert!(check_antonym("感到满意", "感到失望").is_some());
        assert!(check_antonym("很兴奋", "很沮丧").is_some());
        assert!(check_antonym("喜欢这个", "讨厌这个").is_some());
    }

    #[test]
    fn test_antonym_chinese_evaluation() {
        // 评价类
        assert!(check_antonym("质量好", "质量坏").is_some());
        assert!(check_antonym("表现优秀", "表现糟糕").is_some());
        assert!(check_antonym("很聪明", "很愚蠢").is_some());
        assert!(check_antonym("非常勇敢", "非常懦弱").is_some());
    }

    #[test]
    fn test_antonym_chinese_action() {
        // 动作类
        assert!(check_antonym("向前进", "向后退").is_some());
        assert!(check_antonym("向上走", "向下走").is_some());
        assert!(check_antonym("推门", "拉门").is_some());
        assert!(check_antonym("买入", "卖出").is_some());
    }

    #[test]
    fn test_antonym_chinese_degree() {
        // 程度类
        assert!(check_antonym("数量多", "数量少").is_some());
        assert!(check_antonym("速度快", "速度慢").is_some());
        assert!(check_antonym("距离远", "距离近").is_some());
        assert!(check_antonym("信号强", "信号弱").is_some());
    }

    #[test]
    fn test_antonym_chinese_nature() {
        // 自然类
        assert!(check_antonym("天气热", "天气冷").is_some());
        assert!(check_antonym("房间亮", "房间暗").is_some());
        assert!(check_antonym("衣服干", "衣服湿").is_some());
    }

    #[test]
    fn test_antonym_chinese_attitude() {
        // 态度类
        assert!(check_antonym("态度积极", "态度消极").is_some());
        assert!(check_antonym("很主动", "很被动").is_some());
        assert!(check_antonym("性格乐观", "性格悲观").is_some());
    }

    #[test]
    fn test_antonym_english_basic() {
        // 英文反义词
        assert!(check_antonym("I love coffee", "I hate coffee").is_some());
        assert!(check_antonym("good quality", "bad quality").is_some());
        assert!(check_antonym("big size", "small size").is_some());
        assert!(check_antonym("fast speed", "slow speed").is_some());
    }

    #[test]
    fn test_antonym_english_extended() {
        // 更多英文反义词
        assert!(check_antonym("strong signal", "weak signal").is_some());
        assert!(check_antonym("happy mood", "sad mood").is_some());
        assert!(check_antonym("hot weather", "cold weather").is_some());
        assert!(check_antonym("hard material", "soft material").is_some());
        assert!(check_antonym("win the game", "lose the game").is_some());
        assert!(check_antonym("accept offer", "reject offer").is_some());
        assert!(check_antonym("yes answer", "no answer").is_some());
        assert!(check_antonym("true statement", "false statement").is_some());
    }

    #[test]
    fn test_antonym_no_false_positive_extended() {
        // 确保新增反义词不引入误判
        assert!(check_antonym("用户喜欢咖啡", "用户喜欢茶").is_none());
        assert!(check_antonym("今天很热", "今天很暖").is_none(), "热和暖不是反义词");
        assert!(check_antonym("质量很好", "质量很好").is_none(), "相同极性不应判定为对立");
        assert!(check_antonym("I love coffee", "I love tea").is_none());
        assert!(check_antonym("big and tall", "big and short").is_none(), "tall 和 short 不同反义词对，不应对 big 判定对立");
    }

    #[tokio::test]
    async fn test_detect_direct_contradiction_extended() {
        // v2.11: 测试新增反义词的端到端检测
        let detector = HeuristicDetector::new();
        let historical = MemoryUpdate::new().add_fact("用户性格乐观");
        let memory = make_memory_with_updates(vec![historical]);

        let update = MemoryUpdate::new().add_fact("用户性格悲观");
        let report = detector.detect(&update, &memory).await;
        assert!(report.has_critical(), "乐观 vs 悲悲应检测到直接矛盾");
        assert_eq!(report.conflicts[0].kind, ConflictKind::DirectContradict);
    }

    #[tokio::test]
    async fn test_detect_direct_contradiction_english() {
        // v2.11: 英文反义词端到端检测
        let detector = HeuristicDetector::new();
        let historical = MemoryUpdate::new().add_fact("User loves coffee");
        let memory = make_memory_with_updates(vec![historical]);

        let update = MemoryUpdate::new().add_fact("User hates coffee");
        let report = detector.detect(&update, &memory).await;
        assert!(report.has_critical(), "love vs hate 应检测到直接矛盾");
    }
}
