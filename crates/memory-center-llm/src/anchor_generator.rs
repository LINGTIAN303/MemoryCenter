//! # LLM 线索锚点生成器（v2.16 IMP-05）
//!
//! 基于 [`memory_center_core::generate::AnchorGenerator`] trait 的 HTTP 实现，
//! 通过调用外部 LLM API 从记忆文件提取检索锚点（`clue_anchors`）。
//!
//! ## 架构定位
//!
//! - **core**：定义 `AnchorGenerator` trait + `LlmGeneratorConfig`（纯逻辑）
//! - **llm**（本 crate）：实现 `HttpAnchorGenerator`（HTTP IO，依赖 reqwest）
//!
//! ## 生成流程
//!
//! 1. 从 MemoryFile 提取摘要信息（标题 + 首条消息 + 标签）
//! 2. 构造 Prompt 发送给 LLM API
//! 3. 解析 LLM 返回的锚点列表（JSON 数组或换行分隔）
//! 4. 失败时降级返回空 Vec（不影响主流程）
//!
//! ## Prompt 策略
//!
//! 要求 LLM 以严格 JSON 格式返回：
//!
//! ```json
//! {"anchors": ["锚点1", "锚点2", "锚点3"]}
//! ```
//!
//! ## 错误处理
//!
//! - 未配置 API URL：返回空 Vec（降级）
//! - 网络错误 / API 错误 / 解析失败：返回空 Vec（降级）

use memory_center_core::generate::{AnchorGenerator, LlmGeneratorConfig};
use memory_center_core::model::{MemoryFile, Tag};
use serde::Deserialize;
use std::time::Duration;

/// LLM 返回的锚点列表（用于反序列化）
#[derive(Debug, Deserialize)]
struct LlmAnchors {
    #[serde(default)]
    anchors: Vec<String>,
}

/// HTTP LLM 线索锚点生成器
///
/// 通过调用外部 LLM API（OpenAI 兼容格式）从记忆文件提取检索锚点。
///
/// ## 降级策略
///
/// - 未配置 API URL：返回空 Vec
/// - 网络错误 / API 错误：返回空 Vec
/// - JSON 解析失败：返回空 Vec
pub struct HttpAnchorGenerator {
    /// 配置
    config: LlmGeneratorConfig,
    /// HTTP 客户端
    client: reqwest::Client,
}

impl HttpAnchorGenerator {
    /// 创建新的锚点生成器
    pub fn new(config: LlmGeneratorConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { config, client }
    }

    /// 从 MemoryFile 提取摘要信息（用于构造 Prompt）
    fn extract_summary_text(file: &MemoryFile) -> String {
        let mut parts = Vec::new();

        if let Some(first_turn) = file.turns.first() {
            if let Some(text) = &first_turn.user_message.text {
                let title: String = text.chars().take(80).collect();
                parts.push(format!("标题: {}", title));

                // 完整内容（截断到 300 字符，锚点生成需要更多上下文）
                let truncated: String = text.chars().take(300).collect();
                parts.push(format!("内容: {}", truncated));
            }
        }

        // 汇总所有 turn 的 LLM 回复（截断）
        let llm_texts: Vec<String> = file
            .turns
            .iter()
            .filter_map(|t| t.llm_message.text.as_ref())
            .take(3)
            .map(|t| t.chars().take(200).collect())
            .collect();
        if !llm_texts.is_empty() {
            parts.push(format!("回复摘要: {}", llm_texts.join(" / ")));
        }

        let tags: Vec<String> = file.tags.iter().map(|t: &Tag| t.to_string()).collect();
        if !tags.is_empty() {
            parts.push(format!("标签: {}", tags.join(", ")));
        }

        parts.join("\n")
    }

    /// 构造锚点生成 Prompt
    fn build_prompt(&self, file: &MemoryFile) -> String {
        let summary = Self::extract_summary_text(file);
        format!(
            r#"你是一个记忆检索锚点提取器。请从以下记忆片段中提取 3-5 个高辨识度的检索锚点。

锚点要求：
- 应为关键词或短语（2-8 字）
- 具有高辨识度，能唯一指向这段记忆
- 优先选择实体名、技术名词、核心概念

记忆内容：
{}

请以严格 JSON 格式返回（不要包含其他文本）：
{{"anchors": ["锚点1", "锚点2", "锚点3"]}}"#,
            summary
        )
    }
}

