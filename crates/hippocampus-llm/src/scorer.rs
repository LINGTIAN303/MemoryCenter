//! # LLM 评分器实现
//!
//! 基于 [`hippocampus_core::score::AsyncScorer`] trait 的 HTTP 实现，
//! 通过调用外部 LLM API 评估记忆文件的 topic_relevance（主题相关性）。
//!
//! ## 架构定位
//!
//! - **core**：定义 `AsyncScorer` trait + `LlmScorerConfig` 配置（纯逻辑）
//! - **llm**（本 crate）：实现 `HttpLlmScorer`（HTTP IO，依赖 reqwest）
//!
//! ## 评分流程
//!
//! 1. 从 MemoryFile 提取摘要信息（标题 + 首条消息文本 + 标签）
//! 2. 构造 Prompt 发送给 LLM API
//! 3. 解析 LLM 返回的分数（0-100）
//! 4. 失败时降级返回 50 分（中性分数，不影响排序）
//!
//! ## Prompt 策略
//!
//! ```text
//! 你是一个记忆相关性评估器。请评估以下记忆片段与主题"{topic}"的相关性。
//!
//! 主题：{topic}
//! 记忆标题：{title}
//! 记忆内容：{content}
//! 标签：{tags}
//!
//! 请只返回一个 0-100 的整数分数，不需要解释。
//! 100 表示高度相关，0 表示完全无关。
//! ```
//!
//! ## 错误处理
//!
//! - 网络错误 / API 错误 / 解析失败：返回 50 分（降级策略）
//! - 超时：按配置 `timeout_secs` 处理

use hippocampus_core::model::{MemoryFile, Tag};
use hippocampus_core::score::{AsyncScorer, LlmScorerConfig};
use std::time::Duration;

/// HTTP LLM 评分器
///
/// 通过调用外部 LLM API（OpenAI 兼容格式）评估记忆的主题相关性。
pub struct HttpLlmScorer {
    /// 配置
    config: LlmScorerConfig,
    /// HTTP 客户端
    client: reqwest::Client,
}

impl HttpLlmScorer {
    /// 创建新的 LLM 评分器
    pub fn new(config: LlmScorerConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self { config, client }
    }

    /// 从 MemoryFile 提取摘要信息（用于构造 Prompt）
    fn extract_summary_text(file: &MemoryFile) -> String {
        let mut parts = Vec::new();

        // 标题：用首条用户消息前 80 字符作为标题（与 IndexHook 启发式一致）
        if let Some(first_turn) = file.turns.first() {
            if let Some(text) = &first_turn.user_message.text {
                let title: String = text.chars().take(80).collect();
                parts.push(format!("标题: {}", title));

                // 完整内容（截断到 200 字符）
                let truncated = if text.len() > 200 {
                    format!("{}...", &text[..200])
                } else {
                    text.clone()
                };
                parts.push(format!("内容: {}", truncated));
            }
        }

        // 标签（中文显示）
        let tags: Vec<String> = file.tags.iter().map(|t: &Tag| t.to_string()).collect();
        if !tags.is_empty() {
            parts.push(format!("标签: {}", tags.join(", ")));
        }

        parts.join("\n")
    }

    /// 构造评分 Prompt
    fn build_prompt(&self, file: &MemoryFile) -> String {
        let summary = Self::extract_summary_text(file);
        format!(
            r#"你是一个记忆相关性评估器。请评估以下记忆片段与主题的相关性。

主题：{}
{}

请只返回一个 0-100 的整数分数，不需要解释。
100 表示高度相关，0 表示完全无关。"#,
            self.config.topic, summary
        )
    }

    /// 解析 LLM 返回的分数
    fn parse_score(raw: &str) -> Option<f64> {
        // 尝试从纯数字文本中提取
        let trimmed = raw.trim();
        if let Ok(score) = trimmed.parse::<f64>() {
            return Some(score.clamp(0.0, 100.0));
        }

        // 尝试从 "分数：85" 或 "score: 85" 等格式中提取
        for line in trimmed.lines() {
            let line = line.trim();
            for sep in ["：", ":", " "] {
                if let Some(idx) = line.rfind(sep) {
                    let rest = line[idx + sep.len()..].trim();
                    if let Ok(score) = rest.parse::<f64>() {
                        return Some(score.clamp(0.0, 100.0));
                    }
                }
            }
        }

        // 尝试提取第一个数字
        let mut num_str = String::new();
        for c in trimmed.chars() {
            if c.is_ascii_digit() || c == '.' || c == '-' {
                num_str.push(c);
            } else if !num_str.is_empty() {
                break;
            }
        }
        if !num_str.is_empty() {
            if let Ok(score) = num_str.parse::<f64>() {
                return Some(score.clamp(0.0, 100.0));
            }
        }

        None
    }
}

