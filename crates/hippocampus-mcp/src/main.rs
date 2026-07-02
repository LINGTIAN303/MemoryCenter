//! # Hippocampus MCP Server (stdio)
//!
//! stdio 传输模式的 MCP server 入口。
//! 被 Claude Code / Cursor / Trae 等 MCP 客户端作为子进程拉起。
//!
//! ## 环境变量
//!
//! - `HIPPOCAMPUS_ROOT`：存储根目录（默认 `./data`）
//! - `RUST_LOG`：日志级别（默认 `info`）

use std::path::PathBuf;

use hippocampus_mcp::HippocampusMcp;
use rmcp::ServiceExt;
use rmcp::transport::stdio;

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

    tracing::info!(
        root = %storage_root.display(),
        "启动 Hippocampus MCP server (stdio 传输)"
    );

    // 启动 stdio MCP server
    let service = HippocampusMcp::new(storage_root).serve(stdio()).await?;

    service.waiting().await?;

    Ok(())
}
