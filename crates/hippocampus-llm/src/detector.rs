//! # LLM 冲突检测器（v2.10）
//!
//! 基于 [`hippocampus_core::conflict::ConflictDetector`] trait 的 HTTP 实现，
//! 通过调用外部 LLM API 进行语义级冲突检测，弥补 HeuristicDetector 的局限。
//!
//! ## 架构定位
//!
//! - **core**：定义 `ConflictDetector` trait + `ConflictReport`（纯逻辑）
//! - **llm**（本 crate）：实现 `HttpLlmDetector`（HTTP IO，依赖 reqwest）
//!
//! ## 检测流程
//!
//! 1. 从 MemoryUpdate + MemoryFile 提取事实集（added/revised/deprecated + 历史事实）
//! 2. 构造结构化 Prompt 发送给 LLM API
//! 3. 解析 LLM 返回的 JSON 冲突列表
//! 4. 失败时降级返回空报告（不阻塞更新，与 HeuristicDetector 行为一致）
//!
//! ## Prompt 策略
//!
//! 要求 LLM 以严格 JSON 格式返回，便于解析：
//!
//! ```json
//! {
//!   "conflicts": [
//!     {
//!       "kind": "direct_contradict",
//!       "severity": "critical",
//!       "description": "用户先说喜欢咖啡，后说不喜欢咖啡",
//!       "existing_fact": "用户喜欢咖啡",
//!       "new_fact": "用户不喜欢咖啡"
//!     }
//!   ]
//! }
//! ```
//!
//! ## 与 HeuristicDetector 的关系
//!
//! - **互补**：HeuristicDetector 擅长精确反义词匹配，LlmDetector 擅长语义级矛盾
//! - **可组合**：未来可设计 `HybridDetector` 串联两者（启发式先行 + LLM 补充）
//! - **降级**：LLM 失败时返回空报告，不影响更新流程
//!
//! ## 错误处理
//!
//! - 网络错误 / API 错误 / 解析失败：返回空报告（降级策略）
//! - 超时：按配置 `timeout_secs` 处理

use hippocampus_core::conflict::{
    ConflictDetector, ConflictKind, ConflictRecord, ConflictReport, Severity,
};
use hippocampus_core::model::{MemoryFile, MemoryUpdate};
use serde::Deserialize;
use std::time::Duration;

// ============================================================================
// 配置
// ============================================================================

/// LLM 冲突检测器配置（v2.10 新增，v2.13 默认值更新）
///
/// 与 `LlmScorerConfig` 独立，因为检测器的 prompt 和 token 需求不同。
#[derive(Debug, Clone)]
pub struct LlmDetectorConfig {
    /// LLM API 端点 URL（OpenAI 兼容 /v1/chat/completions）
    pub api_url: String,
    /// API Key（Bearer token）
    pub api_key: String,
    /// 模型名称（如 gpt-5.5-instant / deepseek-chat）
    pub model: String,
    /// 请求超时（秒），默认 30
    pub timeout_secs: u64,
    /// 最大 token 数（限制 LLM 输出），默认 500（冲突报告需要更多空间）
    pub max_tokens: u32,
}

impl Default for LlmDetectorConfig {
    fn default() -> Self {
        Self {
            api_url: String::new(),
            api_key: String::new(),
            // v2.13：默认值更新为 2026-06-24 发布的 gpt-5.5-instant（ChatGPT 默认模型，幻觉降 52%）
            model: "gpt-5.5-instant".into(),
            timeout_secs: 30,
            max_tokens: 500,
        }
    }
}

impl LlmDetectorConfig {
    /// 环境变量前缀
    pub const ENV_PREFIX: &'static str = "HIPPOCAMPUS_DETECTOR";

