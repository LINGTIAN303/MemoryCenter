//! # sidecar 状态持久化（v2.41 新增）
//!
//! 解决 backfill 重复归档问题：每次 `--backfill` 都会归档所有历史 compaction，
//! 因为 sidecar 重启后 `processed_message_ids` 丢失。
//!
//! ## 状态文件
//!
//! 路径：`--state-file` 指定，默认按平台：
//! - Linux: `~/.local/share/mc-sidecar/state.json`
//! - macOS: `~/Library/Application Support/mc-sidecar/state.json`
//! - Windows: `~/.local/share/mc-sidecar/state.json`
//!
//! ## 结构
//!
//! ```json
//! {
//!   "processed_message_ids": ["msg_xxx", "msg_yyy"],
//!   "last_archived_seq": { "ses_xxx": 1780817115050 }
//! }
//! ```

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// sidecar 持久化状态
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SidecarState {
    /// 已处理的 compaction 消息 ID 集合（msg_xxx），避免重复归档
    pub processed_message_ids: HashSet<String>,
    /// 每个 session 上次归档的 compaction seq（用于增量归档范围起点）
    pub last_archived_seq: HashMap<String, i64>,
}

impl SidecarState {
    /// 从文件加载状态，文件不存在则返回空状态
    pub fn load(path: &std::path::Path) -> Result<Self, StateError> {
        if !path.exists() {
            tracing::info!(
                state_file = %path.display(),
                "状态文件不存在，使用空状态（首次启动）"
            );
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(path)?;
        if content.trim().is_empty() {
            tracing::warn!(state_file = %path.display(), "状态文件为空，使用空状态");
            return Ok(Self::default());
        }

        let state: Self = serde_json::from_str(&content)?;
        tracing::info!(
            state_file = %path.display(),
            processed_count = state.processed_message_ids.len(),
            session_count = state.last_archived_seq.len(),
            "状态文件加载成功"
        );
        Ok(state)
    }

    /// 保存状态到文件（原子写入：先写临时文件再 rename）
    pub fn save(&self, path: &std::path::Path) -> Result<(), StateError> {
        // 确保父目录存在
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string_pretty(self)?;

        // 原子写入：先写 .tmp 再 rename（避免写入中断导致文件损坏）
        let tmp_path = path.with_extension("json.tmp");
        std::fs::write(&tmp_path, &json)?;
        std::fs::rename(&tmp_path, path)?;

        tracing::debug!(
            state_file = %path.display(),
            processed_count = self.processed_message_ids.len(),
            "状态文件保存成功"
        );
        Ok(())
    }

    /// 标记 compaction 事件已归档（更新内存状态）
    pub fn mark_archived(&mut self, message_id: &str, session_id: &str, seq: i64) {
        self.processed_message_ids.insert(message_id.to_string());
        self.last_archived_seq.insert(session_id.to_string(), seq);
    }

    /// 检查 compaction 消息是否已处理
    pub fn is_processed(&self, message_id: &str) -> bool {
        self.processed_message_ids.contains(message_id)
    }

    /// 获取 session 上次归档的 seq
    pub fn get_last_seq(&self, session_id: &str) -> Option<i64> {
        self.last_archived_seq.get(session_id).copied()
    }
}

/// 解析状态文件默认路径
///
/// 优先级：CLI 参数 > 平台默认路径
pub fn resolve_state_path(cli_path: Option<&PathBuf>) -> Result<PathBuf, std::io::Error> {
    if let Some(p) = cli_path {
        return Ok(p.clone());
    }

    // 平台默认路径
    // 注意：Windows 上也使用 ~/.local/share/mc-sidecar/ 路径（与 opencode 一致）
    let path = if cfg!(target_os = "linux") {
        dirs_home().join(".local/share/mc-sidecar/state.json")
    } else if cfg!(target_os = "macos") {
        dirs_home()
            .join("Library/Application Support/mc-sidecar/state.json")
    } else if cfg!(target_os = "windows") {
        dirs_home().join(".local/share/mc-sidecar/state.json")
    } else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "不支持的平台，请通过 --state-file 显式指定路径",
        ));
    };

    Ok(path)
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// 状态文件错误
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON 解析错误: {0}")]
    Json(#[from] serde_json::Error),
}
