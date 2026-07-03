//! # Hippocampus HTTP 服务入口
//!
//! 启动 Axum HTTP 服务，将 Core 的能力暴露为 REST API。
//!
//! ## 语义检索配置（v2.5 批次 7）
//!
//! 通过环境变量配置 Embedder API 后，`/search` 端点和归档后自动索引生效：
//!
//! | 环境变量 | 说明 | 默认值 |
//! |---------|------|--------|
//! | `HIPPOCAMPUS_EMBEDDER_API_URL` | Embedding API 地址（OpenAI 兼容 `/v1/embeddings`） | 空（降级为仅关键词） |
//! | `HIPPOCAMPUS_EMBEDDER_API_KEY` | API Key | 空 |
//! | `HIPPOCAMPUS_EMBEDDER_MODEL` | 模型名 | `text-embedding-3-small` |
//! | `HIPPOCAMPUS_EMBEDDER_DIM` | 向量维度 | `1536` |
//! | `HIPPOCAMPUS_EMBEDDER_TIMEOUT` | 超时秒数 | `30` |
//!
//! 未配置 `API_URL` 时，自动降级为 `KeywordOnlyRetriever`（仅 BM25 关键词检索）。

use hippocampus_server::{create_router, AppState, Config};
use std::sync::Arc;
use tower_http::trace::TraceLayer;

/// 从环境变量读取 Embedder 配置并构造 SearchIndexer + retriever
///
/// 返回 (search_indexer, retriever)：
/// - 配置完整：HybridRetriever（关键词 + 向量 + RRF 融合）
/// - 未配置或失败：KeywordOnlyRetriever（仅关键词，降级模式）
fn build_search_components() -> (
    Option<Arc<hippocampus_server::SearchIndexer>>,
    Option<Arc<dyn hippocampus_core::semantic::SemanticRetriever>>,
) {
    use hippocampus_core::bm25::Bm25Searcher;
    use hippocampus_core::hybrid::{HybridRetriever, KeywordOnlyRetriever};
    use hippocampus_core::semantic::{Embedder, KeywordSearcher, SemanticRetriever, VectorIndex};
    use hippocampus_core::vector::InMemoryVectorIndex;
    use hippocampus_server::{EmbedderConfig, HttpEmbedder, SearchIndexer};

    // 关键词索引器（必填，降级模式也启用）
    let keyword: Arc<dyn KeywordSearcher> = Arc::new(Bm25Searcher::new());

    // 读取 Embedder 配置
    let api_url = std::env::var("HIPPOCAMPUS_EMBEDDER_API_URL").unwrap_or_default();
    let api_key = std::env::var("HIPPOCAMPUS_EMBEDDER_API_KEY").unwrap_or_default();

    if api_url.is_empty() || api_key.is_empty() {
        // 降级模式：仅关键词检索
        tracing::info!("未配置 Embedder API，降级为仅关键词检索（KeywordOnlyRetriever）");
        let indexer = Arc::new(SearchIndexer::new(keyword.clone(), None, None));
        let retriever: Arc<dyn SemanticRetriever> =
            Arc::new(KeywordOnlyRetriever::new(keyword));
        return (Some(indexer), Some(retriever));
    }

    // 完整模式：构造 HttpEmbedder + InMemoryVectorIndex + HybridRetriever
    let dim: usize = std::env::var("HIPPOCAMPUS_EMBEDDER_DIM")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1536);

    let embedder_config = EmbedderConfig {
        api_url,
        api_key,
        model: std::env::var("HIPPOCAMPUS_EMBEDDER_MODEL")
            .unwrap_or_else(|_| "text-embedding-3-small".to_string()),
        dim,
        timeout_secs: std::env::var("HIPPOCAMPUS_EMBEDDER_TIMEOUT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30),
    };

    tracing::info!(
        api_url = %embedder_config.api_url,
        model = %embedder_config.model,
        dim = embedder_config.dim,
        "Embedder 已配置，启用混合检索（HybridRetriever）"
    );

    let embedder: Arc<dyn Embedder> = Arc::new(HttpEmbedder::new(embedder_config));
    let vector_index: Arc<dyn VectorIndex> = Arc::new(InMemoryVectorIndex::new(dim));

    let indexer = Arc::new(SearchIndexer::new(
        keyword.clone(),
        Some(embedder.clone()),
        Some(vector_index.clone()),
    ));

    let retriever: Arc<dyn SemanticRetriever> =
        Arc::new(HybridRetriever::new(keyword, embedder, vector_index));

    (Some(indexer), Some(retriever))
}

#[tokio::main]
async fn main() {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hippocampus_server=info,tower_http=info".into()),
        )
        .init();

    let config = Config::default();

    // 确保存储目录存在
    std::fs::create_dir_all(&config.storage_root).expect("创建存储目录失败");

    // v2.5 批次 7：构造语义检索组件（SearchIndexer + retriever）
    let (search_indexer, retriever) = build_search_components();

    // v2.6 批次 8：构造冲突检测器（默认 HeuristicDetector）
    let conflict_detector: Option<std::sync::Arc<dyn hippocampus_core::conflict::ConflictDetector>> =
        Some(std::sync::Arc::new(hippocampus_core::heuristic::HeuristicDetector::new()));
    tracing::info!("冲突检测器已启用（HeuristicDetector，三维度检测）");

    let state = AppState {
        storage_root: config.storage_root.clone(),
        retriever,
        search_indexer,
        conflict_detector,
    };

    let app = create_router(state).layer(TraceLayer::new_for_http());

    let addr = format!("{}:{}", config.host, config.port);
    tracing::info!("Hippocampus HTTP 服务启动于 http://{}", addr);
    tracing::info!("存储根目录: {:?}", config.storage_root);
    tracing::info!("API 端点:");
    tracing::info!("  POST   /api/v1/sessions/{{sid}}/archive");
    tracing::info!("  GET    /api/v1/sessions/{{sid}}/memories/{{hook_id}}");
    tracing::info!("  GET    /api/v1/sessions/{{sid}}/summaries");
    tracing::info!("  GET    /api/v1/sessions/{{sid}}/prompt");
    tracing::info!("  POST   /api/v1/sessions/{{sid}}/compaction");
    tracing::info!("  POST   /api/v1/sessions/{{sid}}/search");
    tracing::info!("  GET    /api/v1/sessions/{{sid}}/memories/{{hook_id}}/conflicts");

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