    /// 从环境变量构造配置（v2.13 新增）
    ///
    /// 读取以下环境变量（前缀 `HIPPOCAMPUS_DETECTOR_`）：
    ///
    /// | 环境变量 | 字段 | 默认值 |
    /// |---------|------|--------|
    /// | `_API_URL` | `api_url` | 必填（缺失返回 None） |
    /// | `_API_KEY` | `api_key` | 必填（缺失返回 None） |
    /// | `_MODEL` | `model` | `gpt-5.5-instant` |
    /// | `_TIMEOUT` | `timeout_secs` | `30` |
    /// | `_MAX_TOKENS` | `max_tokens` | `500` |
    ///
    /// ## 返回
    ///
    /// - `Some(config)`：`api_url` 和 `api_key` 均非空
    /// - `None`：`api_url` 或 `api_key` 为空（调用方应降级为 HeuristicDetector）
    pub fn from_env() -> Option<Self> {
        let api_url = std::env::var(format!("{}_API_URL", Self::ENV_PREFIX)).ok()?;
        let api_key = std::env::var(format!("{}_API_KEY", Self::ENV_PREFIX)).ok()?;
        if api_url.is_empty() || api_key.is_empty() {
            return None;
        }

        let config = Self {
            api_url,
            api_key,
            model: std::env::var(format!("{}_MODEL", Self::ENV_PREFIX))
                .unwrap_or_else(|_| Self::default().model),
            timeout_secs: std::env::var(format!("{}_TIMEOUT", Self::ENV_PREFIX))
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(30),
            max_tokens: std::env::var(format!("{}_MAX_TOKENS", Self::ENV_PREFIX))
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(500),
        };
        Some(config)
    }
}

// ============================================================================
// LLM 响应结构（用于反序列化）
// ============================================================================

/// LLM 返回的单条冲突（用于反序列化）
///
/// 字段名与 `ConflictRecord` 对应，但用 snake_case 字符串接收 kind/severity。
#[derive(Debug, Deserialize)]
struct LlmConflict {
    /// 冲突类型字符串："self_contradict" / "direct_contradict" / "stance_reversal"
    kind: String,
    /// 严重级别字符串："info" / "warning" / "critical"
    severity: String,
    /// 中文描述
    description: String,
    /// 冲突的已有事实（可选）
    #[serde(default)]
    existing_fact: Option<String>,
    /// 新事实
    new_fact: String,
}

/// LLM 返回的完整冲突报告（用于反序列化）
#[derive(Debug, Deserialize)]
struct LlmConflictReport {
    /// 冲突列表
    #[serde(default)]
    conflicts: Vec<LlmConflict>,
}

// ============================================================================
// HttpLlmDetector
// ============================================================================

/// HTTP LLM 冲突检测器
///
/// 通过调用外部 LLM API（OpenAI 兼容格式）进行语义级冲突检测。
///
/// ## 降级策略
///
/// - 未配置 API URL：返回空报告
/// - 网络错误 / API 错误：返回空报告
/// - JSON 解析失败：返回空报告
/// - 单条冲突字段无效：跳过该条，保留有效项
pub struct HttpLlmDetector {
    /// 配置
    config: LlmDetectorConfig,
    /// HTTP 客户端
    client: reqwest::Client,
}

impl HttpLlmDetector {
    /// 创建新的 LLM 冲突检测器
    pub fn new(config: LlmDetectorConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { config, client }
    }

    /// 从 MemoryUpdate + MemoryFile 提取事实集，构造 Prompt
    fn build_prompt(update: &MemoryUpdate, existing_memory: &MemoryFile) -> String {
        // 提取历史事实集（与 HeuristicDetector::extract_historical_facts 一致）
        let historical_facts: Vec<&String> = existing_memory
            .updates
            .iter()
            .flat_map(|r| r.update.added_facts.iter())
            .collect();

        let added = if update.added_facts.is_empty() {
            "（无）".to_string()
        } else {
            update.added_facts.iter().map(|s| format!("- {}", s)).collect::<Vec<_>>().join("\n")
        };

        let revised = if update.revised_facts.is_empty() {
            "（无）".to_string()
        } else {
            update.revised_facts.iter().map(|s| format!("- {}", s)).collect::<Vec<_>>().join("\n")
        };

        let deprecated = if update.deprecated_facts.is_empty() {
            "（无）".to_string()
        } else {
            update.deprecated_facts.iter().map(|s| format!("- {}", s)).collect::<Vec<_>>().join("\n")
        };

        let historical = if historical_facts.is_empty() {
            "（无）".to_string()
        } else {
            historical_facts.iter().map(|s| format!("- {}", s)).collect::<Vec<_>>().join("\n")
        };

        format!(
            r#"你是一个记忆冲突检测器。请检测以下记忆更新与已有事实之间的冲突。

## 待应用的更新

新增事实：
{}

修正事实：
{}

废弃事实：
{}

## 已有历史事实

{}

## 冲突类型说明

- self_contradict：自我矛盾（同一批更新内既添加又废弃相似事实）
- direct_contradict：直接矛盾（新事实与已有事实语义相反）
- stance_reversal：立场反转（废弃的事实与历史新增事实一致，即推翻原立场）

## 严重级别

- critical：明确矛盾
- warning：可能矛盾
- info：信息性提示

## 输出要求

请只返回 JSON，不要包含任何解释或 markdown 标记。格式如下：

{{"conflicts": [{{"kind": "direct_contradict", "severity": "critical", "description": "冲突描述", "existing_fact": "已有事实", "new_fact": "新事实"}}]}}

若无冲突，返回：{{"conflicts": []}}"#,
            added, revised, deprecated, historical
        )
    }

