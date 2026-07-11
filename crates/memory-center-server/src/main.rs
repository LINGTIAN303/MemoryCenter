//! # MemoryCenter HTTP 服务入口
//!
//! 启动 Axum HTTP 服务，将 Core 的能力暴露为 REST API。
//!
//! ## v2.36 MCP Streamable HTTP 传输
//!
//! 通过 `/mcp` 端点提供 MCP Streamable HTTP 传输，支持远程访问和多客户端共享。
//! 与 stdio 模式共享同一套 `build_*` 组件构造逻辑（`memory_center_mcp::bootstrap`）。
//!
//! | 环境变量 | 说明 | 默认值 |
//! |---------|------|--------|
//! | `MEMORY_CENTER_MCP_ENABLED` | 是否启用 MCP Streamable HTTP 端点 | `false`（需显式启用） |
//! | `MEMORY_CENTER_MCP_STATEFUL` | 是否启用 session 模式 | `true` |
//! | `MEMORY_CENTER_MCP_ALLOWED_HOSTS` | 允许的 Host 列表（逗号分隔） | `localhost,127.0.0.1,::1` |
//! | `MEMORY_CENTER_MCP_ALLOWED_ORIGINS` | 允许的 Origin 列表（逗号分隔） | 空（不校验 Origin） |
//!
//! - 未启用：仅 REST API 可用（向后兼容）
//! - 已启用：`/mcp` 端点支持 POST（请求）、GET（SSE 流）、DELETE（关闭 session）
//! - `/mcp` 不经过 REST API 的 `require_api_key` 鉴权（MCP 客户端使用 MCP 协议自身认证）
//!
//! ## 语义检索配置（v2.5 批次 7，v2.13 默认值更新）
//!
//! 通过环境变量配置 Embedder API 后，`/search` 端点和归档后自动索引生效：
//!
//! | 环境变量 | 说明 | 默认值 |
//! |---------|------|--------|
//! | `MEMORY_CENTER_EMBEDDER_API_URL` | Embedding API 地址（OpenAI 兼容 `/v1/embeddings`） | 空（降级为仅关键词） |
//! | `MEMORY_CENTER_EMBEDDER_API_KEY` | API Key | 空 |
//! | `MEMORY_CENTER_EMBEDDER_MODEL` | 模型名 | `text-embedding-3-large` |
//! | `MEMORY_CENTER_EMBEDDER_DIM` | 向量维度 | `3072` |
//! | `MEMORY_CENTER_EMBEDDER_TIMEOUT` | 超时秒数 | `30` |
//!
//! 未配置 `API_URL` 时，自动降级为 `KeywordOnlyRetriever`（仅 BM25 关键词检索）。
//!
//! ## 冲突检测配置（v2.10，v2.13 默认值更新，v2.14 升级 HybridDetector）
//!
//! | 环境变量 | 说明 | 默认值 |
//! |---------|------|--------|
//! | `MEMORY_CENTER_DETECTOR_API_URL` | LLM API 地址（OpenAI 兼容 `/v1/chat/completions`） | 空（降级为 HeuristicDetector） |
//! | `MEMORY_CENTER_DETECTOR_API_KEY` | API Key | 空 |
//! | `MEMORY_CENTER_DETECTOR_MODEL` | 模型名 | `gpt-5.5-instant` |
//! | `MEMORY_CENTER_DETECTOR_TIMEOUT` | 超时秒数 | `30` |
//! | `MEMORY_CENTER_DETECTOR_MAX_TOKENS` | LLM 最大输出 token | `500` |
//!
//! - 未配置 `API_URL`：使用 `HeuristicDetector`（启发式纯算法，三维度检测）
//! - 配置完整：使用 `HybridDetector`（串联 Heuristic + LLM，合并两份报告，v2.14 语义去重默认阈值 0.7）
//!
//! ## 摘要生成器配置（v2.21 批次 8b）
//!
//! 通过环境变量配置 LLM API 后，归档时自动生成结构化摘要填入 IndexHook：
//!
//! | 环境变量 | 说明 | 默认值 |
//! |---------|------|--------|
//! | `MEMORY_CENTER_GENERATOR_API_URL` | LLM API 地址（OpenAI 兼容 `/v1/chat/completions`） | 空（降级为启发式） |
//! | `MEMORY_CENTER_GENERATOR_API_KEY` | API Key | 空 |
//! | `MEMORY_CENTER_GENERATOR_MODEL` | 模型名 | `gpt-5.5-instant` |
//! | `MEMORY_CENTER_GENERATOR_TIMEOUT` | 超时秒数 | `60` |
//! | `MEMORY_CENTER_GENERATOR_MAX_TOKENS` | LLM 最大输出 token | `500` |
//!
//! - 未配置 `API_URL`：使用启发式 `Summary::from_title`（首条消息前 80 字符）
//! - 配置完整：使用 `HttpSummaryGenerator`（LLM 生成 title + abstract + key_facts + key_entities）
//! - LLM 调用失败：降级为启发式，归档主流程不中断
//!
//! ## API Key 鉴权配置（v2.24）
//!
//! | 环境变量 | 说明 | 默认值 |
//! |---------|------|--------|
//! | `MEMORY_CENTER_API_KEY` | API Key（客户端需在 `Authorization: Bearer <key>` 头携带） | 空（不鉴权） |
//!
//! - **未配置**：所有请求无鉴权放行（向后兼容，本地开发零配置）
//! - **已配置**：所有请求必须携带正确的 `Authorization: Bearer <key>` 头
//! - 错误响应：
//!   - 未携带 Authorization 头 → `401 {"error":{"code":"UNAUTHORIZED"}}`
//!   - API Key 不匹配 → `403 {"error":{"code":"FORBIDDEN"}}`
//! - 安全特性：常量时间比对（避免时序侧信道攻击）

