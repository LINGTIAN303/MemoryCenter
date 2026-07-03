//! # Hippocampus HTTP 服务库
//!
//! 将 [`hippocampus_core`] 的核心能力暴露为 REST API，
//! 供所有语言（Python/Node/Go/Java 等）通过 HTTP 调用。
//!
//! ## 架构
//!
//! - **无状态设计**：每次请求从磁盘读取，操作完释放
//! - **Storage 共享**：`AppState` 持有存储根目录，每次请求创建 `LocalStorage`
//! - **Archiver 一次性模式**：客户端一次性传入 turns，服务端 push 后归档
//!
//! ## API 端点
//!
//! | 方法 | 路径 | 作用 |
//! |------|------|------|
//! | POST | `/api/v1/sessions/{sid}/archive` | 归档 turns |
//! | GET  | `/api/v1/sessions/{sid}/memories/{hook_id}` | 检索记忆 |
//! | GET  | `/api/v1/sessions/{sid}/summaries` | 摘要视图 |
//! | GET  | `/api/v1/sessions/{sid}/prompt` | 渲染 system prompt |
//! | POST | `/api/v1/sessions/{sid}/compaction` | 周期任务 |

mod error;
/// v2.5 批次 7: HTTP Embedder 实现
pub mod embedding;
mod handlers;
/// v2.4: LLM 评分器实现（HttpLlmScorer）
pub mod llm;
/// v2.5 批次 7: 搜索索引器（归档后自动索引到 BM25 + 向量索引）
pub mod search;
/// v2.8: Session 级索引隔离路由器
pub mod session_search;

pub use embedding::{EmbedderConfig, HttpEmbedder};
pub use error::AppError;
pub use llm::HttpLlmScorer;
pub use search::SearchIndexer;
pub use session_search::SessionSearchRouter;

use std::path::PathBuf;

/// 应用配置（从环境变量读取）
#[derive(Debug, Clone)]
pub struct Config {
    /// 监听地址
    pub host: String,
    /// 监听端口
    pub port: u16,
    /// 存储根目录
    pub storage_root: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: std::env::var("HIPPOCAMPUS_HOST").unwrap_or_else(|_| "127.0.0.1".to_string()),
            port: std::env::var("HIPPOCAMPUS_PORT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(8765),
            storage_root: PathBuf::from(
                std::env::var("HIPPOCAMPUS_ROOT").unwrap_or_else(|_| "./data".to_string()),
            ),
        }
    }
}

/// 应用共享状态（通过 Axum State 提取器注入）
#[derive(Clone)]
pub struct AppState {
    /// 存储根目录（每次请求创建 LocalStorage 时使用）
    pub storage_root: PathBuf,
    /// 可选的语义检索器（未配置时 /search 返回 501 Not Implemented）
    ///
    /// v2.5 批次 7：全局单例，所有 session 共享索引（已废弃，保留向后兼容）
    /// v2.8：优先使用 `session_search`，未配置时降级到此字段
    #[deprecated(note = "v2.8 起优先使用 session_search 字段实现 session 隔离")]
    pub retriever: Option<std::sync::Arc<dyn hippocampus_core::semantic::SemanticRetriever>>,
    /// 可选的搜索索引器（归档后自动索引到 BM25 + 向量索引）
    ///
    /// v2.5 批次 7：全局单例（已废弃，保留向后兼容）
    /// v2.8：优先使用 `session_search`，未配置时降级到此字段
    #[deprecated(note = "v2.8 起优先使用 session_search 字段实现 session 隔离")]
    pub search_indexer: Option<std::sync::Arc<SearchIndexer>>,
    /// v2.8：Session 级索引隔离路由器
    ///
    /// 按 session_id 路由到独立的子索引器，实现 session 间完全隔离。
    /// 配置后优先使用此字段；未配置时降级到全局 `retriever` + `search_indexer`。
    pub session_search: Option<std::sync::Arc<SessionSearchRouter>>,
    /// 可选的冲突检测器（未配置时 update_memory 不做冲突检测）
    ///
    /// v2.6 批次 8：在 PATCH /memories/{hook_id} 时同步检测冲突，
    /// 检测结果随 MemoryUpdateRecord 一起持久化。
    pub conflict_detector:
        Option<std::sync::Arc<dyn hippocampus_core::conflict::ConflictDetector>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            storage_root: PathBuf::from("./data"),
            retriever: None,
            search_indexer: None,
            session_search: None,
            conflict_detector: None,
        }
    }
}

/// 创建路由
pub fn create_router(state: AppState) -> axum::Router {
    use axum::routing::{get, post};

    axum::Router::new()
        // 5 个核心端点
        .route(
            "/api/v1/sessions/{sid}/archive",
            post(handlers::archive),
        )
        .route(
            "/api/v1/sessions/{sid}/memories/{hook_id}",
            get(handlers::retrieve).patch(handlers::update_memory),
        )
        .route(
            "/api/v1/sessions/{sid}/summaries",
            get(handlers::get_summaries),
        )
        .route(
            "/api/v1/sessions/{sid}/prompt",
            get(handlers::render_prompt),
        )
        .route(
            "/api/v1/sessions/{sid}/compaction",
            post(handlers::run_compaction),
        )
        // v2.5 批次 6：批量操作端点
        .route(
            "/api/v1/sessions/{sid}/memories/batch-retrieve",
            post(handlers::batch_retrieve),
        )
        .route(
            "/api/v1/sessions/{sid}/memories/batch-delete",
            post(handlers::batch_delete),
        )
        .route(
            "/api/v1/sessions/{sid}/memories/batch-update",
            post(handlers::batch_update),
        )
        // v2.5 批次 7：语义检索端点
        .route(
            "/api/v1/sessions/{sid}/search",
            post(handlers::search),
        )
        // v2.6 批次 8：冲突查询端点（GET 单条记忆的所有冲突记录）
        .route(
            "/api/v1/sessions/{sid}/memories/{hook_id}/conflicts",
            get(handlers::get_conflicts),
        )
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = Config::default();
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.port, 8765);
        assert_eq!(config.storage_root, PathBuf::from("./data"));
    }
}