    /// 解析 LLM 返回的 JSON 报告
    ///
    /// 宽松解析：单条冲突字段无效时跳过，保留有效项。
    fn parse_report(raw: &str) -> ConflictReport {
        // 尝试直接解析
        let llm_report: LlmConflictReport = match serde_json::from_str(raw) {
            Ok(r) => r,
            Err(_) => {
                // 尝试从 markdown 代码块中提取 JSON
                let trimmed = Self::extract_json_from_markdown(raw);
                match serde_json::from_str(&trimmed) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(error = %e, raw = %raw, "LLM 响应 JSON 解析失败，降级返回空报告");
                        return ConflictReport::empty();
                    }
                }
            }
        };

        let mut report = ConflictReport::empty();
        for c in llm_report.conflicts {
            // 解析 kind
            let kind = match c.kind.as_str() {
                "self_contradict" => ConflictKind::SelfContradict,
                "direct_contradict" => ConflictKind::DirectContradict,
                "stance_reversal" => ConflictKind::StanceReversal,
                other => {
                    tracing::warn!(kind = %other, "未知冲突类型，跳过");
                    continue;
                }
            };

            // 解析 severity
            let severity = match c.severity.as_str() {
                "info" => Severity::Info,
                "warning" => Severity::Warning,
                "critical" => Severity::Critical,
                other => {
                    tracing::warn!(severity = %other, "未知严重级别，跳过");
                    continue;
                }
            };

            report.push(ConflictRecord {
                kind,
                severity,
                description: c.description,
                existing_fact: c.existing_fact,
                new_fact: c.new_fact,
            });
        }
        report
    }

    /// 从 markdown 代码块中提取 JSON
    ///
    /// LLM 有时会返回 ```json ... ``` 包裹的内容，需要剥离。
    fn extract_json_from_markdown(raw: &str) -> String {
        let trimmed = raw.trim();
        // 剥离 ```json ... ``` 或 ``` ... ```
        if let Some(start) = trimmed.find("```") {
            let after = &trimmed[start + 3..];
            // 跳过 "json" 语言标记
            let after = after.strip_prefix("json").unwrap_or(after);
            if let Some(end) = after.find("```") {
                return after[..end].trim().to_string();
            }
        }
        trimmed.to_string()
    }
}

