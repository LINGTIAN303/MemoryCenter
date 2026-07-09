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

use crate::builder::{scenario_from_str, scenario_to_str};
use memory_center_agents::AgentFamily;
use memory_center_core::model::MessageTurn;
use memory_center_core::storage::{SessionMeta, Storage};
use memory_center_llm::LlmDetectorConfig;
use memory_center_scenarios::Scenario;
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
pub struct KeywordScenarioDetector;

impl KeywordScenarioDetector {
    pub fn new() -> Self {
        Self
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
// HttpScenarioDetector
// ============================================================================

/// HTTP LLM 场景识别器
///
/// 复用 `LlmDetectorConfig`（同 `MEMORY_CENTER_DETECTOR_*` 环境变量前缀），
/// 调用 OpenAI 兼容 API 推断对话场景。
///
/// ## Prompt 策略
///
/// 要求 LLM 严格返回 JSON：`{"scenario": "coding", "reason": "..."}`
///
/// ## 降级策略
///
/// - 未配置 API URL（config.api_url 为空）：返回 None
/// - 网络错误 / 超时 / API 错误：返回 None
/// - JSON 解析失败：返回 None
/// - 场景标签不在 7 个内置场景中：视为 `Custom(s)`
pub struct HttpScenarioDetector {
    config: LlmDetectorConfig,
    client: reqwest::Client,
}

impl HttpScenarioDetector {
    /// 创建新的 LLM 场景识别器
    pub fn new(config: LlmDetectorConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { config, client }
    }

    /// 从对话轮次提取文本（前 N 轮，默认 10 轮）
    fn build_conversation_summary(turns: &[MessageTurn], max_turns: usize) -> String {
        let take = turns.len().min(max_turns);
        let mut summary = String::new();
        for (i, turn) in turns.iter().take(take).enumerate() {
            if let Some(t) = &turn.user_message.text {
                summary.push_str(&format!("轮次 {} 用户: {}\n", i + 1, truncate(t, 200)));
            }
            if let Some(t) = &turn.llm_message.text {
                summary.push_str(&format!("轮次 {} 助手: {}\n", i + 1, truncate(t, 200)));
            }
        }
        summary
    }

    /// 构造 LLM prompt
    fn build_prompt(conversation_summary: &str) -> String {
        format!(
            r#"你是一个场景识别器。请分析以下对话内容，判断属于哪个场景。

## 可选场景标签

- coding: 编码场景（编程/调试/架构设计/code review）
- writing: 写作场景（文章/文档/创意写作）
- research: 科研场景（论文/实验/数据分析）
- daily: 日常场景（闲聊/咨询/生活）
- finance: 金融场景（交易/投资/风险分析）
- design: 设计场景（UI/UX/视觉/产品设计）
- officework: 工作场景（会议/文档/项目协作）

## 对话摘要（前 10 轮）

{conversation_summary}

## 输出要求

请只返回 JSON，不要包含任何解释或 markdown 标记。格式如下：

{{"scenario": "coding", "reason": "对话涉及 Rust 代码实现"}}

若无法判断，返回：{{"scenario": "daily", "reason": "无明显场景特征"}}"#,
            conversation_summary = conversation_summary
        )
    }

    /// 解析 LLM 返回的 JSON，提取场景标签
    fn parse_scenario(raw: &str) -> Option<Scenario> {
        // 尝试直接解析
        let value: serde_json::Value = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(_) => {
                // 尝试从 markdown 代码块中提取
                let trimmed = Self::extract_json_from_markdown(raw);
                match serde_json::from_str(&trimmed) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(error = %e, raw = %raw, "LLM 场景识别响应 JSON 解析失败");
                        return None;
                    }
                }
            }
        };

        let scenario_str = value
            .get("scenario")
            .and_then(|s| s.as_str())
            .unwrap_or("");

        if scenario_str.is_empty() {
            tracing::warn!(raw = %raw, "LLM 响应缺少 scenario 字段");
            return None;
        }

