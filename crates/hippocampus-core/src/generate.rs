//! # LLM 生成器 trait（v2.16 IMP-05 / IMP-10 新增）
//!
//! 定义 LLM 驱动的内容生成接口，供 `hippocampus-llm` crate 实现。
//!
//! ## 架构定位
//!
//! - **core**（本模块）：定义 `AnchorGenerator` / `SummaryGenerator` trait + 配置（纯逻辑）
//! - **llm**：HTTP 实现（`HttpAnchorGenerator` / `HttpSummaryGenerator`）
//! - **compact**：`Compactor` 可选注入这两个生成器，在周级/月级合并时生成 richer 摘要
//!
//! ## 设计动机
//!
//! - **IMP-05**：月级合并时 `clue_anchors` 为空（启发式无法生成语义锚点），需 LLM 提取
//! - **IMP-10**：日级/周级 `Summary` 启发式仅生成 title，需 LLM 生成完整结构化摘要
//!
//! ## 降级策略
//!
//! 未注入生成器或 LLM 调用失败时，退化为现有启发式（`Summary::from_title` / 空 anchors），
//! 不影响主流程。

use crate::model::{MemoryFile, Summary};

/// LLM 生成器共享配置
///
/// 用于 [`AnchorGenerator`] 和 [`SummaryGenerator`] 的 HTTP 实现。
/// 复用 OpenAI 兼容 API 格式。
#[derive(Debug, Clone)]
pub struct LlmGeneratorConfig {
    /// LLM API 端点 URL（如 https://api.example.com/v1/chat/completions）
    pub api_url: String,
    /// API Key（Bearer token）
    pub api_key: String,
    /// 模型名称（如 gpt-5.5-instant / deepseek-chat）
    pub model: String,
    /// 请求超时（秒），默认 60
    pub timeout_secs: u64,
    /// 最大 token 数（限制 LLM 输出），默认 500
    pub max_tokens: u32,
}

impl Default for LlmGeneratorConfig {
    fn default() -> Self {
        Self {
            api_url: String::new(),
            api_key: String::new(),
            // v2.13：默认值更新为 gpt-5.5-instant
            model: "gpt-5.5-instant".into(),
            timeout_secs: 60,
            max_tokens: 500,
        }
    }
}

impl LlmGeneratorConfig {
    /// 环境变量前缀
    pub const ENV_PREFIX: &'static str = "HIPPOCAMPUS_GENERATOR";

    /// 从环境变量构造配置（v2.13 模式）
    ///
    /// 读取以下环境变量（前缀 `HIPPOCAMPUS_GENERATOR_`）：
    ///
    /// | 环境变量 | 字段 | 默认值 |
    /// |---------|------|--------|
    /// | `_API_URL` | `api_url` | 必填（缺失返回 None） |
    /// | `_API_KEY` | `api_key` | 必填（缺失返回 None） |
    /// | `_MODEL` | `model` | `gpt-5.5-instant` |
    /// | `_TIMEOUT` | `timeout_secs` | `60` |
    /// | `_MAX_TOKENS` | `max_tokens` | `500` |
    ///
    /// ## 返回
    ///
    /// - `Some(config)`：`api_url` 和 `api_key` 均非空
    /// - `None`：`api_url` 或 `api_key` 为空（调用方应降级为启发式）
    pub fn from_env() -> Option<Self> {
        let api_url = std::env::var(format!("{}_API_URL", Self::ENV_PREFIX)).ok()?;
        let api_key = std::env::var(format!("{}_API_KEY", Self::ENV_PREFIX)).ok()?;
        if api_url.is_empty() || api_key.is_empty() {
            return None;
        }

        Some(Self {
            api_url,
            api_key,
            model: std::env::var(format!("{}_MODEL", Self::ENV_PREFIX))
                .unwrap_or_else(|_| Self::default().model),
            timeout_secs: std::env::var(format!("{}_TIMEOUT", Self::ENV_PREFIX))
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(60),
            max_tokens: std::env::var(format!("{}_MAX_TOKENS", Self::ENV_PREFIX))
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(500),
        })
    }
}

/// 线索锚点生成器 trait（v2.16 IMP-05）
///
/// 用于在月级合并时为记忆文件生成 `clue_anchors`（检索锚点）。
///
/// ## 实现选择
///
/// - **启发式**：直接从 tags/key_entities 提取（无需 LLM）
/// - **LLM**（`HttpAnchorGenerator`）：调用 LLM 从记忆内容提取 3-5 个语义锚点
///
/// ## 调用时机
///
/// 月级合并（`Compactor::monthly_evict`）生成 monthly 钩子时，
/// 若注入了 `AnchorGenerator`，则调用生成 `clue_anchors`。
#[async_trait::async_trait]
pub trait AnchorGenerator: Send + Sync {
    /// 为记忆文件生成线索锚点（3-5 个关键词/短语）
    ///
    /// 返回的锚点用于检索匹配，应具有高辨识度。
    async fn generate_anchors(&self, file: &MemoryFile) -> crate::Result<Vec<String>>;
}

/// 摘要生成器 trait（v2.16 IMP-10）
///
/// 用于在归档/周级合并/月级合并时为记忆文件生成结构化 [`Summary`]。
///
/// ## 实现选择
///
/// - **启发式**：`Summary::from_title`（首条消息前 80 字符作 title，其余字段为空）
/// - **LLM**（`HttpSummaryGenerator`）：生成完整结构化摘要（title + abstract + key_facts + key_entities）
///
/// ## 调用时机
///
/// - 日级归档：`Archiver::archive` 后生成钩子的 summary
/// - 周级合并：`Compactor::weekly_merge` 生成 weekly 钩子的 summary
/// - 月级合并：`Compactor::monthly_evict` 生成 monthly 钩子的 summary
///
/// 注入后所有层级均可使用 LLM 生成；未注入时退化为启发式。
#[async_trait::async_trait]
pub trait SummaryGenerator: Send + Sync {
    /// 为记忆文件生成结构化摘要
    async fn generate_summary(&self, file: &MemoryFile) -> crate::Result<Summary>;
}
