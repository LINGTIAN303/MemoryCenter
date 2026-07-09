//! # MemoryCenter 归档调用（v2.36 新增，v2.43 改为结构化 turns）
//!
//! 封装 MemoryCenter HTTP pre-compress 端点调用。
//!
//! ## 端点
//!
//! `POST /api/v1/sessions/{sid}/pre-compress`
//!
//! ## 请求体（v2.43 起支持结构化 turns）
//!
//! ```json
//! {
//!   "turns": [{"user_message": {"text": "..."}, "llm_message": {"text": "...", "thinking": "...", "tool_calls": [...]}}],
//!   "estimated_tokens": 12345,
//!   "project_id": "opencode"
//! }
//! ```
//!
//! turns 结构与服务器 MessageTurn 兼容（服务器 `#[serde(default)]` 补全缺失字段）。
//! 服务端调 apply_turn_defaults 自动推断 tags + 估算 token_count。
//!
//! ## 响应
//!
//! 返回 token 反馈循环信息（threshold / ratio / suggestion）。

use crate::config::SidecarConfig;
use serde::{Deserialize, Serialize};

/// sidecar 本地的轮次结构（v2.43 新增）
///
/// 与服务器 `MessageTurn` JSON 格式兼容，但只包含 sidecar 能产出的字段。
/// 服务器反序列化时用 `#[serde(default)]` 补全 id/timestamp/tags/token_count。
#[derive(Serialize, Clone, Debug)]
pub struct SidecarTurn {
    pub user_message: SidecarContent,
    pub llm_message: SidecarContent,
}

/// sidecar 本地的消息内容结构
///
/// 与服务器 `MessageContent` JSON 格式兼容。
/// `attachments` 字段 sidecar 不产生，序列化时省略（服务器默认空 Vec）。
#[derive(Serialize, Clone, Debug)]
pub struct SidecarContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<SidecarToolCall>,
}

/// sidecar 本地的工具调用结构
///
/// 与服务器 `ToolInvocation` JSON 格式兼容。
/// `duration_ms` 字段 sidecar 无法获取，序列化时省略（服务器默认 None）。
#[derive(Serialize, Clone, Debug)]
pub struct SidecarToolCall {
    pub name: String,
    pub arguments: String,
    pub result: String,
}

impl SidecarContent {
    /// 创建仅含文本的内容
    pub fn text_only(text: String) -> Self {
        Self {
            text: if text.is_empty() { None } else { Some(text) },
            thinking: None,
            tool_calls: Vec::new(),
        }
    }
}

/// pre-compress 请求体（v2.43 改为 turns 优先）
#[derive(Serialize)]
pub struct PreCompressRequest {
    /// 结构化轮次列表（v2.43 推荐，保留 tool_calls/thinking）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turns: Option<Vec<SidecarTurn>>,
    /// 完整上下文字符串（向后兼容，turns 优先时省略）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_context: Option<String>,
    /// 估算 token 数
    pub estimated_tokens: Option<usize>,
    /// 项目 ID
    pub project_id: Option<String>,
}

/// pre-compress 响应体
#[derive(Deserialize, Debug)]
pub struct PreCompressResponse {
    pub hook_id: String,
    pub raw_context_path: String,
    pub parse_success: bool,
    pub parsed_turns_count: usize,
    pub archived_tokens: usize,
    pub estimated_total_tokens: usize,
    pub threshold: usize,
    pub threshold_ratio_percent: u64,
    pub suggestion: String,
    pub archived_at: String,
}

/// MemoryCenter HTTP 归档客户端
pub struct ArchiveClient {
    client: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
}

impl ArchiveClient {
    /// 创建新的归档客户端
    pub fn new(config: &SidecarConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            client,
            base_url: config.memorycenter_url.trim_end_matches('/').to_string(),
            api_key: config.memorycenter_api_key.clone(),
        }
    }

    /// 调用 pre-compress 端点归档会话上下文（v2.43 起传结构化 turns）
    ///
    /// - `session_id`: OpenCode session ID（用作 MemoryCenter session ID）
    /// - `turns`: 结构化轮次列表（保留 tool_calls/thinking）
    /// - `estimated_tokens`: 估算的 token 数
    /// - `project_id`: 项目 ID
    pub async fn pre_compress(
        &self,
        session_id: &str,
        turns: Vec<SidecarTurn>,
        estimated_tokens: usize,
        project_id: &str,
    ) -> Result<PreCompressResponse, ArchiveError> {
        // URL path segment encoding（防止 session_id 含特殊字符导致 404）
        let encoded_sid = url_encode_path_segment(session_id);
        let url = format!(
            "{}/api/v1/sessions/{}/pre-compress",
            self.base_url, encoded_sid
        );

        let req_body = PreCompressRequest {
            turns: Some(turns),
            full_context: None,
            estimated_tokens: Some(estimated_tokens),
            project_id: Some(project_id.to_string()),
        };

        let mut req = self.client.post(&url).json(&req_body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }

        let resp = req.send().await.map_err(ArchiveError::Request)?;
        let status = resp.status();

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ArchiveError::HttpStatus(status, body));
        }

        let resp_body: PreCompressResponse = resp.json().await.map_err(ArchiveError::Parse)?;
        Ok(resp_body)
    }

    /// 健康检查（ping MemoryCenter 服务）
    ///
    /// 使用 `/api/v1/presets/agents` 端点（GET，无状态）确认服务在线。
    /// 带 API key 避免被 401 鉴权拦截误判为"不可达"。
    pub async fn health_check(&self) -> Result<bool, ArchiveError> {
        let url = format!("{}/api/v1/presets/agents", self.base_url);
        let mut req = self.client.get(&url);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().await;
        match resp {
            Ok(r) => Ok(r.status().is_success()),
            Err(_) => Ok(false),
        }
    }
}

/// 归档错误
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("HTTP 请求失败: {0}")]
    Request(#[from] reqwest::Error),
    #[error("HTTP 状态错误: {0} - {1}")]
    HttpStatus(reqwest::StatusCode, String),
    #[error("响应解析失败: {0}")]
    Parse(reqwest::Error),
}

/// URL path segment 编码（RFC 3986 unreserved 字符保持原样，其余 percent-encode）
///
/// 避免引入新依赖，内联实现。OpenCode session ID 通常格式为 `ses_01JXXXXXXXX`，
/// 均为安全字符，但此函数确保任何特殊字符都能正确编码。
fn url_encode_path_segment(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '~' {
            result.push(c);
        } else {
            // 非 ASCII 或保留字符：按 UTF-8 字节 percent-encode
            let mut buf = [0u8; 4];
            let bytes = c.encode_utf8(&mut buf).as_bytes();
            for &b in bytes {
                result.push_str(&format!("%{:02X}", b));
            }
        }
    }
    result
}
