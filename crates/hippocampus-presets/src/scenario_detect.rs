//! # 场景识别模块（v2.33）
//!
//! 从对话内容自动推断 Scenario（Coding/Writing/Research 等 7 类），
//! 解决 Trae/Cursor 等 Agent 里写非 coding 任务时 5 维配置错配的痛点。
//!
//! ## 架构
//!
//! ```text
//! KeywordScenarioDetector  ──┐   HttpScenarioDetector ──┐
//!  (纯算法，零依赖)          │   (LLM 推断)            │
//!  7 场景 × ~15 关键词        │   复用 LlmDetectorConfig │
//!  返回 (Scenario, f32)       │                         │
//! └─────┬───────────────────┘ └────────┬────────────────┘
//!       │                                │
//!       └────────────┬───────────────────┘
//!                    ▼
//!      HybridScenarioDetector
//!       (串联关键词 + LLM 兜底)
//!       置信度 < 0.6 时调 LLM
//!                    │
//!                    ▼ return DetectionResult
//!      resolve_effective_scenario (编排函数)
//!       1. 用户显式 > 2. session_meta > 3. 识别 > 4. Agent 默认
//! ```
//!
//! ## 设计要点
//!
//! - **首次识别**：仅在首次 archive 时调用，后续读取 session_meta 跳过
//! - **失败降级**：识别失败永不阻塞 archive，降级到 Agent 默认场景
//! - **跨进程持久**：识别结果写入 `sessions/{sid}/meta.json`

use hippocampus_core::model::MessageTurn;
use hippocampus_scenarios::Scenario;
use std::sync::Arc;

// ============================================================================
// 关键词字典
// ============================================================================

/// 关键词字典：7 场景 × ~15 关键词
///
/// 子串匹配（大小写不敏感），统计每个场景命中数。
fn keyword_dict() -> Vec<(Scenario, Vec<&'static str>)> {
    vec![
        (Scenario::Coding, vec![
            "fn ", "class ", "def ", "function", "bug", "compile", "commit", "refactor",
            "api", "函数", "编译", "重构", "报错", "调试", "架构",
        ]),
        (Scenario::Writing, vec![
            "文章", "论点", "论据", "素材", "风格", "段落", "开头", "结尾", "修辞",
            "article", "essay", "draft", "outline", "narrative", "tone",
        ]),
        (Scenario::Research, vec![
            "假设", "方法", "数据", "结论", "引用", "文献", "实验", "样本", "论文",
            "hypothesis", "methodology", "conclusion", "citation", "abstract",
        ]),
        (Scenario::Daily, vec![
            "今天", "昨天", "吃饭", "天气", "心情", "朋友", "周末", "电影", "购物",
            "约会", "family", "dinner", "weather", "mood", "weekend",
        ]),
        (Scenario::Finance, vec![
            "交易", "金额", "收益", "风险", "投资", "股票", "基金", "利率", "止损",
            "portfolio", "stock", "bond", "dividend", "volatility", "hedge",
        ]),
        (Scenario::Design, vec![
            "设计", "原型", "用户", "界面", "交互", "迭代", "视觉", "反馈",
            "mockup", "wireframe", "ui", "ux", "persona", "iteration",
        ]),
        (Scenario::OfficeWork, vec![
            "会议", "待办", "文档", "决议", "项目", "截止", "参会", "纪要",
            "meeting", "todo", "memo", "deadline", "agenda", "minutes",
        ]),
    ]
}

// ============================================================================
// DetectionResult
// ============================================================================

/// 识别结果
#[derive(Debug, Clone)]
pub struct DetectionResult {
    /// 识别的场景（None 表示识别失败，调用方应降级）
    pub scenario: Option<Scenario>,
    /// 置信度 0.0-1.0（关键词规则按 top/(top+second) 计算，LLM 默认 0.8）
    pub confidence: f32,
    /// 识别方法："keyword" / "llm" / "failed"
    pub method: &'static str,
}

impl DetectionResult {
    /// 识别失败
    pub fn failed() -> Self {
        Self {
            scenario: None,
            confidence: 0.0,
            method: "failed",
        }
    }

