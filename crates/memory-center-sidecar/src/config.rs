//! # Sidecar 配置（v2.36 新增）
//!
//! 通过 CLI 参数 + 环境变量配置 sidecar 行为。
//!
//! ## 环境变量
//!
//! | 环境变量 | 说明 | 默认值 |
//! |---------|------|--------|
//! | `OPENCODE_DB_PATH` | OpenCode SQLite 路径 | 平台默认路径 |
//! | `MEMORYCENTER_URL` | MemoryCenter HTTP 地址 | `http://127.0.0.1:8080` |
//! | `MEMORYCENTER_API_KEY` | MemoryCenter API Key | 空（不鉴权） |
//! | `OPENCODE_SIDECAR_POLL_INTERVAL` | 轮询间隔（秒） | `5` |
//! | `OPENCODE_SIDECAR_PROJECT_ID` | 项目 ID | `opencode` |

use std::path::PathBuf;
use clap::Parser;

/// OpenCode 压缩事件监听 sidecar
///
/// 监听 OpenCode SQLite 会话库的压缩事件，自动触发 MemoryCenter 归档。
/// OpenCode 端零源码改动，完全在 MemoryCenter 侧实现。
#[derive(Parser, Debug, Clone)]
#[command(name = "mc-sidecar", version, about)]
pub struct SidecarConfig {
    /// OpenCode SQLite 数据库路径
    ///
    /// 默认按平台查找：
    /// - Linux: ~/.local/share/opencode/opencode.db
    /// - macOS: ~/Library/Application Support/opencode/opencode.db
    /// - Windows: %APPDATA%\opencode\opencode.db
    #[arg(long, env = "OPENCODE_DB_PATH")]
    pub opencode_db: Option<PathBuf>,

    /// MemoryCenter HTTP 服务地址
    #[arg(long, env = "MEMORYCENTER_URL", default_value = "http://127.0.0.1:8080")]
    pub memorycenter_url: String,

    /// MemoryCenter API Key（若服务端配置了鉴权）
    #[arg(long, env = "MEMORYCENTER_API_KEY")]
    pub memorycenter_api_key: Option<String>,

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
    /// - Windows: %APPDATA%\mc-sidecar\state.json
    #[arg(long, env = "MC_SIDECAR_STATE_FILE")]
    pub state_file: Option<PathBuf>,
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
        let path = if cfg!(target_os = "linux") {
            dirs_home().join(".local/share/opencode/opencode.db")
        } else if cfg!(target_os = "macos") {
            dirs_home().join("Library/Application Support/opencode/opencode.db")
        } else if cfg!(target_os = "windows") {
            std::env::var("APPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|_| dirs_home().join("AppData/Roaming"))
                .join("opencode/opencode.db")
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
