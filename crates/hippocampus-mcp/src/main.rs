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
//!
//! ## v2.36 重构说明
//!
//! 启动期 `build_*` 函数已抽离到 `hippocampus_mcp::bootstrap` 模块，
//! 供 `hippocampus-mcp` bin（stdio）和 `hippocampus-server` bin（HTTP）复用。

use std::path::PathBuf;

use hippocampus_mcp::HippocampusMcp;
// v2.36：build_* 函数从 bootstrap 模块引入
use hippocampus_mcp::bootstrap::{
    build_combined_profile, build_conflict_detector, build_scenario_detector,
    build_session_search, build_summary_generator,
};
use rmcp::ServiceExt;
use rmcp::transport::stdio;

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
                eprintln!("Hippocampus MCP server v2.36\n");
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