    /// 是否识别失败
    pub fn is_failed(&self) -> bool {
        self.scenario.is_none()
    }
}

// ============================================================================
// KeywordScenarioDetector
// ============================================================================

/// 关键词规则场景识别器（纯算法，零依赖）
///
/// 子串匹配（大小写不敏感）+ 置信度计算：
/// - `confidence = top / (top + second)`
/// - `>= 0.6` 算高置信，直接采用
/// - `< 0.6` 触发 LLM 兜底
/// - 全部零命中 → 返回 None
pub struct KeywordScenarioDetector {
    // 预留：后续 HybridScenarioDetector 会注入 LLM detector
    #[allow(dead_code)]
    placeholder: Arc<()>,
}

impl KeywordScenarioDetector {
    pub fn new() -> Self {
        Self {
            placeholder: Arc::new(()),
        }
    }

    /// 从对话轮次提取文本（拼接 user_message.text + llm_message.text）
    fn extract_text(turns: &[MessageTurn]) -> String {
        let mut text = String::new();
        for turn in turns {
            if let Some(t) = &turn.user_message.text {
                text.push_str(t);
                text.push(' ');
            }
            if let Some(t) = &turn.llm_message.text {
                text.push_str(t);
                text.push(' ');
            }
        }
        text.to_lowercase()
    }

    /// 关键词匹配，返回 (场景, 命中数) 列表，按命中数降序
    fn count_hits(text: &str) -> Vec<(Scenario, usize)> {
        let dict = keyword_dict();
        let mut hits: Vec<(Scenario, usize)> = dict
            .into_iter()
            .map(|(scenario, keywords)| {
                let count = keywords
                    .iter()
                    .filter(|kw| text.contains(*kw))
                    .count();
                (scenario, count)
            })
            .filter(|(_, c)| *c > 0)
            .collect();
        hits.sort_by(|a, b| b.1.cmp(&a.1));
        hits
    }

    /// 识别场景
    ///
    /// 返回 `Some((Scenario, confidence))` 或 `None`（零命中）。
    pub fn detect(&self, turns: &[MessageTurn]) -> Option<(Scenario, f32)> {
        let text = Self::extract_text(turns);
        let hits = Self::count_hits(&text);

        if hits.is_empty() {
            return None;
        }

        let top = hits[0].clone();
        let top_count = top.1;

        let confidence = if hits.len() >= 2 {
            let second_count = hits[1].1;
            top_count as f32 / (top_count + second_count) as f32
        } else {
            // 只有一个场景命中 → 高置信
            1.0
        };

        Some((top.0, confidence))
    }
}

impl Default for KeywordScenarioDetector {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use hippocampus_core::model::{MessageContent, MessageTurn};
    use chrono::Utc;
    use uuid::Uuid;

    fn make_turn(user: &str, llm: &str) -> MessageTurn {
        MessageTurn {
            id: Uuid::new_v4(),
            user_message: MessageContent {
                text: Some(user.to_string()),
                attachments: vec![],
                tool_calls: vec![],
                thinking: None,
            },
            llm_message: MessageContent {
                text: Some(llm.to_string()),
                attachments: vec![],
                tool_calls: vec![],
                thinking: None,
            },
            tags: vec![],
            timestamp: Utc::now(),
            token_count: 100,
        }
    }

    // ========================================================================
    // KeywordScenarioDetector 测试
    // ========================================================================

    #[test]
    fn test_keyword_detect_coding() {
        let turns = vec![
            make_turn("帮我写一个 Rust 函数", "好的，fn 主体如下..."),
            make_turn("这里报错了", "调试一下，可能是架构问题。"),
        ];
        let detector = KeywordScenarioDetector::new();
        let result = detector.detect(&turns);
        assert!(result.is_some(), "应识别到 Coding");
        let (scenario, conf) = result.unwrap();
        assert_eq!(scenario, Scenario::Coding);
        assert!(conf >= 0.6, "Coding 单场景命中应高置信: {}", conf);
    }