#[async_trait::async_trait]
impl AnchorGenerator for HttpAnchorGenerator {
    async fn generate_anchors(&self, file: &MemoryFile) -> memory_center_core::Result<Vec<String>> {
        // 未配置 API URL：降级返回空 Vec
        if self.config.api_url.is_empty() {
            tracing::warn!("锚点生成器未配置 api_url，降级返回空列表");
            return Ok(Vec::new());
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
            .map_err(|e| memory_center_core::Error::Storage(format!("LLM API 请求失败: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(status = %status, body = %body, "LLM API 返回错误状态");
            return Ok(Vec::new()); // 降级
        }

        let resp_json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| memory_center_core::Error::Storage(format!("LLM API 响应解析失败: {}", e)))?;

        let content = resp_json
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("");

        // 尝试解析 JSON
        if let Ok(anchors) = serde_json::from_str::<LlmAnchors>(content) {
            let filtered: Vec<String> = anchors
                .anchors
                .into_iter()
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_string())
                .collect();
            if !filtered.is_empty() {
                return Ok(filtered);
            }
        }

        // JSON 解析失败，尝试从纯文本提取（每行一个锚点）
        let lines: Vec<String> = content
            .lines()
            .map(|l| l.trim().trim_matches(|c| c == '"' || c == ',' || c == '-' || c == '•'))
            .filter(|l| !l.is_empty() && l.len() <= 50)
            .take(5)
            .map(|l| l.to_string())
            .collect();
        if !lines.is_empty() {
            return Ok(lines);
        }

        tracing::warn!(raw = %content, "无法从 LLM 响应中解析锚点，降级返回空列表");
        Ok(Vec::new())
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use memory_center_core::model::{ArchivePeriod, MessageContent, MessageTurn};
    use uuid::Uuid;

    fn make_memory() -> MemoryFile {
        let turn = MessageTurn {
            id: Uuid::new_v4(),
            user_message: MessageContent {
                text: Some("讨论 Rust 记忆库的三级索引架构设计".into()),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                thinking: None,
                file_changes: Vec::new(),
            },
            llm_message: MessageContent {
                text: Some("建议采用 daily/weekly/monthly 三级周期".into()),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                thinking: None,
                file_changes: Vec::new(),
            },
            tags: vec![Tag::Text, Tag::CodeBlock],
            timestamp: chrono::Utc::now(),
            token_count: 100,
            stop_reason: None,
            cost: None,
        };
        MemoryFile::new("test-session", None, vec![turn], ArchivePeriod::Weekly)
    }

    #[test]
    fn test_extract_summary_text() {
        let file = make_memory();
        let text = HttpAnchorGenerator::extract_summary_text(&file);
        assert!(text.contains("标题:"));
        assert!(text.contains("Rust 记忆库"));
        assert!(text.contains("标签:"));
    }

    #[test]
    fn test_build_prompt() {
        let config = LlmGeneratorConfig::default();
        let gen = HttpAnchorGenerator::new(config);
        let file = make_memory();
        let prompt = gen.build_prompt(&file);
        assert!(prompt.contains("锚点"));
        assert!(prompt.contains("JSON"));
        assert!(prompt.contains("Rust 记忆库"));
    }

    #[tokio::test]
    async fn test_generate_anchors_without_api_url_returns_empty() {
        let config = LlmGeneratorConfig::default(); // api_url 为空
        let gen = HttpAnchorGenerator::new(config);
        let file = make_memory();
        let anchors = gen.generate_anchors(&file).await.unwrap();
        assert!(anchors.is_empty());
    }

    #[test]
    fn test_parse_json_anchors() {
        let json = r#"{"anchors": ["Rust", "记忆库", "三级索引"]}"#;
        let parsed: LlmAnchors = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.anchors.len(), 3);
        assert_eq!(parsed.anchors[0], "Rust");
    }
}
