//! # LLM 摘要生成器（v2.16 IMP-10 / v2.20 模板优先级链）
//!
//! 基于 [`hippocampus_core::generate::SummaryGenerator`] trait 的 HTTP 实现，
//! 通过调用外部 LLM API 为记忆文件生成结构化 [`Summary`]。
//!
//! ## 架构定位
//!
//! - **core**：定义 `SummaryGenerator` trait + `LlmGeneratorConfig`（纯逻辑）
//! - **llm**（本 crate）：实现 `HttpSummaryGenerator`（HTTP IO，依赖 reqwest）
//!
//! ## 生成流程
//!
//! 1. 从 MemoryFile 提取摘要信息（所有 turn 的文本内容）
//! 2. 构造 Prompt 发送给 LLM API
//! 3. 解析 LLM 返回的 JSON 结构化摘要
//! 4. 失败时降级返回启发式 `Summary::from_title`（不影响主流程）
//!
//! ## Prompt 策略（v2.20 模板优先级链）
//!
//! 支持通过 [`with_summary_template`](HttpSummaryGenerator::with_summary_template)
//! 注入自定义摘要模板，模板中的 `{conversation}` 占位符会被替换为实际对话内容。
//!
//! 优先级链（由调用方解析，本生成器只接收最终模板）：
//!
//! ```text
//! 用户 custom > ScenarioProfile.custom_summary_template > SummaryFocus 预设 > 默认硬编码
//! ```
//!
//! - **注入模板**：替换 `{conversation}` 后作为 prompt 发送
//! - **未注入**：使用默认硬编码 prompt（向后兼容）
//!
//! 要求 LLM 以严格 JSON 格式返回：
//!
//! ```json
//! {
//!   "title": "一句话标题",
//!   "abstract": "2-3 句话的摘要",
//!   "key_facts": ["事实1", "事实2"],
//!   "key_entities": ["实体1", "实体2"]
//! }
//! ```
//!
//! ## 错误处理
//!
//! - 未配置 API URL：降级为 `Summary::from_title`
//! - 网络错误 / API 错误 / 解析失败：降级为 `Summary::from_title`

use hippocampus_core::generate::{LlmGeneratorConfig, SummaryGenerator};
use hippocampus_core::model::{MemoryFile, Summary, Tag};
use serde::Deserialize;
use std::time::Duration;

/// LLM 返回的结构化摘要（用于反序列化）
#[derive(Debug, Deserialize)]
struct LlmSummary {
    /// 一句话标题
    title: String,
    /// 抽象摘要（2-3 句话）
    #[serde(default)]
    r#abstract: Option<String>,
    /// 关键事实列表
    #[serde(default)]
    key_facts: Vec<String>,
    /// 关键实体列表
    #[serde(default)]
    key_entities: Vec<String>,
}

/// HTTP LLM 摘要生成器
///
/// 通过调用外部 LLM API（OpenAI 兼容格式）为记忆文件生成结构化摘要。
///
/// ## 降级策略
///
/// - 未配置 API URL：降级为 `Summary::from_title`（启发式）
/// - 网络错误 / API 错误：降级为 `Summary::from_title`
/// - JSON 解析失败：降级为 `Summary::from_title`
///
/// ## 模板优先级链（v2.20）
///
/// 通过 [`with_summary_template`](Self::with_summary_template) 注入自定义模板，
/// 模板中的 `{conversation}` 占位符会被替换为实际对话内容。
/// 未注入时使用默认硬编码 prompt（向后兼容）。
pub struct HttpSummaryGenerator {
    /// 配置
    config: LlmGeneratorConfig,
    /// HTTP 客户端
    client: reqwest::Client,
    /// 自定义摘要模板（含 `{conversation}` 占位符）
    ///
    /// 由调用方（hippocampus-server / mcp）通过 CombinedProfile 解析后注入。
    /// None 时使用默认硬编码 prompt。
    summary_template: Option<String>,
}

