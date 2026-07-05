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
mod handlers;
pub mod middleware;

// v2.18 批次2：搜索模块下沉到 hippocampus-search crate
// v2.5 批次 7: SearchIndexer（归档后自动索引）
// v2.8: SessionSearchRouter（session 级索引隔离）
// 这里 re-export 保持向后兼容，server 内部代码与外部消费者的 import 路径不变
pub use hippocampus_search::{SearchIndexer, SessionSearchRouter, SessionSearchRouterConfig};

// v2.12: LLM 客户端组件（HttpLlmDetector / HttpEmbedder / HttpLlmScorer）下沉到 hippocampus-llm crate
// v2.21 批次 8b: 新增 HttpSummaryGenerator re-export
// 这里 re-export 保持向后兼容，server 内部代码与外部消费者的 import 路径不变
pub use hippocampus_llm::{
    EmbedderConfig, HttpEmbedder, HttpLlmDetector, HttpLlmScorer, HttpSummaryGenerator,
    LlmDetectorConfig,
};
pub use error::AppError;

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
    /// 可选的 LLM 摘要生成器（v2.21 批次 8b）
    ///
    /// 注入后 archive() 时调用 LLM 生成结构化摘要填入 IndexHook。
    /// 未配置时使用启发式 Summary::from_title（首条消息前 80 字符）。
    pub summary_generator:
        Option<std::sync::Arc<dyn hippocampus_core::generate::SummaryGenerator>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            storage_root: PathBuf::from("./data"),
            retriever: None,
            search_indexer: None,
            session_search: None,
            conflict_detector: None,
            summary_generator: None,
        }
    }
}

/// 创建路由
///
/// v2.24：新增 API Key 鉴权中间件（环境变量 `HIPPOCAMPUS_API_KEY` 驱动）
/// - 未配置：跳过鉴权（向后兼容，本地开发零配置）
/// - 已配置：所有请求必须携带 `Authorization: Bearer <key>` 头
pub fn create_router(state: AppState) -> axum::Router {
    use axum::middleware as axum_mw;
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
        // v2.27：冲突预检测端点（POST，不实际写入，复用 MCP 端 key_facts 注入逻辑）
        .route(
            "/api/v1/sessions/{sid}/memories/{hook_id}/detect-conflicts",
            post(handlers::detect_conflicts),
        )
        // v2.24：API Key 鉴权中间件（对所有路由生效）
        // 顺序：路由定义 → 鉴权中间件 → TraceLayer（在 main.rs 中添加）
        // 注意：axum::middleware 与 crate::middleware 同名，用别名 axum_mw 消歧
        .layer(axum_mw::from_fn(middleware::auth::require_api_key))
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
