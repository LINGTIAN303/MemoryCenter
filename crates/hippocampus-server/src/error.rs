//! # 错误处理
//!
//! 统一的 HTTP 错误响应格式。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

/// 应用错误类型
#[derive(Debug)]
pub enum AppError {
    /// 请求参数错误（400）
    BadRequest(String),
    /// 资源未找到（404）
    NotFound(String),
    /// 功能未实现（501）
    NotImplemented(String),
    /// 内部错误（500）
    Internal(String),
}

/// 错误响应体
#[derive(Serialize)]
struct ErrorResponse {
    error: ErrorDetail,
}

#[derive(Serialize)]
struct ErrorDetail {
    code: String,
    message: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, "BAD_REQUEST", msg),
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, "NOT_FOUND", msg),
            AppError::NotImplemented(msg) => {
                (StatusCode::NOT_IMPLEMENTED, "NOT_IMPLEMENTED", msg)
            }
            AppError::Internal(msg) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR", msg)
            }
        };
        let body = Json(ErrorResponse {
            error: ErrorDetail {
                code: code.to_string(),
                message,
            },
        });
        (status, body).into_response()
    }
}

/// 从 Core 错误转换
impl From<hippocampus_core::Error> for AppError {
    fn from(e: hippocampus_core::Error) -> Self {
        match e {
            // 索引错误中含「未找到」「不存在」「已删除」视为 404
            // （如 hook_id 精确+前缀匹配均无结果、记忆文件已软删除）
            hippocampus_core::Error::Index(msg)
                if msg.contains("未找到") || msg.contains("不存在") || msg.contains("已删除") =>
            {
                AppError::NotFound(msg)
            }
            // 存储错误中含「不存在」或「读取记忆文件失败」视为 404
            // （如 memory 已删除但 hook 仍在 index 中，read_memory 找不到文件）
            hippocampus_core::Error::Storage(msg)
                if msg.contains("不存在") || msg.contains("读取记忆文件失败") =>
            {
                AppError::NotFound(msg)
            }
            // 序列化错误视为 400（客户端传了非法 JSON）
            hippocampus_core::Error::Serialize(msg) => AppError::BadRequest(msg),
            // 其余视为内部错误
            other => AppError::Internal(other.to_string()),
        }
    }
}

/// 从 serde JSON 错误转换
impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::BadRequest(format!("JSON 解析失败: {}", e))
    }
}