impl HttpSummaryGenerator {
    /// 创建新的摘要生成器
    pub fn new(config: LlmGeneratorConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            config,
            client,
            summary_template: None,
        }
    }

    /// 注入自定义摘要模板（v2.20 模板优先级链）
    ///
    /// 模板需包含 `{conversation}` 占位符，该占位符会被替换为实际对话内容。
    ///
    /// ## 优先级链（由调用方解析）
    ///
    /// ```text
    /// 用户 custom > ScenarioProfile.custom_summary_template > SummaryFocus 预设 > 默认硬编码
    /// ```
    ///
    /// 调用方（hippocampus-server / mcp）通过 `CombinedProfile::summary_template()`
    /// 获取最终模板后注入本生成器。
    pub fn with_summary_template(mut self, template: impl Into<String>) -> Self {
        self.summary_template = Some(template.into());
        self
    }

    /// 从 MemoryFile 提取对话内容（用于构造 Prompt）
    fn extract_conversation(file: &MemoryFile) -> String {
        let mut parts = Vec::new();

        for (i, turn) in file.turns.iter().take(10).enumerate() {
            // 用户消息
            if let Some(text) = &turn.user_message.text {
                let truncated: String = text.chars().take(300).collect();
                parts.push(format!("用户[{}]: {}", i + 1, truncated));
            }
            // LLM 回复
            if let Some(text) = &turn.llm_message.text {
                let truncated: String = text.chars().take(300).collect();
                parts.push(format!("助手[{}]: {}", i + 1, truncated));
            }
        }

        let tags: Vec<String> = file.tags.iter().map(|t: &Tag| t.to_string()).collect();
        if !tags.is_empty() {
            parts.push(format!("标签: {}", tags.join(", ")));
        }

        parts.join("\n")
    }

    /// 构造摘要生成 Prompt
    ///
    /// 优先级：注入的 summary_template > 默认硬编码
    fn build_prompt(&self, file: &MemoryFile) -> String {
        let conversation = Self::extract_conversation(file);

        if let Some(template) = &self.summary_template {
            // 注入模板：替换 {conversation} 占位符
            return template.replace("{conversation}", &conversation);
        }

        // 默认硬编码 prompt（向后兼容）
        format!(
            r#"你是一个记忆摘要生成器。请为以下对话生成结构化摘要。

摘要要求：
- title: 一句话标题（≤30 字），概括对话主题
- abstract: 2-3 句话的摘要，提炼核心内容
- key_facts: 2-5 条关键事实（可被直接引用的陈述）
- key_entities: 1-5 个关键实体（人名/项目名/技术名词等）

对话内容：
{}

请以严格 JSON 格式返回（不要包含其他文本）：
{{"title": "标题", "abstract": "摘要", "key_facts": ["事实1"], "key_entities": ["实体1"]}}"#,
            conversation
        )
    }

    /// 启发式降级：从首条用户消息生成 title
    fn heuristic_fallback(file: &MemoryFile) -> Summary {
        if let Some(first_turn) = file.turns.first() {
            if let Some(text) = &first_turn.user_message.text {
                let title: String = text.chars().take(80).collect();
                return Summary::from_title(title);
            }
        }
        Summary::from_title("(空记忆)")
    }
}

