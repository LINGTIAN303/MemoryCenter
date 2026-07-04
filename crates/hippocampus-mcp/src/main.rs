//! # Hippocampus MCP Server (stdio)
//!
//! stdio 传输模式的 MCP server 入口。
//! 被 Claude Code / Cursor / Trae 等 MCP 客户端作为子进程拉起。
//!
//! ## 环境变量
//!
//! - `HIPPOCAMPUS_ROOT`：存储根目录（默认 `./data`）
//! - `RUST_LOG`：日志级别（默认 `info`）
//!
//! ## 冲突检测器配置（v2.11，v2.13 默认值更新）
//!
//! | 环境变量 | 说明 | 默认值 |
//! |---------|------|--------|
//! | `HIPPOCAMPUS_DETECTOR_API_URL` | LLM API 地址（OpenAI 兼容 `/v1/chat/completions`） | 空（降级为 HeuristicDetector） |
//! | `HIPPOCAMPUS_DETECTOR_API_KEY` | API Key | 空 |
//! | `HIPPOCAMPUS_DETECTOR_MODEL` | 模型名 | `gpt-5.5-instant` |
//! | `HIPPOCAMPUS_DETECTOR_TIMEOUT` | 超时秒数 | `30` |
//! | `HIPPOCAMPUS_DETECTOR_MAX_TOKENS` | LLM 最大输出 token | `500` |
//!
//! 未配置 `API_URL` 时：注入 `HeuristicDetector`（启发式纯算法，三维度检测）。
//! 配置完整时：注入 `HybridDetector`（串联 Heuristic + LLM，合并两份报告）。
//!
//! ## 语义检索配置（v2.18 新增）
//!
//! 通过环境变量配置 Embedder API 后，`semantic_search` 工具可用：
//!
//! | 环境变量 | 说明 | 默认值 |
//! |---------|------|--------|
//! | `HIPPOCAMPUS_EMBEDDER_API_URL` | Embedding API 地址（OpenAI 兼容 `/v1/embeddings`） | 空（降级为仅关键词） |
//! | `HIPPOCAMPUS_EMBEDDER_API_KEY` | API Key | 空 |
//! | `HIPPOCAMPUS_EMBEDDER_MODEL` | 模型名 | `text-embedding-3-large` |
//! | `HIPPOCAMPUS_EMBEDDER_DIM` | 向量维度 | `3072` |
//! | `HIPPOCAMPUS_EMBEDDER_TIMEOUT` | 超时秒数 | `30` |
//!
//! - 未配置 `API_URL`：降级为 `KeywordOnlyRetriever`（仅 BM25 关键词检索）
//! - 配置完整：每 session 独立 `HybridRetriever`（关键词 + 向量 + RRF 融合）
//! - **MCP 进程重启后索引丢失**：SessionSearchRouter 注入了 storage 引用，
//!   首次访问 session 时自动从 storage 批量重建索引（用 `embed_batch` 优化 API 调用）
//!
//! ## 摘要生成器配置（v2.21 批次 8c）
//!
//! 通过环境变量配置 LLM API 后，`archive` 工具归档时自动生成结构化摘要填入 IndexHook：
//!
//! | 环境变量 | 说明 | 默认值 |
//! |---------|------|--------|
//! | `HIPPOCAMPUS_GENERATOR_API_URL` | LLM API 地址（OpenAI 兼容 `/v1/chat/completions`） | 空（降级为启发式） |
//! | `HIPPOCAMPUS_GENERATOR_API_KEY` | API Key | 空 |
//! | `HIPPOCAMPUS_GENERATOR_MODEL` | 模型名 | `gpt-5.5-instant` |
//! | `HIPPOCAMPUS_GENERATOR_TIMEOUT` | 超时秒数 | `60` |
//! | `HIPPOCAMPUS_GENERATOR_MAX_TOKENS` | LLM 最大输出 token | `500` |
//!
//! - 未配置 `API_URL`：使用启发式 `Summary::from_title`（首条消息前 80 字符）
//! - 配置完整：使用 `HttpSummaryGenerator`（LLM 生成 title + abstract + key_facts + key_entities）
//! - LLM 调用失败：降级为启发式，归档主流程不中断

use std::path::{Path, PathBuf};
use std::sync::Arc;

use hippocampus_core::conflict::{ConflictDetector, HybridDetector};
use hippocampus_core::heuristic::HeuristicDetector;
use hippocampus_core::storage::{LocalStorage, Storage};
use hippocampus_mcp::HippocampusMcp;
use hippocampus_llm::{HttpLlmDetector, LlmDetectorConfig};
use rmcp::ServiceExt;
use rmcp::transport::stdio;

/// 从环境变量构造冲突检测器（v2.11，v2.13 简化）
///
/// - 配置了 `HIPPOCAMPUS_DETECTOR_API_URL` + `API_KEY`：
///   返回 `HybridDetector`（串联 Heuristic + LLM，合并两份报告）
/// - 未配置：返回 `HeuristicDetector`（启发式纯算法，无 LLM 依赖）
fn build_conflict_detector() -> Arc<dyn ConflictDetector> {
    // v2.13：使用 LlmDetectorConfig::from_env() 统一环境变量读取
    let config = match LlmDetectorConfig::from_env() {
        Some(config) => config,
        None => {
            tracing::info!(
                "冲突检测器：未配置 LLM API，使用 HeuristicDetector（启发式纯算法，三维度检测）"
            );
            return Arc::new(HeuristicDetector::new());
        }
    };

    tracing::info!(
        api_url = %config.api_url,
        model = %config.model,
        max_tokens = config.max_tokens,
        "冲突检测器：LLM API 已配置，使用 HybridDetector（串联 Heuristic + LLM，失败时降级保留启发式结果）"
    );

    // v2.11：串联 Heuristic + LLM，合并两份报告
    let heuristic: Arc<dyn ConflictDetector> = Arc::new(HeuristicDetector::new());
    let llm: Arc<dyn ConflictDetector> = Arc::new(HttpLlmDetector::new(config));
    Arc::new(HybridDetector::new(heuristic, llm))
}

