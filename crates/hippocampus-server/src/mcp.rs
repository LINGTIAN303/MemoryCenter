//! # MCP Streamable HTTP 传输支持（v2.36）
//!
//! 将 MCP server 合并到 HTTP Server，通过 `/mcp` 端点提供 Streamable HTTP 传输。
//!
//! ## 设计
//!
//! - **合并到 HTTP Server**：与 REST API 共享同一个 Axum 服务，无需独立进程
//! - **复用 bootstrap**：调用 `hippocampus_mcp::bootstrap` 的 `build_*` 函数，
//!   与 stdio 模式保持一致的组件构造逻辑
//! - **Session 模式**：环境变量驱动（`HIPPOCAMPUS_MCP_STATEFUL`，默认 true）
//!   - true：`LocalSessionManager`（有状态，支持 session 管理 + SSE 流）
//!   - false：`NeverSessionManager`（无状态，每次请求独立，JSON 响应）
//! - **安全防护**：allowed_hosts（DNS rebinding）+ allowed_origins（CORS）
//!
//! ## 配置项
//!
//! | 环境变量 | 说明 | 默认值 |
//! |---------|------|--------|
//! | `HIPPOCAMPUS_MCP_ENABLED` | 是否启用 MCP Streamable HTTP 端点 | `false`（需显式启用） |
//! | `HIPPOCAMPUS_MCP_STATEFUL` | 是否启用 session 模式 | `true` |
//! | `HIPPOCAMPUS_MCP_ALLOWED_HOSTS` | 允许的 Host 列表（逗号分隔） | `localhost,127.0.0.1,::1` |
//! | `HIPPOCAMPUS_MCP_ALLOWED_ORIGINS` | 允许的 Origin 列表（逗号分隔） | 空（不校验 Origin） |
//!
//! ## 路由挂载
//!
//! `/mcp` 路由在 `main.rs` 中通过 `mount_mcp_route()` 追加到 Axum Router，
//! 不经过 REST API 的 `require_api_key` 鉴权中间件（MCP 客户端使用 MCP 协议自身认证）。
//! DNS rebinding 和 CORS 防护由 `StreamableHttpServerConfig` 内部处理。
//!
//! ## Agent 客户端识别（v2.36 限制说明）
//!
//! `service_factory` 闭包在每个 session 创建时调用 `build_combined_profile()`，
//! 内部调用 `detect_agent_client(None)` 进行 4 层信号融合识别。
//!
//! **HTTP 模式下的限制**：rmcp 的 `service_factory` 签名为 `Fn() -> Result<T>`，
//! 不接受请求参数，因此无法在 session 创建时获取 MCP `ClientInfo`（Layer 2 失效）。
//! per-session 识别实际依赖：
//!
//! - **Layer 1**（推荐）：`HIPPOCAMPUS_PRESET_AGENT` 环境变量（服务器启动时设置）
//! - **Layer 3**：父进程名 / 环境变量前缀（HTTP 模式下父进程是 systemd/手动启动）
//! - **Layer 4**：降级 Custom（不注入 preset）
//!
//! **生产环境推荐**：在 systemd unit 或部署脚本中设置 `HIPPOCAMPUS_PRESET_AGENT=trae`
//! （或 `claude_code` / `cursor` / `codex`），所有连接的 session 统一识别为该 family。
//! 这符合服务端持久化部署的常见模式。
//!
//! 未来若需真正的 per-session ClientInfo 识别，需在 `HippocampusMcp` 内部拦截
//! `initialize` 请求动态更新 `combined_profile`（架构调整较大，留作 v2.37+ 候选）。

use std::path::PathBuf;
use std::sync::Arc;

use hippocampus_mcp::HippocampusMcp;
use hippocampus_mcp::bootstrap::{
    build_combined_profile, build_conflict_detector, build_scenario_detector,
    build_session_search, build_summary_generator,
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService,
    session::local::LocalSessionManager,
    session::never::NeverSessionManager,
};

/// MCP Streamable HTTP 配置（环境变量驱动）
#[derive(Clone, Debug)]
pub struct McpConfig {
    /// 是否启用 session 模式
    /// - true：LocalSessionManager（有状态，支持 session 管理 + SSE 流）
    /// - false：NeverSessionManager（无状态，每次请求独立，JSON 响应）
    pub stateful_mode: bool,
    /// 允许的 Host 列表（DNS rebinding 防护）
    /// 默认仅允许 loopback，生产环境需配置实际域名
    pub allowed_hosts: Vec<String>,
    /// 允许的 Origin 列表（CORS 防护）
    /// 空列表表示不校验 Origin
    pub allowed_origins: Vec<String>,
    /// 存储根目录（传递给 HippocampusMcp）
    pub storage_root: PathBuf,
}