        Some(scenario_from_str(scenario_str))
    }

    /// 从 markdown 代码块中提取 JSON
    fn extract_json_from_markdown(raw: &str) -> String {
        let trimmed = raw.trim();
        if let Some(start) = trimmed.find("```") {
            let after = &trimmed[start + 3..];
            let after = after.strip_prefix("json").unwrap_or(after);
            if let Some(end) = after.find("```") {
                return after[..end].trim().to_string();
            }
        }
        trimmed.to_string()
    }

    /// 识别场景
    ///
    /// 返回 `Some(Scenario)` 或 `None`（失败时调用方应降级到 Agent 默认场景）。
    pub async fn detect(&self, turns: &[MessageTurn]) -> Option<Scenario> {
        if self.config.api_url.is_empty() {
            tracing::debug!("HttpScenarioDetector 未配置 api_url，跳过");
            return None;
        }

        let summary = Self::build_conversation_summary(turns, 10);
        let prompt = Self::build_prompt(&summary);

        let request_body = serde_json::json!({
            "model": self.config.model,
            "messages": [
                {"role": "user", "content": prompt}
            ],
            "max_tokens": self.config.max_tokens,
            "temperature": 0.0,
            "thinking": {"type": "disabled"},
        });

        let resp = match self
            .client
            .post(&self.config.api_url)
            .bearer_auth(&self.config.api_key)
            .json(&request_body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "LLM 场景识别 API 请求失败");
                return None;
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(status = %status, body = %body, "LLM 场景识别 API 返回错误状态");
            return None;
        }

        let resp_json: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "LLM 场景识别响应解析失败");
                return None;
            }
        };

        let content = resp_json
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("");

        Self::parse_scenario(content)
    }
}

/// 截断文本到指定字符数（避免 prompt 过长）
fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        s.chars().take(max_chars).collect::<String>() + "..."
    }
}

// ============================================================================
// HybridScenarioDetector
// ============================================================================

/// 混合场景识别器（关键词 + LLM 兜底）
///
/// ## 串联策略
///
/// 1. 关键词规则优先
/// 2. 关键词置信度 `>= 0.6` → 直接采用，跳过 LLM
/// 3. 关键词置信度 `< 0.6` 或零命中 → 调 LLM
/// 4. LLM 失败 → 返回 `DetectionResult::failed()`
pub struct HybridScenarioDetector {
    keyword: KeywordScenarioDetector,
    llm: Option<Arc<HttpScenarioDetector>>,
}

impl HybridScenarioDetector {
    /// 创建混合识别器
    ///
    /// - `llm = None`：仅关键词模式（未配置 LLM API）
    /// - `llm = Some`：关键词 + LLM 兜底
    pub fn new(llm: Option<Arc<HttpScenarioDetector>>) -> Self {
        Self {
            keyword: KeywordScenarioDetector::new(),
            llm,
        }
    }

    /// 识别场景
    pub async fn detect(&self, turns: &[MessageTurn]) -> DetectionResult {
        // 1. 关键词规则优先
        if let Some((scenario, conf)) = self.keyword.detect(turns) {
            if conf >= 0.6 {
                tracing::debug!(
                    ?scenario,
                    confidence = conf,
                    "关键词高置信，跳过 LLM"
                );
                return DetectionResult {
                    scenario: Some(scenario),
                    confidence: conf,
                    method: "keyword",
                };
            }
            tracing::debug!(
                ?scenario,
                confidence = conf,
                "关键词低置信，触发 LLM 兜底"
            );
        } else {
            tracing::debug!("关键词零命中，触发 LLM 兜底");
        }

        // 2. LLM 兜底
        if let Some(llm) = &self.llm {
            if let Some(scenario) = llm.detect(turns).await {
                return DetectionResult {
                    scenario: Some(scenario),
                    confidence: 0.8,
                    method: "llm",
                };
            }
        }

        // 3. 全部失败
        DetectionResult::failed()
    }
}

// ============================================================================
// resolve_effective_scenario 编排函数
// ============================================================================