// v2.36：复用 memory-center-mcp::bootstrap 的 build_* 函数（与 stdio 模式一致）
use memory_center_mcp::bootstrap::{
    build_conflict_detector, build_scenario_detector, build_session_search,
    build_summary_generator,
};
// v2.36：MCP Streamable HTTP 传输（/mcp 端点）
use memory_center_server::mcp::{mount_mcp_route, McpConfig};
use memory_center_server::{create_router, AppState, Config};
use tower_http::trace::TraceLayer;

#[tokio::main]
async fn main() {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "memory_center_server=info,tower_http=info".into()),
        )
        .init();

    let config = Config::default();

    // 确保存储目录存在
    std::fs::create_dir_all(&config.storage_root).expect("创建存储目录失败");

    // v2.36：复用 memory-center-mcp::bootstrap 的 build_* 函数（与 stdio 模式一致）
    // - build_session_search 注入 storage 支持懒重建（服务重启后 session 索引可重建）
    // - 所有组件未配置 LLM API 时降级为启发式实现
    let (session_search, _, _) = build_session_search(&config.storage_root);
    let (conflict_detector, _) = build_conflict_detector();
    let (summary_generator, _) = build_summary_generator();
    let scenario_detector = build_scenario_detector();

    // v2.50：构建归档引擎（复用 bootstrap 构造的组件，避免重复初始化）
    let mut archive_engine = memory_center_archive_core::ArchiveEngine::new(config.storage_root.clone());
    if let Some(gen) = &summary_generator {
        archive_engine = archive_engine.with_summary_generator(gen.clone());
    }
    archive_engine = archive_engine.with_scenario_detector(scenario_detector.clone());
    if let Some(router) = &session_search {
        archive_engine = archive_engine.with_session_search(router.clone());
    }

    let state = AppState {
        storage_root: config.storage_root.clone(),
        archive_engine: std::sync::Arc::new(archive_engine),
        session_search,
        conflict_detector: Some(conflict_detector),
        summary_generator,
        scenario_detector: Some(scenario_detector),
    };

    let app = create_router(state).layer(TraceLayer::new_for_http());

    // v2.36：MCP Streamable HTTP 端点（环境变量驱动，默认不启用）
    let app = if std::env::var("MEMORY_CENTER_MCP_ENABLED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(false)
    {
        let mcp_config = McpConfig::from_env(config.storage_root.clone());
        tracing::info!(
            stateful = mcp_config.stateful_mode,
            allowed_hosts = ?mcp_config.allowed_hosts,
            allowed_origins = ?mcp_config.allowed_origins,
            "MCP Streamable HTTP 端点已启用：/mcp"
        );
        mount_mcp_route(app, &mcp_config)
    } else {
        tracing::info!("MCP Streamable HTTP 端点未启用（设置 MEMORY_CENTER_MCP_ENABLED=true 启用）");
        app
    };

    let addr = format!("{}:{}", config.host, config.port);
    tracing::info!("MemoryCenter HTTP 服务启动于 http://{}", addr);
    tracing::info!("存储根目录: {:?}", config.storage_root);
    // v2.24：API Key 鉴权状态日志
    if memory_center_server::middleware::auth::configured_api_key().is_some() {
        tracing::info!("API Key 鉴权：已启用（环境变量 MEMORY_CENTER_API_KEY 已配置）");
    } else {
        tracing::warn!(
            "API Key 鉴权：未启用（未配置 MEMORY_CENTER_API_KEY，所有请求将无鉴权放行）"
        );
    }
    tracing::info!("API 端点:");
    tracing::info!("  POST   /api/v1/sessions/{{sid}}/archive");
    tracing::info!("  GET    /api/v1/sessions/{{sid}}/memories/{{hook_id}}");
    tracing::info!("  GET    /api/v1/sessions/{{sid}}/summaries");
    tracing::info!("  GET    /api/v1/sessions/{{sid}}/prompt");
    tracing::info!("  POST   /api/v1/sessions/{{sid}}/compaction");
    tracing::info!("  POST   /api/v1/sessions/{{sid}}/search");
    tracing::info!("  GET    /api/v1/sessions/{{sid}}/memories/{{hook_id}}/conflicts");
    tracing::info!("  POST/GET/DELETE  /mcp  (v2.36 MCP Streamable HTTP，需启用 MEMORY_CENTER_MCP_ENABLED)");

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
