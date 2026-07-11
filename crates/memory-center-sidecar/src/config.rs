//! # Sidecar 配置（v2.36 新增，v2.50 改为直写存储）
//!
//! 通过 CLI 参数 + 环境变量配置 sidecar 行为。
//!
//! ## 环境变量
//!
//! | 环境变量 | 说明 | 默认值 |
//! |---------|------|--------|
//! | `OPENCODE_DB_PATH` | OpenCode SQLite 路径 | 平台默认路径 |
//! | `MEMORY_CENTER_ROOT` | MemoryCenter 存储根目录 | `./data` |
//! | `OPENCODE_SIDECAR_POLL_INTERVAL` | 轮询间隔（秒） | `5` |
//! | `OPENCODE_SIDECAR_PROJECT_ID` | 项目 ID | `opencode` |
//!
//! v2.50：移除 `MEMORYCENTER_URL` / `MEMORYCENTER_API_KEY`（不再依赖 HTTP server），
//! 新增 `MEMORY_CENTER_ROOT`（直写存储目录，与 mc-server / mc-mcp 共用）。

use std::path::PathBuf;
use clap::Parser;

/// OpenCode 压缩事件监听 sidecar
///
/// 监听 OpenCode SQLite 会话库的压缩事件，自动触发 MemoryCenter 归档。
/// OpenCode 端零源码改动，完全在 MemoryCenter 侧实现。
///
/// v2.46：支持多 Agent adapter，通过 --agent 选择。
/// v2.50：直写存储，不再依赖 HTTP server。
#[derive(Parser, Debug, Clone)]
#[command(name = "mc-sidecar", version, about)]
pub struct SidecarConfig {
    /// Agent 适配器类型（v2.46 新增）
    ///
    /// 选择 sidecar 监听的 Agent 工具，决定数据源读取方式。
    /// - `opencode`（默认）：读取 OpenCode SQLite 数据库
    /// - `claude-code`：读取 Claude Code 日志文件（未来实现）
    ///
    /// 各 Agent 特有参数仍用现有字段（如 --opencode-db），
    /// 未来加 --claude-code-log-dir 等。
    #[arg(long, env = "MC_SIDECAR_AGENT", default_value = "opencode")]
    pub agent: String,

    /// OpenCode SQLite 数据库路径
    ///
    /// 默认按平台查找：
    /// - Linux: ~/.local/share/opencode/opencode.db
    /// - macOS: ~/Library/Application Support/opencode/opencode.db
    /// - Windows: %APPDATA%\opencode\opencode.db
    #[arg(long, env = "OPENCODE_DB_PATH")]
    pub opencode_db: Option<PathBuf>,

    /// MemoryCenter 存储根目录（v2.50 新增，替代 memorycenter_url）
    ///
    /// sidecar 直接写入此目录（与 mc-server / mc-mcp 共用同一存储）。
    /// 应与 `MEMORY_CENTER_ROOT` 环境变量保持一致。
    #[arg(long, env = "MEMORY_CENTER_ROOT", default_value = "./data")]
    pub storage_root: PathBuf,

    /// 轮询间隔（秒）
    #[arg(long, env = "OPENCODE_SIDECAR_POLL_INTERVAL", default_value = "5")]
    pub poll_interval: u64,

    /// 归档时使用的项目 ID
    #[arg(long, env = "OPENCODE_SIDECAR_PROJECT_ID", default_value = "opencode")]
    pub project_id: String,

    /// 启动时全量扫描已有压缩事件（归档历史压缩会话）
    #[arg(long, env = "OPENCODE_SIDECAR_BACKFILL", default_value = "false")]
    pub backfill: bool,

    /// 单次会话最多归档的 turns 数（防止超大会话撑爆 MemoryCenter）
    #[arg(long, env = "OPENCODE_SIDECAR_MAX_TURNS", default_value = "100")]
    pub max_turns: usize,

    /// 状态文件路径（持久化已处理的 compaction ID，避免重复归档）
    ///
    /// 默认按平台：
    /// - Linux: ~/.local/share/mc-sidecar/state.json
    /// - macOS: ~/Library/Application Support/mc-sidecar/state.json
    /// - Windows: ~/.local/share/mc-sidecar/state.json
    #[arg(long, env = "MC_SIDECAR_STATE_FILE")]
    pub state_file: Option<PathBuf>,

    /// Token 阈值（v2.47 新增）
    ///
    /// 当 session 累积 tokens 达到此值 * 触发比例时，sidecar 主动归档 + 插入 compaction 消息对。
    /// - `0`（默认）：从服务器归档响应的 `threshold` 字段缓存，最终降级到 120000
    /// - 非 0：直接使用此值（覆盖服务器阈值）
    ///
    /// 优先级：CLI 参数 > 服务器缓存 > 默认 120000
    #[arg(long, env = "MC_SIDECAR_TOKEN_THRESHOLD", default_value = "0")]
    pub token_threshold: usize,

    /// 触发主动归档的比例（v2.47 新增）
    ///
    /// 累积 tokens >= threshold * ratio / 100 时触发。
    /// 默认 80（即阈值的 80%），避免等到 OpenCode 自身触发 compaction 才归档。
    #[arg(long, env = "MC_SIDECAR_TOKEN_TRIGGER_RATIO", default_value = "80")]
    pub token_trigger_ratio: u64,

    /// Compaction 时保留的尾部轮数（v2.47 新增）
    ///
    /// 插入 compaction 消息对时，保留最近 N 轮对话不归档（作为 tail）。
    /// 与 OpenCode 原生行为一致（默认 2 轮），让 LLM 保持近期上下文连续性。
    #[arg(long, env = "MC_SIDECAR_TAIL_TURNS", default_value = "2")]
    pub tail_turns: usize,
}

impl SidecarConfig {
    /// 解析 OpenCode SQLite 路径
    ///
    /// 优先级：CLI 参数 > 环境变量 > 平台默认路径
    pub fn resolve_db_path(&self) -> Result<PathBuf, std::io::Error> {
        if let Some(path) = &self.opencode_db {
            return Ok(path.clone());
        }

        // 平台默认路径
        // 注意：opencode 在所有平台都使用类 Unix 路径风格（~/.local/share/opencode）
        let path = if cfg!(target_os = "linux") {
            dirs_home().join(".local/share/opencode/opencode.db")
        } else if cfg!(target_os = "macos") {
            dirs_home().join("Library/Application Support/opencode/opencode.db")
        } else if cfg!(target_os = "windows") {
            // opencode 在 Windows 上也使用 ~/.local/share/opencode 路径（非 %APPDATA%）
            dirs_home().join(".local/share/opencode/opencode.db")
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "不支持的平台，请通过 --opencode-db 显式指定路径",
            ));
        };

        Ok(path)
    }
}

/// 获取用户 home 目录（避免引入 dirs crate）
fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}