    #[test]
    fn test_keyword_detect_writing() {
        let turns = vec![
            make_turn("帮我写一篇文章", "好的，先列大纲。论点是什么？"),
            make_turn("论据需要充实", "段落开头可以用修辞。"),
        ];
        let detector = KeywordScenarioDetector::new();
        let (scenario, _) = detector.detect(&turns).unwrap();
        assert_eq!(scenario, Scenario::Writing);
    }

    #[test]
    fn test_keyword_detect_research() {
        let turns = vec![
            make_turn("假设是什么", "根据数据，假设是..."),
            make_turn("引用哪篇文献", "结论在论文的 abstract。"),
        ];
        let detector = KeywordScenarioDetector::new();
        let (scenario, _) = detector.detect(&turns).unwrap();
        assert_eq!(scenario, Scenario::Research);
    }

    #[test]
    fn test_keyword_detect_daily() {
        let turns = vec![
            make_turn("今天天气怎么样", "周末适合看电影。"),
            make_turn("和朋友吃饭", "好的，心情不错。"),
        ];
        let detector = KeywordScenarioDetector::new();
        let (scenario, _) = detector.detect(&turns).unwrap();
        assert_eq!(scenario, Scenario::Daily);
    }

    #[test]
    fn test_keyword_detect_finance() {
        let turns = vec![
            make_turn("这只股票怎么样", "投资有风险，建议止损。"),
            make_turn("基金收益如何", "portfolio 配置需要分散。"),
        ];
        let detector = KeywordScenarioDetector::new();
        let (scenario, _) = detector.detect(&turns).unwrap();
        assert_eq!(scenario, Scenario::Finance);
    }

    #[test]
    fn test_keyword_detect_design() {
        let turns = vec![
            make_turn("UI 设计问题", "wireframe 先画一下"),
            make_turn("用户体验如何", "persona 和交互迭代。"),
        ];
        let detector = KeywordScenarioDetector::new();
        let (scenario, _) = detector.detect(&turns).unwrap();
        assert_eq!(scenario, Scenario::Design);
    }

    #[test]
    fn test_keyword_detect_officework() {
        let turns = vec![
            make_turn("帮我写会议纪要", "agenda 如下，参会人..."),
            make_turn("项目截止日期", "deadline 是下周，待办事项..."),
        ];
        let detector = KeywordScenarioDetector::new();
        let (scenario, _) = detector.detect(&turns).unwrap();
        assert_eq!(scenario, Scenario::OfficeWork);
    }

    #[test]
    fn test_keyword_detect_empty_turns_returns_none() {
        let detector = KeywordScenarioDetector::new();
        assert!(detector.detect(&[]).is_none());
    }

    #[test]
    fn test_keyword_detect_zero_hits_returns_none() {
        let turns = vec![
            make_turn("啊啊啊", "嗯嗯嗯"),
            make_turn("哦哦哦", "呃呃呃"),
        ];
        let detector = KeywordScenarioDetector::new();
        assert!(detector.detect(&turns).is_none(), "零命中应返回 None");
    }

    #[test]
    fn test_keyword_detect_single_scenario_high_confidence() {
        // 只命中一个场景 → confidence = 1.0
        let turns = vec![make_turn("fn compile refactor", "好的")];
        let detector = KeywordScenarioDetector::new();
        let (scenario, conf) = detector.detect(&turns).unwrap();
        assert_eq!(scenario, Scenario::Coding);
        assert_eq!(conf, 1.0);
    }

    #[test]
    fn test_keyword_detect_mixed_scenarios_lower_confidence() {
        // 同时命中 Coding 和 Writing → confidence < 1.0
        let turns = vec![make_turn(
            "fn function 文章 论点 段落 调试",
            "compile 重构"
        )];
        let detector = KeywordScenarioDetector::new();
        let (scenario, conf) = detector.detect(&turns).unwrap();
        // 哪个场景命中数多就选哪个
        let _ = (scenario, conf);
        // 关键是置信度 < 1.0（说明触发了 LLM 兜底条件）
        assert!(conf < 1.0, "混合场景置信度应 < 1.0: {}", conf);
    }
}
