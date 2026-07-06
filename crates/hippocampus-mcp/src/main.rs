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
// v2.30：启动时识别 Agent 客户端 + 注入 CombinedProfile（行为契约）
use hippocampus_agents::AgentProfile;
use hippocampus_presets::{
    detect_agent_client, resolve_scenario_name, scenario_from_str, CombinedProfile,
    PresetBuilder,
};
use hippocampus_scenarios::ScenarioProfile;
use rmcp::ServiceExt;
use rmcp::transport::stdio;

/// 从环境变量构造冲突检测器（v2.11，v2.13 简化，v2.32 返回降级状态）
///
/// - 配置了 `HIPPOCAMPUS_DETECTOR_API_URL` + `API_KEY`：
///   返回 `(HybridDetector, "hybrid")`（串联 Heuristic + LLM，合并两份报告）
/// - 未配置：返回 `(HeuristicDetector, "heuristic")`（启发式纯算法，无 LLM 依赖）
///
/// ## 返回
///
/// - `Arc<dyn ConflictDetector>`：检测器实例
/// - `&'static str`：降级状态字符串（`"hybrid"` / `"heuristic"`），供 `RuntimeStatus` 记录
fn build_conflict_detector() -> (Arc<dyn ConflictDetector>, &'static str) {
    // v2.13：使用 LlmDetectorConfig::from_env() 统一环境变量读取
    let config = match LlmDetectorConfig::from_env() {
        Some(config) => config,
        None => {
            tracing::info!(
                "冲突检测器：未配置 LLM API，使用 HeuristicDetector（启发式纯算法，三维度检测）"
            );
            return (Arc::new(HeuristicDetector::new()), "heuristic");
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
    (Arc::new(HybridDetector::new(heuristic, llm)), "hybrid")
}

/// 从环境变量构造 SessionSearchRouter（v2.18 新增，v2.32 返回降级状态）
///
/// 与 server 端 `build_session_search` 的关键差异：
/// **MCP 端必须注入 storage 引用**（用 `with_storage`），因为 MCP 进程是
/// 短生命周期子进程，每次启动后内存索引为空，必须从 storage 懒重建。
///
/// - 配置了 `HIPPOCAMPUS_EMBEDDER_API_URL` + `API_KEY`：
///   返回 `(Some(router), "hybrid", Some(dim))`（混合检索）
/// - 未配置：返回 `(Some(router), "keyword_only", None)`（降级，但仍带 storage 懒重建）
///
/// ## 返回
///
/// - `Option<Arc<SessionSearchRouter>>`：路由器实例（当前总返回 Some）
/// - `&'static str`：降级状态字符串（`"hybrid"` / `"keyword_only"`）
/// - `Option<usize>`：Embedder 向量维度（仅 hybrid 模式有值）
fn build_session_search(
    storage_root: &Path,
) -> (
    Option<Arc<hippocampus_search::SessionSearchRouter>>,
    &'static str,
    Option<usize>,
) {
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
            return (Some(Arc::new(router)), "keyword_only", None);
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
    (Some(Arc::new(router)), "hybrid", Some(dim))
}

/// 从环境变量构造 LLM 摘要生成器（v2.21 批次 8c，v2.32 返回降级状态）
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
/// ## 返回
///
/// - `Option<Arc<dyn SummaryGenerator>>`：生成器实例（None 表示使用启发式）
/// - `&'static str`：降级状态字符串（`"llm"` / `"heuristic"`）
fn build_summary_generator() -> (
    Option<Arc<dyn hippocampus_core::generate::SummaryGenerator>>,
    &'static str,
) {
    use hippocampus_core::generate::LlmGeneratorConfig;
    use hippocampus_llm::HttpSummaryGenerator;

    let config = match LlmGeneratorConfig::from_env() {
        Some(config) => config,
        None => {
            tracing::info!(
                "摘要生成器：未配置 LLM API（HIPPOCAMPUS_GENERATOR_API_URL），使用启发式 Summary::from_title"
            );
            return (None, "heuristic");
        }
    };

    tracing::info!(
        api_url = %config.api_url,
        model = %config.model,
        max_tokens = config.max_tokens,
        "摘要生成器：LLM API 已配置，启用 HttpSummaryGenerator（归档时生成结构化摘要）"
    );

    (Some(Arc::new(HttpSummaryGenerator::new(config))), "llm")
}

/// 从环境变量构造场景识别器（v2.33 新增）
///
/// 复用 `HIPPOCAMPUS_DETECTOR_*` 环境变量（与冲突检测器、摘要生成器共享 LLM 配置）：
///
/// - 配置了 `HIPPOCAMPUS_DETECTOR_API_URL` + `API_KEY`：
///   返回关键词 + LLM 兜底的 HybridScenarioDetector
/// - 未配置：返回仅关键词模式的 HybridScenarioDetector
fn build_scenario_detector() -> Arc<hippocampus_presets::HybridScenarioDetector> {
    use hippocampus_llm::LlmDetectorConfig;
    use hippocampus_presets::scenario_detect::HttpScenarioDetector;

    let llm_config = match LlmDetectorConfig::from_env() {
        Some(config) => {
            tracing::info!(
                api_url = %config.api_url,
                model = %config.model,
                "场景识别器：LLM API 已配置，启用关键词 + LLM 兜底"
            );
            Some(Arc::new(HttpScenarioDetector::new(config)))
        }
        None => {
            tracing::info!(
                "场景识别器：未配置 LLM API，仅用关键词规则识别（7 场景 × 15 关键词）"
            );
            None
        }
    };

    Arc::new(hippocampus_presets::HybridScenarioDetector::new(llm_config))
}

/// 启动时识别 Agent 客户端 + 构建 CombinedProfile（v2.30 新增）
///
/// 执行流程：
/// 1. 调用 `detect_agent_client(None)` 进行 3 层信号融合识别
///    （Layer 1 显式 env > Layer 2 MCP ClientInfo > Layer 3 父进程/env 前缀 > Layer 4 降级）
/// 2. 若识别为 mainstream family（ClaudeCode/Cursor/Trae/Codex）：
///    - 按 family 推导 scenario（HIPPOCAMPUS_PRESET_SCENARIO 优先，否则 coding/daily）
///    - 调用 PresetBuilder 构建 CombinedProfile（含 usage_protocol）
/// 3. 若识别为 Custom/降级：返回 None（tool 行为与 v2.29 一致）
///
/// ## 降级策略
///
/// - PresetBuilder::build 失败：日志警告，返回 None（不阻塞启动）
/// - 识别为 Custom：返回 None（向后兼容）
///
/// ## 注入路径（v2.30 1.5 + 1.8 已完成）
///
/// - `usage_protocol.instructions` → MCP `InitializeResult.instructions` 顶层字段
///   （由 `HippocampusMcp::get_info()` 注入，客户端把它注入 LLM system prompt）
/// - 不写入 SERVER_METADATA.json 文件（避免多 MCP 进程并发写入冲突）
/// - debug 级日志输出完整 instructions，方便调试（`RUST_LOG=debug` 可查看）
///
/// ## 日志输出
///
/// 启动时输出识别结果 + 应用的 preset 摘要：
/// ```text
/// Agent 客户端识别：family=Trae, source=EnvVarPrefix
/// 应用预设：scenario=Coding, archive_threshold=400000, session_prefix=trae
/// 行为契约生成完成：usage_protocol 已注入 MCP InitializeResult.instructions（LLM 启动即看到）
/// ```
fn build_combined_profile() -> Option<CombinedProfile> {
    let detected = detect_agent_client(None);

    tracing::info!(
        family = ?detected.family,
        source = ?detected.source,
        "Agent 客户端识别：3 层信号融合完成"
    );

    // 非 mainstream agent：降级，不构建 CombinedProfile
    if !detected.family.is_mainstream() {
        tracing::info!(
            family = ?detected.family,
            "未识别为主流 Agent（ClaudeCode/Cursor/Trae/Codex），跳过 preset 注入（向后兼容 v2.29）"
        );
        return None;
    }

    // 推导 scenario（环境变量 HIPPOCAMPUS_PRESET_SCENARIO 优先）
    let scenario_str = resolve_scenario_name(&detected.family);
    let scenario = scenario_from_str(&scenario_str);

    tracing::info!(
        family = ?detected.family,
        scenario = %scenario_str,
        "应用预设：按 Agent family 推导 scenario"
    );

    // 构建 CombinedProfile（Agent + Scenario，联动推导 Window）
    let agent_profile = AgentProfile::from_family(detected.family);
    let scenario_profile = ScenarioProfile::from_scenario(scenario);

    match PresetBuilder::new()
        .with_agent(agent_profile)
        .with_scenario(scenario_profile)
        .build()
    {
        Ok(combined) => {
            let protocol = combined.usage_protocol();
            tracing::info!(
                archive_threshold = combined.archive_threshold(),
                session_prefix = ?combined.session_prefix(),
                instructions_len = protocol.instructions.len(),
                trigger_rules_count = protocol.trigger_rules.len(),
                "行为契约生成完成：usage_protocol 已注入 MCP InitializeResult.instructions（LLM 启动即看到）"
            );
            // v2.30 1.8：debug 级输出完整 instructions，方便调试和验证注入内容
            // 生产环境 RUST_LOG=info 不会输出，RUST_LOG=debug 可查看完整行为契约
            tracing::debug!(
                instructions = %protocol.instructions,
                session_id_pattern = %protocol.session_id_pattern,
                trigger_rules = ?protocol
                    .trigger_rules
                    .iter()
                    .map(|r| format!("{}/{}", r.condition, r.tool))
                    .collect::<Vec<_>>(),
                "完整 usage_protocol 内容（debug 级）"
            );
            Some(combined)
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "CombinedProfile 构建失败：跳过 preset 注入（不阻塞启动，tool 行为与 v2.29 一致）"
            );
            None
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // v2.31：CLI 子命令支持
    // 用法：
    //   hippocampus-mcp                           → 启动 MCP server（默认）
    //   hippocampus-mcp install-rules --client <type> --project-root <path> [--force]
    //   hippocampus-mcp --help
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 2 {
        match args[1].as_str() {
            "install-rules" => {
                return run_install_rules_cli(&args[2..]);
            }
            "--help" | "-h" => {
                eprintln!("Hippocampus MCP server v2.31\n");
                eprintln!("用法:");
                eprintln!("  hippocampus-mcp                           启动 MCP server (stdio 传输)");
                eprintln!("  hippocampus-mcp install-rules --client <type> --project-root <path> [--force]");
                eprintln!("                                           安装 Rules 模板到 Agent 客户端");
                eprintln!("\ninstall-rules 参数:");
                eprintln!("  --client <type>          客户端类型: catpaw / trae / claude-code");
                eprintln!("  --project-root <path>    项目根目录的绝对路径");
                eprintln!("  --force                   强制覆盖已存在的文件");
                eprintln!("\n环境变量:");
                eprintln!("  HIPPOCAMPUS_ROOT          存储根目录 (默认 ./data)");
                eprintln!("  RUST_LOG                  日志级别 (默认 info)");
                return Ok(());
            }
            _ => {}
        }
    }

    // 初始化日志
    // v2.30 修复：tracing 必须输出到 stderr（MCP 协议要求 stdout 只能输出 JSON-RPC）
    // tracing_subscriber::fmt() 默认输出到 stdout，会污染 JSON-RPC 流
    use tracing_subscriber::fmt::writer::MakeWriterExt;
    let stderr = std::io::stderr.with_max_level(tracing::Level::TRACE);
    tracing_subscriber::fmt()
        .with_writer(stderr)
        .with_ansi(false) // v2.30.1：禁用 ANSI 颜色码，避免 CatPaw 等客户端合并 stdout/stderr 时污染 JSON-RPC
        .with_target(false) // 去掉模块名前缀，让日志更干净
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hippocampus_mcp=info".into()),
        )
        .init();

    // 读取存储根目录配置
    let storage_root = PathBuf::from(
        std::env::var("HIPPOCAMPUS_ROOT").unwrap_or_else(|_| "./data".to_string()),
    );

    // v2.11：构造冲突检测器并注入（v2.32：同时获取降级状态）
    let (conflict_detector, detector_status) = build_conflict_detector();

    // v2.18：构造 SessionSearchRouter（注入 storage 支持懒重建，v2.32 同时获取降级状态）
    let (session_search, search_status, embedder_dim) = build_session_search(&storage_root);

    // v2.21 批次 8c：构造 LLM 摘要生成器（v2.32：同时获取降级状态）
    let (summary_generator, generator_status) = build_summary_generator();

    // v2.30：启动时识别 Agent 客户端 + 构建 CombinedProfile（注入行为契约）
    let combined_profile = build_combined_profile();

    // v2.32：汇总降级状态快照（供 get_config 工具查询）
    let runtime_status = hippocampus_mcp::RuntimeStatus {
        conflict_detector: detector_status,
        semantic_search: search_status,
        summary_generator: generator_status,
        embedder_dim,
    };

    tracing::info!(
        root = %storage_root.display(),
        has_combined_profile = combined_profile.is_some(),
        conflict_detector = runtime_status.conflict_detector,
        semantic_search = runtime_status.semantic_search,
        summary_generator = runtime_status.summary_generator,
        "启动 Hippocampus MCP server (stdio 传输)"
    );

    // 启动 stdio MCP server
    let service = HippocampusMcp::with_conflict_detector(
        storage_root,
        Some(conflict_detector),
    )
    .with_session_search(session_search)
    .with_summary_generator(summary_generator)
    .with_scenario_detector(Some(build_scenario_detector()))
    .with_combined_profile(combined_profile)
    .with_runtime_status(runtime_status)
    .serve(stdio())
    .await?;

    service.waiting().await?;

    Ok(())
}

// ============================================================================
// v2.31：install-rules CLI 子命令
// ============================================================================
//
// 用法：
//   hippocampus-mcp install-rules --client <type> --project-root <path> [--force]
//
// 复用 lib.rs 的 install_rules_to_project 公共函数

/// 解析 install-rules CLI 参数并执行安装
fn run_install_rules_cli(args: &[String]) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut client = String::new();
    let mut project_root = String::new();
    let mut force = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--client" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("错误: --client 需要参数");
                    eprintln!("用法: hippocampus-mcp install-rules --client <catpaw|trae|claude-code> --project-root <path> [--force]");
                    std::process::exit(1);
                }
                client = args[i].clone();
            }
            "--project-root" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("错误: --project-root 需要参数");
                    eprintln!("用法: hippocampus-mcp install-rules --client <catpaw|trae|claude-code> --project-root <path> [--force]");
                    std::process::exit(1);
                }
                project_root = args[i].clone();
            }
            "--force" => {
                force = true;
            }
            "--help" | "-h" => {
                eprintln!("安装 Hippocampus Rules 模板到 Agent 客户端\n");
                eprintln!("用法: hippocampus-mcp install-rules --client <type> --project-root <path> [--force]\n");
                eprintln!("参数:");
                eprintln!("  --client <type>          客户端类型: catpaw / trae / claude-code");
                eprintln!("  --project-root <path>    项目根目录的绝对路径");
                eprintln!("  --force                   强制覆盖已存在的文件（默认 false）\n");
                eprintln!("示例:");
                eprintln!("  hippocampus-mcp install-rules --client catpaw --project-root D:/myapp");
                eprintln!("  hippocampus-mcp install-rules --client trae --project-root /home/user/myapp");
                eprintln!("  hippocampus-mcp install-rules --client claude-code --project-root ./myapp --force");
                return Ok(());
            }
            other => {
                eprintln!("错误: 未知参数 {other}");
                eprintln!("用法: hippocampus-mcp install-rules --client <catpaw|trae|claude-code> --project-root <path> [--force]");
                std::process::exit(1);
            }
        }
        i += 1;
    }

    // 验证必填参数
    if client.is_empty() {
        eprintln!("错误: 缺少 --client 参数");
        eprintln!("用法: hippocampus-mcp install-rules --client <catpaw|trae|claude-code> --project-root <path> [--force]");
        std::process::exit(1);
    }
    if project_root.is_empty() {
        eprintln!("错误: 缺少 --project-root 参数");
        eprintln!("用法: hippocampus-mcp install-rules --client <catpaw|trae|claude-code> --project-root <path> [--force]");
        std::process::exit(1);
    }

    // 调用公共函数
    match hippocampus_mcp::install_rules_to_project(&client, &project_root, force) {
        Ok(result_json) => {
            println!("{result_json}");
            Ok(())
        }
        Err(e) => {
            eprintln!("错误: {e}");
            std::process::exit(1);
        }
    }
}