#[async_trait::async_trait]
impl ConflictDetector for HttpLlmDetector {
    async fn detect(
        &self,
        update: &MemoryUpdate,
        existing_memory: &MemoryFile,
    ) -> ConflictReport {
        // 未配置 API URL → 降级返回空报告
        if self.config.api_url.is_empty() {
            tracing::warn!("LLM 冲突检测器未配置 api_url，降级返回空报告");
            return ConflictReport::empty();
        }

        let prompt = Self::build_prompt(update, existing_memory);

        let request_body = serde_json::json!({
            "model": self.config.model,
            "messages": [
                {"role": "user", "content": prompt}
            ],
            "max_tokens": self.config.max_tokens,
            "temperature": 0.0,
            // v2.24：关闭思考模式（DeepSeek V4 Flash 默认启用思考模式，
            // 输出会进入 reasoning_content 而 content 为空，导致解析失败）
            // 此参数对不支持 thinking 的 API（如 OpenAI/SenseNova）无害，会被忽略
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
                tracing::warn!(error = %e, "LLM 冲突检测 API 请求失败，降级返回空报告");
                return ConflictReport::empty();
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(status = %status, body = %body, "LLM 冲突检测 API 返回错误状态，降级返回空报告");
            return ConflictReport::empty();
        }

        let resp_json: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "LLM 冲突检测响应解析失败，降级返回空报告");
                return ConflictReport::empty();
            }
        };

        // OpenAI 兼容格式：choices[0].message.content
        let content = resp_json
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("");

        Self::parse_report(content)
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use hippocampus_core::model::{
        ArchivePeriod, MemoryUpdateRecord, MessageContent, MessageTurn,
    };
    use chrono::Utc;
    use uuid::Uuid;

    // ============================================================================
    // 测试辅助
    // ============================================================================

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

    fn make_memory_with_history(facts: Vec<&str>) -> MemoryFile {
        let mut memory = make_test_memory();
        let update = MemoryUpdate::new().add_fact(facts.join("\n"));
        memory.updates.push(MemoryUpdateRecord {
            updated_at: Utc::now(),
            update,
            conflicts: vec![],
        });
        memory
    }

    // ============================================================================
    // Prompt 构造测试
    // ============================================================================

    #[test]
    fn test_build_prompt_empty_update() {
        let memory = make_test_memory();
        let update = MemoryUpdate::new();
        let prompt = HttpLlmDetector::build_prompt(&update, &memory);

        assert!(prompt.contains("新增事实："));
        assert!(prompt.contains("（无）"));
        assert!(prompt.contains("已有历史事实"));
        assert!(prompt.contains("self_contradict"));
        assert!(prompt.contains("direct_contradict"));
        assert!(prompt.contains("stance_reversal"));
    }

    #[test]
    fn test_build_prompt_with_facts() {
        let memory = make_memory_with_history(vec!["用户喜欢咖啡"]);
        let update = MemoryUpdate::new()
            .add_fact("用户不喜欢咖啡")
            .deprecate_fact("用户喜欢咖啡");

        let prompt = HttpLlmDetector::build_prompt(&update, &memory);

        assert!(prompt.contains("- 用户不喜欢咖啡"));
        assert!(prompt.contains("- 用户喜欢咖啡"));
        assert!(prompt.contains("用户喜欢咖啡")); // 历史事实
    }

    #[test]
    fn test_build_prompt_with_revised_facts() {
        let memory = make_test_memory();
        let update = MemoryUpdate::new().revise_fact("修正后的事实");
        let prompt = HttpLlmDetector::build_prompt(&update, &memory);

        assert!(prompt.contains("- 修正后的事实"));
    }

    // ============================================================================
    // JSON 解析测试
    // ============================================================================

    #[test]
    fn test_parse_report_empty() {
        let raw = r#"{"conflicts": []}"#;
        let report = HttpLlmDetector::parse_report(raw);
        assert!(report.is_clean());
    }

    #[test]
    fn test_parse_report_single_conflict() {
        let raw = r#"{"conflicts": [{"kind": "direct_contradict", "severity": "critical", "description": "用户先说喜欢，后说不喜欢", "existing_fact": "用户喜欢咖啡", "new_fact": "用户不喜欢咖啡"}]}"#;
        let report = HttpLlmDetector::parse_report(raw);

        assert_eq!(report.count(), 1);
        assert!(report.has_critical());
        let c = &report.conflicts[0];
        assert_eq!(c.kind, ConflictKind::DirectContradict);
        assert_eq!(c.severity, Severity::Critical);
        assert_eq!(c.new_fact, "用户不喜欢咖啡");
    }

    #[test]
    fn test_parse_report_multiple_conflicts() {
        let raw = r#"{"conflicts": [
            {"kind": "self_contradict", "severity": "critical", "description": "自我矛盾", "new_fact": "A"},
            {"kind": "stance_reversal", "severity": "warning", "description": "立场反转", "existing_fact": "旧", "new_fact": "新"}
        ]}"#;
        let report = HttpLlmDetector::parse_report(raw);

        assert_eq!(report.count(), 2);
        assert!(report.has_critical());
        assert_eq!(report.by_severity(Severity::Critical).len(), 1);
        assert_eq!(report.by_severity(Severity::Warning).len(), 1);
    }

    #[test]
    fn test_parse_report_invalid_json_returns_empty() {
        let raw = "这不是 JSON";
        let report = HttpLlmDetector::parse_report(raw);
        assert!(report.is_clean());
    }

    #[test]
    fn test_parse_report_unknown_kind_skipped() {
        let raw = r#"{"conflicts": [
            {"kind": "unknown_type", "severity": "critical", "description": "未知类型", "new_fact": "A"},
            {"kind": "direct_contradict", "severity": "critical", "description": "有效冲突", "new_fact": "B"}
        ]}"#;
        let report = HttpLlmDetector::parse_report(raw);

        // 未知类型应被跳过，只保留有效项
        assert_eq!(report.count(), 1);
        assert_eq!(report.conflicts[0].new_fact, "B");
    }

    #[test]
    fn test_parse_report_unknown_severity_skipped() {
        let raw = r#"{"conflicts": [
            {"kind": "direct_contradict", "severity": "extreme", "description": "未知级别", "new_fact": "A"}
        ]}"#;
        let report = HttpLlmDetector::parse_report(raw);

        assert!(report.is_clean(), "未知 severity 应被跳过");
    }

    #[test]
    fn test_parse_report_markdown_wrapped() {
        let raw = r#"```json
{"conflicts": [{"kind": "self_contradict", "severity": "critical", "description": "测试", "new_fact": "A"}]}
```"#;
        let report = HttpLlmDetector::parse_report(raw);

        assert_eq!(report.count(), 1);
        assert_eq!(report.conflicts[0].kind, ConflictKind::SelfContradict);
    }

    #[test]
    fn test_parse_report_missing_existing_fact() {
        // existing_fact 缺失（serde default）
        let raw = r#"{"conflicts": [{"kind": "self_contradict", "severity": "critical", "description": "测试", "new_fact": "A"}]}"#;
        let report = HttpLlmDetector::parse_report(raw);

        assert_eq!(report.count(), 1);
        assert!(report.conflicts[0].existing_fact.is_none());
    }

    #[test]
    fn test_extract_json_from_markdown_plain() {
        let raw = r#"{"conflicts": []}"#;
        let extracted = HttpLlmDetector::extract_json_from_markdown(raw);
        assert_eq!(extracted, r#"{"conflicts": []}"#);
    }

    #[test]
    fn test_extract_json_from_markdown_json_block() {
        let raw = "```json\n{\"conflicts\": []}\n```";
        let extracted = HttpLlmDetector::extract_json_from_markdown(raw);
        assert_eq!(extracted, r#"{"conflicts": []}"#);
    }

    #[test]
    fn test_extract_json_from_markdown_plain_block() {
        let raw = "```\n{\"conflicts\": []}\n```";
        let extracted = HttpLlmDetector::extract_json_from_markdown(raw);
        assert_eq!(extracted, r#"{"conflicts": []}"#);
    }

    // ============================================================================
    // 降级测试
    // ============================================================================

    #[tokio::test]
    async fn test_detect_without_api_url_returns_empty() {
        let config = LlmDetectorConfig::default(); // api_url 为空
        let detector = HttpLlmDetector::new(config);
        let memory = make_test_memory();
        let update = MemoryUpdate::new().add_fact("新事实");

        let report = detector.detect(&update, &memory).await;
        assert!(report.is_clean());
    }

    #[tokio::test]
    async fn test_detect_with_empty_history() {
        // 无历史事实时，prompt 应正常构造
        let config = LlmDetectorConfig::default();
        let detector = HttpLlmDetector::new(config);
        let memory = make_test_memory();
        let update = MemoryUpdate::new().add_fact("新事实");

        // 未配置 API URL，应降级返回空报告
        let report = detector.detect(&update, &memory).await;
        assert!(report.is_clean());
    }

    #[test]
    fn test_llm_detector_config_default() {
        let config = LlmDetectorConfig::default();
        assert!(config.api_url.is_empty());
        assert!(config.api_key.is_empty());
        // v2.13：默认值更新为 gpt-5.5-instant
        assert_eq!(config.model, "gpt-5.5-instant");
        assert_eq!(config.timeout_secs, 30);
        assert_eq!(config.max_tokens, 500);
    }
}