#[async_trait::async_trait]
impl AsyncScorer for HttpLlmScorer {
    async fn score(&self, file: &MemoryFile) -> hippocampus_core::Result<f64> {
        // 若未配置 API URL，降级返回 50
        if self.config.api_url.is_empty() {
            tracing::warn!("LLM 评分器未配置 api_url，降级返回 50 分");
            return Ok(50.0);
        }

        let prompt = self.build_prompt(file);

        let request_body = serde_json::json!({
            "model": self.config.model,
            "messages": [
                {"role": "user", "content": prompt}
            ],
            "max_tokens": self.config.max_tokens,
            "temperature": 0.0,
            // v2.24：关闭思考模式（DeepSeek V4 Flash 默认启用思考模式，
            // 输出会进入 reasoning_content 而 content 为空，导致解析失败）
            "thinking": {"type": "disabled"},
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
            tracing::warn!(status = %status, body = %body, "LLM API 返回错误状态");
            return Ok(50.0); // 降级
        }

        let resp_json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| hippocampus_core::Error::Storage(format!("LLM API 响应解析失败: {}", e)))?;

        // OpenAI 兼容格式：choices[0].message.content
        let content = resp_json
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("");

        match Self::parse_score(content) {
            Some(score) => Ok(score),
            None => {
                tracing::warn!(raw = %content, "无法从 LLM 响应中解析分数，降级返回 50");
                Ok(50.0)
            }
        }
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
                text: Some("讨论 Rust 异步评分器架构".into()),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                thinking: None,
            },
            llm_message: MessageContent {
                text: Some("建议用 async_trait + HybridScorer".into()),
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
    fn test_extract_summary_text() {
        let file = make_memory();
        let text = HttpLlmScorer::extract_summary_text(&file);
        assert!(text.contains("标题:"));
        assert!(text.contains("讨论 Rust 异步评分器架构"));
        assert!(text.contains("标签:"));
        assert!(text.contains("文本消息"));
        assert!(text.contains("代码块"));
    }

    #[test]
    fn test_build_prompt() {
        let config = LlmScorerConfig {
            topic: "Agent 记忆库开发".into(),
            ..Default::default()
        };
        let scorer = HttpLlmScorer::new(config);
        let file = make_memory();
        let prompt = scorer.build_prompt(&file);

        assert!(prompt.contains("Agent 记忆库开发"));
        assert!(prompt.contains("讨论 Rust 异步评分器架构"));
        assert!(prompt.contains("0-100"));
    }

    #[test]
    fn test_parse_score_pure_number() {
        assert_eq!(HttpLlmScorer::parse_score("85"), Some(85.0));
        assert_eq!(HttpLlmScorer::parse_score("  72  "), Some(72.0));
        assert_eq!(HttpLlmScorer::parse_score("0"), Some(0.0));
        assert_eq!(HttpLlmScorer::parse_score("100"), Some(100.0));
    }

    #[test]
    fn test_parse_score_with_label() {
        assert_eq!(HttpLlmScorer::parse_score("分数：85"), Some(85.0));
        assert_eq!(HttpLlmScorer::parse_score("score: 72"), Some(72.0));
        assert_eq!(HttpLlmScorer::parse_score("Score 90"), Some(90.0));
    }

    #[test]
    fn test_parse_score_clamp() {
        assert_eq!(HttpLlmScorer::parse_score("150"), Some(100.0));
        assert_eq!(HttpLlmScorer::parse_score("-10"), Some(0.0));
    }

    #[test]
    fn test_parse_score_invalid() {
        assert_eq!(HttpLlmScorer::parse_score("无法解析"), None);
        assert_eq!(HttpLlmScorer::parse_score(""), None);
    }

    #[tokio::test]
    async fn test_score_without_api_url_returns_50() {
        let config = LlmScorerConfig::default(); // api_url 为空
        let scorer = HttpLlmScorer::new(config);
        let file = make_memory();
        let score = scorer.score(&file).await.unwrap();
        assert_eq!(score, 50.0);
    }
}