/// 从环境变量构造 SessionSearchRouter（v2.18 新增）
///
/// 与 server 端 `build_session_search` 的关键差异：
/// **MCP 端必须注入 storage 引用**（用 `with_storage`），因为 MCP 进程是
/// 短生命周期子进程，每次启动后内存索引为空，必须从 storage 懒重建。
///
/// - 配置了 `HIPPOCAMPUS_EMBEDDER_API_URL` + `API_KEY`：
///   返回注入了 HttpEmbedder 的 SessionSearchRouter（混合检索）
/// - 未配置：返回仅关键词模式的 SessionSearchRouter（降级，但仍带 storage 懒重建）
fn build_session_search(storage_root: &Path) -> Option<Arc<hippocampus_search::SessionSearchRouter>> {
    use hippocampus_core::semantic::Embedder;
    use hippocampus_llm::{EmbedderConfig, HttpEmbedder};
    use hippocampus_search::SessionSearchRouter;

    // 构造 storage 后端（供 SessionSearchRouter 重建索引用）
    let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(storage_root.to_path_buf()));

    // v2.13：使用 EmbedderConfig::from_env() 统一环境变量读取
    let embedder_config = match EmbedderConfig::from_env() {
        Some(config) => config,
        None => {
            // 降级模式：仅关键词检索 + storage 懒重建
            tracing::info!(
                "语义检索：未配置 Embedder API，降级为仅关键词检索（KeywordOnlyRetriever，session 级隔离 + storage 懒重建）"
            );
            let router = SessionSearchRouter::new(None, 0).with_storage(storage);
            return Some(Arc::new(router));
        }
    };

    let dim = embedder_config.dim;
    tracing::info!(
        api_url = %embedder_config.api_url,
        model = %embedder_config.model,
        dim,
        "语义检索：Embedder 已配置，启用 session 级混合检索（HybridRetriever + storage 懒重建 + embed_batch 批量优化）"
    );

    let embedder: Arc<dyn Embedder> = Arc::new(HttpEmbedder::new(embedder_config));
    let router = SessionSearchRouter::new(Some(embedder), dim).with_storage(storage);
    Some(Arc::new(router))
}

/// 从环境变量构造 LLM 摘要生成器（v2.21 批次 8c）
///
/// 与 server 端 `build_summary_generator` 行为一致：
///
/// | 环境变量 | 说明 | 默认值 |
/// |---------|------|--------|
/// | `HIPPOCAMPUS_GENERATOR_API_URL` | LLM API 地址（OpenAI 兼容 `/v1/chat/completions`） | 空（降级为启发式） |
/// | `HIPPOCAMPUS_GENERATOR_API_KEY` | API Key | 空 |
/// | `HIPPOCAMPUS_GENERATOR_MODEL` | 模型名 | `gpt-5.5-instant` |
/// | `HIPPOCAMPUS_GENERATOR_TIMEOUT` | 超时秒数 | `60` |
/// | `HIPPOCAMPUS_GENERATOR_MAX_TOKENS` | LLM 最大输出 token | `500` |
///
/// - 未配置 `API_URL`：返回 None（使用启发式 `Summary::from_title`）
/// - 配置完整：返回 `HttpSummaryGenerator`（归档时生成结构化摘要）
fn build_summary_generator() -> Option<Arc<dyn hippocampus_core::generate::SummaryGenerator>> {
    use hippocampus_core::generate::LlmGeneratorConfig;
    use hippocampus_llm::HttpSummaryGenerator;

    let config = match LlmGeneratorConfig::from_env() {
        Some(config) => config,
        None => {
            tracing::info!(
                "摘要生成器：未配置 LLM API（HIPPOCAMPUS_GENERATOR_API_URL），使用启发式 Summary::from_title"
            );
            return None;
        }
    };

    tracing::info!(
        api_url = %config.api_url,
        model = %config.model,
        max_tokens = config.max_tokens,
        "摘要生成器：LLM API 已配置，启用 HttpSummaryGenerator（归档时生成结构化摘要）"
    );

    Some(Arc::new(HttpSummaryGenerator::new(config)))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hippocampus_mcp=info".into()),
        )
        .init();

    // 读取存储根目录配置
    let storage_root = PathBuf::from(
        std::env::var("HIPPOCAMPUS_ROOT").unwrap_or_else(|_| "./data".to_string()),
    );

    // v2.11：构造冲突检测器并注入
    let conflict_detector = build_conflict_detector();

    // v2.18：构造 SessionSearchRouter（注入 storage 支持懒重建）
    let session_search = build_session_search(&storage_root);

    // v2.21 批次 8c：构造 LLM 摘要生成器（环境变量驱动：未配置时返回 None，使用启发式）
    let summary_generator = build_summary_generator();

    tracing::info!(
        root = %storage_root.display(),
        "启动 Hippocampus MCP server (stdio 传输)"
    );

    // 启动 stdio MCP server
    let service = HippocampusMcp::with_conflict_detector(
        storage_root,
        Some(conflict_detector),
    )
    .with_session_search(session_search)
    .with_summary_generator(summary_generator)
    .serve(stdio())
    .await?;

    service.waiting().await?;

    Ok(())
}
