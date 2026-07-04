//! # API Key 鉴权中间件
//!
//! 从 `Authorization: Bearer <key>` 头提取 API Key 并与 `HIPPOCAMPUS_API_KEY`
//! 环境变量配置的期望值比对。
//!
//! ## 设计
//!
//! - **未配置 `HIPPOCAMPUS_API_KEY` 时跳过鉴权**（向后兼容，本地开发零配置可用）
//! - **配置后**：所有请求必须携带正确的 `Authorization: Bearer <key>` 头
//! - **常量时间比对**：使用 `subtle` 风格的逐字节 XOR 比对，避免时序攻击
//!
//! ## 错误响应
//!
//! - 未携带 Authorization 头 → 401 `{"error":{"code":"UNAUTHORIZED","message":"..."}}`
//! - 格式错误（非 `Bearer ` 前缀） → 401
//! - API Key 不匹配 → 403 `{"error":{"code":"FORBIDDEN","message":"..."}}`

use axum::extract::Request;
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use std::env;

/// 鉴权所需的环境变量名
pub const ENV_API_KEY: &str = "HIPPOCAMPUS_API_KEY";

/// 读取环境变量中配置的 API Key
///
/// - 返回 `None`：未配置，跳过鉴权（向后兼容）
/// - 返回 `Some(key)`：已配置，所有请求必须携带正确的 Bearer token
pub fn configured_api_key() -> Option<String> {
    env::var(ENV_API_KEY).ok().filter(|s| !s.is_empty())
}

/// Axum 中间件：API Key 鉴权
///
/// 使用方式：
/// ```ignore
/// use axum::middleware;
/// let app = create_router(state)
///     .layer(middleware::from_fn(crate::middleware::auth::require_api_key));
/// ```
pub async fn require_api_key(req: Request, next: Next) -> Response {
    // 未配置 API Key 时直接放行（向后兼容，本地开发零配置）
    let expected = match configured_api_key() {
        Some(k) => k,
        None => return next.run(req).await,
    };

    // 提取 Authorization 头
    let auth_header = match req.headers().get(header::AUTHORIZATION) {
        Some(v) => v.to_str().unwrap_or(""),
        None => return unauthorized_response("缺少 Authorization 头"),
    };

    // 校验 Bearer 前缀
    let token = match auth_header.strip_prefix("Bearer ") {
        Some(t) => t,
        None => return unauthorized_response("Authorization 头格式错误，应为 'Bearer <api_key>'"),
    };

    // 常量时间比对（避免时序攻击）
    if !constant_time_eq(token.as_bytes(), expected.as_bytes()) {
        return forbidden_response("API Key 不正确");
    }

    next.run(req).await
}

/// 常量时间字节比对（避免时序侧信道攻击）
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut result: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }
    result == 0
}

// ---------------------------------------------------------------------------
// 错误响应构造
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Serialize)]
struct ErrorDetail {
    code: String,
    message: String,
}

fn unauthorized_response(message: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(ErrorBody {
            error: ErrorDetail {
                code: "UNAUTHORIZED".to_string(),
                message: message.to_string(),
            },
        }),
    )
        .into_response()
}

fn forbidden_response(message: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(ErrorBody {
            error: ErrorDetail {
                code: "FORBIDDEN".to_string(),
                message: message.to_string(),
            },
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constant_time_eq_same() {
        assert!(constant_time_eq(b"abc123", b"abc123"));
    }

    #[test]
    fn test_constant_time_eq_diff() {
        assert!(!constant_time_eq(b"abc123", b"abc124"));
    }

    #[test]
    fn test_constant_time_eq_diff_len() {
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }

    #[test]
    fn test_constant_time_eq_empty() {
        assert!(constant_time_eq(b"", b""));
    }
}
