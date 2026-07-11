//! # Sidecar 归档引擎（v2.50 新增，替代 archive.rs HTTP 客户端）
//!
//! 封装 `memory-center-archive-core::ArchiveEngine`，让 sidecar 直接写入
//! LocalStorage（.mcp-data 目录），不再通过 HTTP server 中转。
//!
//! ## 架构变化
//!
//! ```text
//! v2.49（旧）：sidecar → HTTP POST → server → LocalStorage
//! v2.50（新）：sidecar → ArchiveEngine → LocalStorage（直写）
//! ```
//!
//! ## SidecarTurn → MessageTurn 转换
//!
//! sidecar 从 OpenCode DB 读取的是 `SidecarTurn`（只含 sidecar 能产出的字段），
//! ArchiveEngine.pre_compress 接受 `MessageTurn`（含 id/timestamp/tags 等服务端字段）。
//!
//! 利用 serde 兼容性（SidecarTurn 只 Serialize，MessageTurn 用 `#[serde(default)]`
//! 反序列化），通过 JSON Value 中转完成转换，无需手动逐字段映射。

use std::path::PathBuf;

use memory_center_adapter::SidecarTurn;
use memory_center_archive_core::{
    ArchiveEngine, ArchiveError, PreCompressResult, build_engine_from_env,
};
use memory_center_core::model::MessageTurn;

/// Sidecar 归档引擎（封装 ArchiveEngine）
///
/// 与旧 `ArchiveClient` 的关键差异：
/// - `health_check()` 是同步方法（不再需要 async HTTP 请求）
/// - `pre_compress()` 接受 `Vec<SidecarTurn>`，内部自动转换为 `Vec<MessageTurn>`
pub struct SidecarArchiveEngine {
    engine: ArchiveEngine,
}

impl SidecarArchiveEngine {
    /// 从配置构造引擎（注入 LLM 组件 from env）
    pub fn from_storage_root(storage_root: PathBuf) -> Self {
        Self {
            engine: build_engine_from_env(storage_root),
        }
    }

    /// 健康检查：存储目录可写
    ///
    /// 与旧 `ArchiveClient::health_check` 的差异：
    /// - 旧：async，GET `/api/v1/presets/agents` 确认 server 在线
    /// - 新：sync，检查存储目录存在且可写（不依赖 server 进程）
    pub fn health_check(&self) -> Result<bool, ArchiveError> {
        self.engine.health_check()
    }

    /// 压缩前归档（接受 SidecarTurn，内部转 MessageTurn）
    ///
    /// 参数与旧 `ArchiveClient::pre_compress` 对齐，调用方无需改动调用逻辑。
    pub async fn pre_compress(
        &self,
        session_id: &str,
        turns: Vec<SidecarTurn>,
        estimated_tokens: usize,
        project_id: &str,
    ) -> Result<PreCompressResult, ArchiveError> {
        // SidecarTurn → MessageTurn 转换（JSON roundtrip，利用 serde 兼容性）
        let json = serde_json::to_value(&turns).map_err(|e| {
            ArchiveError::BadRequest(format!("SidecarTurn 序列化失败: {e}"))
        })?;
        let message_turns: Vec<MessageTurn> = serde_json::from_value(json).map_err(|e| {
            ArchiveError::BadRequest(format!("MessageTurn 反序列化失败: {e}"))
        })?;

        self.engine
            .pre_compress(
                session_id,
                message_turns,
                Some(estimated_tokens),
                Some(project_id),
                None, // preset：sidecar 不使用预设
                None, // task_state_snapshot：sidecar 不传任务状态
            )
            .await
    }
}