#[async_trait::async_trait]
impl SummaryGenerator for HttpSummaryGenerator {
    async fn generate_summary(&self, file: &MemoryFile) -> hippocampus_core::Result<Summary> {
        // 未配置 API URL：降级为启发式
        if self.config.api_url.is_empty() {
            tracing::warn!("摘要生成器未配置 api_url，降级为启发式");
            return Ok(Self::heuristic_fallback(file));
        }

        let prompt = self.build_prompt(file);

        let request_body = serde_json::json!({
            "model": self.config.model,
            "messages": [
                {"role": "user", "content": prompt}
            ],
            "max_tokens": self.config.max_tokens,
            "temperature": 0.0,
        });

        let resp = self
            .client
            .post(&self.config.api_url)
            .bearer_auth(&self.config.api_key)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| hippocampus_core::Error::Storage(format!("LLM API 请求失败: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(status = %status, body = %body, "LLM API 返回错误状态，降级为启发式");
            return Ok(Self::heuristic_fallback(file));
        }

        let resp_json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| hippocampus_core::Error::Storage(format!("LLM API 响应解析失败: {}", e)))?;

        let content = resp_json
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("");

        // 尝试解析 JSON
        if let Ok(llm_summary) = serde_json::from_str::<LlmSummary>(content) {
            let title = llm_summary.title.trim().to_string();
            if !title.is_empty() {
                return Ok(Summary {
                    title,
                    abstract_text: llm_summary.r#abstract.filter(|s| !s.trim().is_empty()),
                    key_facts: llm_summary
                        .key_facts
                        .into_iter()
                        .filter(|s| !s.trim().is_empty())
                        .map(|s| s.trim().to_string())
                        .collect(),
                    key_entities: llm_summary
                        .key_entities
                        .into_iter()
                        .filter(|s| !s.trim().is_empty())
                        .map(|s| s.trim().to_string())
                        .collect(),
                    clue_anchors: Vec::new(), // 锚点由 AnchorGenerator 单独生成
                });
            }
        }

        tracing::warn!(raw = %content, "无法从 LLM 响应中解析摘要，降级为启发式");
        Ok(Self::heuristic_fallback(file))
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use hippocampus_core::model::{ArchivePeriod, MessageContent, MessageTurn};
    use uuid::Uuid;

    fn make_memory() -> MemoryFile {
        let turn = MessageTurn {
            id: Uuid::new_v4(),
            user_message: MessageContent {
                text: Some("讨论 Rust 记忆库的三级索引架构设计".into()),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                thinking: None,
            },
            llm_message: MessageContent {
                text: Some("建议采用 daily/weekly/monthly 三级周期，天级归档、周级合并、月级淘汰".into()),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                thinking: None,
            },
            tags: vec![Tag::Text, Tag::CodeBlock],
            timestamp: chrono::Utc::now(),
            token_count: 100,
        };
        MemoryFile::new("test-session", None, vec![turn], ArchivePeriod::Weekly)
    }

    #[test]
    fn test_extract_conversation() {
        let file = make_memory();
        let conv = HttpSummaryGenerator::extract_conversation(&file);
        assert!(conv.contains("用户[1]:"));
        assert!(conv.contains("助手[1]:"));
        assert!(conv.contains("Rust 记忆库"));
    }

    #[test]
    fn test_build_prompt() {
        let config = LlmGeneratorConfig::default();
        let gen = HttpSummaryGenerator::new(config);
        let file = make_memory();
        let prompt = gen.build_prompt(&file);
        assert!(prompt.contains("title"));
        assert!(prompt.contains("abstract"));
        assert!(prompt.contains("key_facts"));
        assert!(prompt.contains("JSON"));
    }

    #[test]
    fn test_build_prompt_with_template() {
        // 注入自定义模板后，应使用模板而非默认 prompt
        let config = LlmGeneratorConfig::default();
        let gen = HttpSummaryGenerator::new(config)
            .with_summary_template("请为以下对话生成摘要：{conversation}（要求 JSON 格式）");
        let file = make_memory();
        let prompt = gen.build_prompt(&file);
        // 模板前缀存在
        assert!(prompt.starts_with("请为以下对话生成摘要："));
        // {conversation} 占位符已被替换为实际对话内容
        assert!(prompt.contains("用户[1]:"));
        assert!(prompt.contains("助手[1]:"));
        assert!(prompt.contains("Rust 记忆库"));
        // 不应包含默认 prompt 的特征词
        assert!(!prompt.contains("你是一个记忆摘要生成器"));
        // 占位符已被替换（不应再出现 {conversation}）
        assert!(!prompt.contains("{conversation}"));
    }

    #[test]
    fn test_build_prompt_template_without_placeholder() {
        // 模板不含 {conversation} 占位符时，原样返回（不追加对话内容）
        let config = LlmGeneratorConfig::default();
        let gen = HttpSummaryGenerator::new(config)
            .with_summary_template("固定 prompt，无占位符");
        let file = make_memory();
        let prompt = gen.build_prompt(&file);
        assert_eq!(prompt, "固定 prompt，无占位符");
    }

    #[test]
    fn test_heuristic_fallback() {
        let file = make_memory();
        let summary = HttpSummaryGenerator::heuristic_fallback(&file);
        assert!(summary.title.contains("Rust 记忆库"));
        assert!(summary.abstract_text.is_none());
        assert!(summary.key_facts.is_empty());
    }

    #[tokio::test]
    async fn test_generate_summary_without_api_url_degrades() {
        let config = LlmGeneratorConfig::default(); // api_url 为空
        let gen = HttpSummaryGenerator::new(config);
        let file = make_memory();
        let summary = gen.generate_summary(&file).await.unwrap();
        // 应降级为启发式
        assert!(summary.title.contains("Rust 记忆库"));
        assert!(summary.abstract_text.is_none());
    }

    #[test]
    fn test_parse_json_summary() {
        let json = r#"{"title":"三级索引设计","abstract":"讨论了记忆库架构","key_facts":["采用三级周期"],"key_entities":["Rust","Hippocampus"]}"#;
        let parsed: LlmSummary = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.title, "三级索引设计");
        assert_eq!(parsed.r#abstract.as_ref().unwrap(), "讨论了记忆库架构");
        assert_eq!(parsed.key_facts.len(), 1);
        assert_eq!(parsed.key_entities.len(), 2);
    }
}