impl McpConfig {
    /// 从环境变量读取配置
    ///
    /// 读取的环境变量：
    /// - `HIPPOCAMPUS_MCP_STATEFUL`：bool，默认 `true`
    /// - `HIPPOCAMPUS_MCP_ALLOWED_HOSTS`：逗号分隔，默认 `localhost,127.0.0.1,::1`
    /// - `HIPPOCAMPUS_MCP_ALLOWED_ORIGINS`：逗号分隔，默认空
    pub fn from_env(storage_root: PathBuf) -> Self {
        let stateful_mode = std::env::var("HIPPOCAMPUS_MCP_STATEFUL")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(true);

        let allowed_hosts = std::env::var("HIPPOCAMPUS_MCP_ALLOWED_HOSTS")
            .ok()
            .map(|s| {
                s.split(',')
                    .map(|h| h.trim().to_string())
                    .filter(|h| !h.is_empty())
                    .collect()
            })
            .unwrap_or_else(|| {
                vec![
                    "localhost".into(),
                    "127.0.0.1".into(),
                    "::1".into(),
                ]
            });

        let allowed_origins = std::env::var("HIPPOCAMPUS_MCP_ALLOWED_ORIGINS")
            .ok()
            .map(|s| {
                s.split(',')
                    .map(|o| o.trim().to_string())
                    .filter(|o| !o.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        Self {
            stateful_mode,
            allowed_hosts,
            allowed_origins,
            storage_root,
        }
    }
}

/// 构造 service_factory 闭包
///
/// 每次创建新 session 时调用，构造一个完整的 `HippocampusMcp` 实例。
/// 复用 `bootstrap` 模块的 `build_*` 函数，与 stdio 模式保持一致。
///
/// ## 组件构造
///
/// 1. `build_conflict_detector()`：冲突检测器（LLM 或启发式降级）
/// 2. `build_session_search()`：语义检索路由器（注入 storage 支持懒重建）
/// 3. `build_summary_generator()`：摘要生成器（LLM 或启发式降级）
/// 4. `build_scenario_detector()`：场景识别器（关键词或 LLM 兜底）
/// 5. `build_combined_profile()`：Agent 客户端识别 + 行为契约注入
///
/// 所有组件都有降级策略，未配置 LLM API 时不会阻塞构造。
fn make_service_factory(
    storage_root: PathBuf,
) -> impl Fn() -> Result<HippocampusMcp, std::io::Error> + Send + Sync + 'static {
    move || {
        let storage_root = storage_root.clone();

        // 调用 bootstrap 函数构造组件（与 stdio 模式一致）
        let (conflict_detector, detector_status) = build_conflict_detector();
        let (session_search, search_status, embedder_dim) = build_session_search(&storage_root);
        let (summary_generator, generator_status) = build_summary_generator();
        let scenario_detector = build_scenario_detector();
        let combined_profile = build_combined_profile();

        let runtime_status = hippocampus_mcp::RuntimeStatus {
            conflict_detector: detector_status,
            semantic_search: search_status,
            summary_generator: generator_status,
            embedder_dim,
        };

        tracing::info!(
            has_combined_profile = combined_profile.is_some(),
            conflict_detector = runtime_status.conflict_detector,
            semantic_search = runtime_status.semantic_search,
            summary_generator = runtime_status.summary_generator,
            "MCP session 创建：构造 HippocampusMcp 实例"
        );

        let mcp = HippocampusMcp::with_conflict_detector(storage_root, Some(conflict_detector))
            .with_session_search(session_search)
            .with_summary_generator(summary_generator)
            .with_scenario_detector(Some(scenario_detector))
            .with_combined_profile(combined_profile)
            .with_runtime_status(runtime_status);

        Ok(mcp)
    }
}

/// 挂载 `/mcp` 路由到 Axum Router
///
/// 根据 `stateful_mode` 选择 SessionManager：
/// - `true`：`LocalSessionManager`（有状态，支持 session 管理 + SSE 流）
/// - `false`：`NeverSessionManager`（无状态，每次请求独立）
///
/// ## 路由特点
///
/// - `/mcp` 端点支持 POST（请求）、GET（SSE 流）、DELETE（关闭 session）
/// - 不经过 REST API 的 `require_api_key` 鉴权中间件
/// - DNS rebinding 和 CORS 防护由 `StreamableHttpServerConfig` 内部处理
///
/// ## 使用方式
///
/// 在 `main.rs` 中，`create_router` 返回的 router 追加 `/mcp` 路由：
///
/// ```rust,ignore
/// let app = create_router(state).layer(TraceLayer::new_for_http());
/// let app = hippocampus_server::mcp::mount_mcp_route(app, &mcp_config);
/// ```
pub fn mount_mcp_route(router: axum::Router, config: &McpConfig) -> axum::Router {
    let server_config = StreamableHttpServerConfig::default()
        .with_stateful_mode(config.stateful_mode)
        .with_allowed_hosts(config.allowed_hosts.clone())
        .with_allowed_origins(config.allowed_origins.clone());

    let service_factory = make_service_factory(config.storage_root.clone());

    if config.stateful_mode {
        tracing::info!(
            allowed_hosts = ?config.allowed_hosts,
            allowed_origins = ?config.allowed_origins,
            "MCP Streamable HTTP：启用 session 模式（LocalSessionManager）"
        );
        let session_manager = Arc::new(LocalSessionManager::default());
        let service = StreamableHttpService::new(service_factory, session_manager, server_config);
        router.route_service("/mcp", service)
    } else {
        tracing::info!(
            allowed_hosts = ?config.allowed_hosts,
            allowed_origins = ?config.allowed_origins,
            "MCP Streamable HTTP：无状态模式（NeverSessionManager，JSON 响应）"
        );
        let session_manager = Arc::new(NeverSessionManager::default());
        let service = StreamableHttpService::new(service_factory, session_manager, server_config);
        router.route_service("/mcp", service)
    }
}