/// 解析生效的场景（v2.33 核心 API）
///
/// 4 级优先级链：
///
/// 1. **用户显式**（`user_explicit` 参数）最高
/// 2. **session 元数据**（已识别则跳过识别）
/// 3. **首次 archive**：调 `detector.detect(turns)` 识别 + 写入元数据
/// 4. **降级**：Agent 默认场景（[`crate::resolve_scenario_name`]）
///
/// ## 参数
///
/// - `storage`：存储 trait（读写 session_meta）
/// - `session_id`：会话 ID
/// - `user_explicit`：用户显式指定的场景（来自 preset.scenario）
/// - `agent_family`：Agent family（用于降级时推导默认场景）
/// - `detector`：场景识别器
/// - `turns`：对话内容（首次识别时用）
///
/// ## 失败容忍
///
/// - `read_session_meta` 失败：当作 None，触发重新识别
/// - `write_session_meta` 失败：日志 warn，不阻塞返回
/// - detector 识别失败：降级到 Agent 默认场景
pub async fn resolve_effective_scenario(
    storage: &dyn Storage,
    session_id: &str,
    user_explicit: Option<&str>,
    agent_family: &AgentFamily,
    detector: &HybridScenarioDetector,
    turns: &[MessageTurn],
) -> Scenario {
    // 1. 用户显式最高
    if let Some(s) = user_explicit {
        tracing::debug!(scenario = %s, "场景识别：用户显式指定");
        return scenario_from_str(s);
    }

    // 2. session 元数据（已识别）
    match storage.read_session_meta(session_id).await {
        Ok(Some(meta)) => {
            tracing::debug!(
                scenario = %meta.scenario,
                confidence = meta.confidence,
                method = %meta.method,
                "场景识别：命中 session 元数据"
            );
            return scenario_from_str(&meta.scenario);
        }
        Ok(None) => { /* 首次识别，继续 */ }
        Err(e) => {
            tracing::warn!(error = %e, "读取 session_meta 失败，触发重新识别");
        }
    }

    // 3. 首次识别
    let result = detector.detect(turns).await;
    if let Some(scenario) = result.scenario {
        // v2.40：写入 agent_family + hook_mode（由 HookModeResolver 解析）
        let hook_mode = memory_center_agents::HookModeResolver::resolve(agent_family)
            .as_str()
            .to_string();
        let meta = SessionMeta {
            scenario: scenario_to_str(&scenario),
            confidence: result.confidence,
            method: result.method.to_string(),
            detected_at: chrono::Utc::now(),
            agent_family: agent_family.display_name().to_string(),
            hook_mode,
        };
        // 写入元数据（失败不阻塞）
        if let Err(e) = storage.write_session_meta(session_id, &meta).await {
            tracing::warn!(error = %e, "写入 session_meta 失败（不阻塞 archive）");
        }
        tracing::info!(
            ?scenario,
            confidence = result.confidence,
            method = %result.method,
            "场景识别完成"
        );
        return scenario;
    }

    // 4. 降级：Agent 默认场景
    let default_str = crate::resolve_scenario_name(agent_family);
    let default = scenario_from_str(&default_str);
    tracing::info!(
        default = ?default,
        "场景识别失败，降级到 Agent 默认场景"
    );
    default
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use memory_center_core::model::{MessageContent, MessageTurn};
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
                file_changes: Vec::new(),
            },
            llm_message: MessageContent {
                text: Some(llm.to_string()),
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

    // ========================================================================
    // HttpScenarioDetector 测试（不含真实网络调用，仅测 prompt 构造 + 解析）
    // ========================================================================

    #[test]
    fn test_http_build_prompt_contains_scenario_labels() {
        let summary = "轮次 1 用户: 写一个 Rust 函数\n轮次 1 助手: 好的\n";
        let prompt = HttpScenarioDetector::build_prompt(summary);
        assert!(prompt.contains("coding"));
        assert!(prompt.contains("writing"));
        assert!(prompt.contains("research"));
        assert!(prompt.contains("daily"));
        assert!(prompt.contains("finance"));
        assert!(prompt.contains("design"));
        assert!(prompt.contains("officework"));
        assert!(prompt.contains("轮次 1 用户: 写一个 Rust 函数"));
    }

    #[test]
    fn test_http_parse_scenario_valid_json() {
        let raw = r#"{"scenario": "coding", "reason": "Rust 代码"}"#;
        let scenario = HttpScenarioDetector::parse_scenario(raw);
        assert_eq!(scenario, Some(Scenario::Coding));
    }

    #[test]
    fn test_http_parse_scenario_markdown_wrapped() {
        let raw = "```json\n{\"scenario\": \"writing\", \"reason\": \"文章\"}\n```";
        let scenario = HttpScenarioDetector::parse_scenario(raw);
        assert_eq!(scenario, Some(Scenario::Writing));
    }

    #[test]
    fn test_http_parse_scenario_unknown_label_falls_back_to_custom() {
        let raw = r#"{"scenario": "medical", "reason": "医学对话"}"#;
        let scenario = HttpScenarioDetector::parse_scenario(raw);
        assert_eq!(scenario, Some(Scenario::Custom("medical".to_string())));
    }

    #[test]
    fn test_http_parse_scenario_missing_scenario_field() {
        let raw = r#"{"reason": "无 scenario 字段"}"#;
        let scenario = HttpScenarioDetector::parse_scenario(raw);
        assert_eq!(scenario, None);
    }

    #[test]
    fn test_http_parse_scenario_invalid_json() {
        let raw = "这不是 JSON";
        let scenario = HttpScenarioDetector::parse_scenario(raw);
        assert_eq!(scenario, None);
    }

    #[test]
    fn test_http_build_conversation_summary_truncates_long_text() {
        let long_text = "a".repeat(500);
        let turns = vec![make_turn(&long_text, &long_text)];
        let summary = HttpScenarioDetector::build_conversation_summary(&turns, 10);
        // 每段截断到 200 字符 + "..."
        assert!(summary.contains("..."));
        // 总长度应远小于原始 1000 字符
        assert!(summary.chars().count() < 600);
    }

    #[tokio::test]
    async fn test_http_detect_without_api_url_returns_none() {
        let config = LlmDetectorConfig::default(); // api_url 为空
        let detector = HttpScenarioDetector::new(config);
        let turns = vec![make_turn("test", "test")];
        assert_eq!(detector.detect(&turns).await, None);
    }

    #[test]
    fn test_http_extract_json_from_markdown_plain() {
        let raw = r#"{"scenario": "coding"}"#;
        let extracted = HttpScenarioDetector::extract_json_from_markdown(raw);
        assert_eq!(extracted, r#"{"scenario": "coding"}"#);
    }

    #[test]
    fn test_http_extract_json_from_markdown_block() {
        let raw = "```json\n{\"scenario\": \"coding\"}\n```";
        let extracted = HttpScenarioDetector::extract_json_from_markdown(raw);
        assert_eq!(extracted, r#"{"scenario": "coding"}"#);
    }

    // ========================================================================
    // HybridScenarioDetector 测试
    // ========================================================================

    #[tokio::test]
    async fn test_hybrid_keyword_high_confidence_skips_llm() {
        // 关键词命中明显，置信度 >= 0.6，应跳过 LLM
        let turns = vec![
            make_turn("fn compile refactor 调试", "好的，重构架构"),
        ];
        let detector = HybridScenarioDetector::new(None); // 无 LLM
        let result = detector.detect(&turns).await;
        assert!(result.scenario.is_some());
        assert_eq!(result.scenario.unwrap(), Scenario::Coding);
        assert!(result.confidence >= 0.6);
        assert_eq!(result.method, "keyword");
    }

    #[tokio::test]
    async fn test_hybrid_keyword_zero_hits_without_llm_returns_failed() {
        // 零命中 + 无 LLM → failed
        let turns = vec![make_turn("啊啊啊", "嗯嗯嗯")];
        let detector = HybridScenarioDetector::new(None);
        let result = detector.detect(&turns).await;
        assert!(result.is_failed());
    }

    #[tokio::test]
    async fn test_hybrid_keyword_zero_hits_with_llm_unconfigured_returns_failed() {
        // 零命中 + LLM 未配置 api_url → LLM 返回 None → failed
        let turns = vec![make_turn("啊啊啊", "嗯嗯嗯")];
        let config = LlmDetectorConfig::default(); // api_url 为空
        let llm = Arc::new(HttpScenarioDetector::new(config));
        let detector = HybridScenarioDetector::new(Some(llm));
        let result = detector.detect(&turns).await;
        assert!(result.is_failed());
    }

    // ========================================================================
    // resolve_effective_scenario 测试
    // ========================================================================

    use memory_center_core::storage::LocalStorage;
    use tempfile::TempDir;

    fn make_storage() -> (TempDir, LocalStorage) {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path().to_path_buf());
        (tmp, storage)
    }

    #[tokio::test]
    async fn test_resolve_user_explicit_overrides_everything() {
        // 用户显式 > session_meta > 识别 > 默认
        let (_tmp, storage) = make_storage();
        let detector = HybridScenarioDetector::new(None);
        let family = AgentFamily::ClaudeCode;

        let result = resolve_effective_scenario(
            &storage,
            "sess-1",
            Some("writing"),
            &family,
            &detector,
            &[make_turn("fn compile", "好的")],
        ).await;
        assert_eq!(result, Scenario::Writing);
        // 用户显式不应写入 session_meta
        assert!(storage.read_session_meta("sess-1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_resolve_session_meta_hit_skips_detection() {
        // 已有 session_meta → 直接用，不调 detector
        let (_tmp, storage) = make_storage();
        let meta = SessionMeta {
            scenario: "research".to_string(),
            confidence: 0.85,
            method: "keyword".to_string(),
            detected_at: chrono::Utc::now(),
            agent_family: "ClaudeCode".to_string(),
            hook_mode: "real".to_string(),
        };
        storage.write_session_meta("sess-2", &meta).await.unwrap();

        let detector = HybridScenarioDetector::new(None);
        let family = AgentFamily::ClaudeCode;

        let result = resolve_effective_scenario(
            &storage,
            "sess-2",
            None,
            &family,
            &detector,
            &[make_turn("fn compile", "好的")], // 即使对话是 coding，也用 meta 的 research
        ).await;
        assert_eq!(result, Scenario::Research);
    }

    #[tokio::test]
    async fn test_resolve_first_archive_writes_meta() {
        // 首次 archive：识别 + 写 meta
        let (_tmp, storage) = make_storage();
        let detector = HybridScenarioDetector::new(None);
        let family = AgentFamily::ClaudeCode;

        let result = resolve_effective_scenario(
            &storage,
            "sess-3",
            None,
            &family,
            &detector,
            &[make_turn("fn compile refactor 调试", "架构")],
        ).await;
        assert_eq!(result, Scenario::Coding);

        // 验证 meta 已写入
        let meta = storage.read_session_meta("sess-3").await.unwrap().unwrap();
        assert_eq!(meta.scenario, "coding");
        assert_eq!(meta.method, "keyword");
    }

    #[tokio::test]
    async fn test_resolve_detection_failure_falls_back_to_agent_default() {
        // 识别失败（零命中 + 无 LLM）→ Agent 默认场景
        let (_tmp, storage) = make_storage();
        let detector = HybridScenarioDetector::new(None);
        let family = AgentFamily::ClaudeCode; // 默认 Coding

        let result = resolve_effective_scenario(
            &storage,
            "sess-4",
            None,
            &family,
            &detector,
            &[make_turn("啊啊啊", "嗯嗯嗯")],
        ).await;
        assert_eq!(result, Scenario::Coding, "ClaudeCode 默认应降级到 Coding");
    }
}
