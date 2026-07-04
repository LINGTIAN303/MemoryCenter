//! # 协作模式
//!
//! 描述 Agent 工具与 Hippocampus 的协作方式。
//!
//! MVP 仅实现 Independent，Cooperative 留作 v2。
//!
//! ## 模式说明
//!
//! ### Independent（独立模式，MVP）
//!
//! Agent 工具独立管理自己的上下文，Hippocampus 被动接收归档：
//! - Agent 工具触发压缩时，调用 Hippocampus 归档被丢弃的内容
//! - Hippocampus 不主动干预 Agent 工具的上下文管理
//! - 归档时机由 Agent 工具决定
//!
//! ### Cooperative（协作模式，v2 未实现）
//!
//! Agent 工具与 Hippocampus 协同管理上下文：
//! - 主动通知 Hippocampus 压缩事件
//! - Hippocampus 可建议保留哪些记忆（基于检索相关性）
//! - 双向通信，Hippocampus 可触发 Agent 工具的压缩

use serde::{Deserialize, Serialize};

/// 协作模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CooperationMode {
    /// 独立模式（MVP）
    ///
    /// Agent 工具独立管理上下文，Hippocampus 被动接收归档
    Independent,

    /// 协作模式（v2，未实现）
    ///
    /// Agent 工具与 Hippocampus 协同管理上下文
    Cooperative,
}

impl Default for CooperationMode {
    fn default() -> Self {
        Self::Independent
    }
}

impl CooperationMode {
    /// 是否为 MVP 支持的模式
    pub fn is_supported(&self) -> bool {
        matches!(self, Self::Independent)
    }

    /// 中文显示名
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Independent => "独立模式",
            Self::Cooperative => "协作模式（v2）",
        }
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_is_independent() {
        assert_eq!(CooperationMode::default(), CooperationMode::Independent);
    }

    #[test]
    fn test_independent_is_supported() {
        assert!(CooperationMode::Independent.is_supported());
    }

    #[test]
    fn test_cooperative_not_supported() {
        assert!(!CooperationMode::Cooperative.is_supported());
    }

    #[test]
    fn test_display_name() {
        assert_eq!(CooperationMode::Independent.display_name(), "独立模式");
        assert_eq!(
            CooperationMode::Cooperative.display_name(),
            "协作模式（v2）"
        );
    }

    #[test]
    fn test_serialize_deserialize() {
        let m = CooperationMode::Independent;
        let json = serde_json::to_string(&m).unwrap();
        let de: CooperationMode = serde_json::from_str(&json).unwrap();
        assert_eq!(m, de);
    }
}
