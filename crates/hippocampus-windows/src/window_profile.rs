//! # 上下文窗口特配组合（WindowProfile）
//!
//! 将压缩方式 + 协作模式 + 触发阈值 + 归档策略组合为完整的窗口特配。
//!
//! ## 使用方式
//!
//! ```rust,ignore
//! use hippocampus_windows::{WindowProfile, CompressionScheme};
//!
//! // 1. 使用预设
//! let profile = WindowProfile::claude_code();
//!
//! // 2. 自定义
//! let profile = WindowProfile::from_scheme(CompressionScheme::GenericSliding {
//!     keep_recent_turns: 8,
//!     summary_on_compress: true,
//! }).with_trigger_threshold(150_000);
//! ```

use crate::compression::CompressionScheme;
use crate::cooperation::CooperationMode;
use serde::{Deserialize, Serialize};

/// 上下文窗口特配
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowProfile {
    /// 压缩方式
    pub scheme: CompressionScheme,
    /// 协作模式（MVP 仅 Independent）
    pub cooperation_mode: CooperationMode,
    /// 触发压缩的阈值（token 数）
    ///
    /// Agent 工具上下文达到此阈值时触发压缩。
    /// 不同工具默认值不同：
    /// - Claude Code: ~180K（200K 窗口的 90%）
    /// - Cursor: ~150K
    /// - Trae: ~120K
    /// - Codex: ~100K
    pub trigger_threshold: usize,
    /// 压缩时是否归档到 Hippocampus
    ///
    /// 默认 true：被压缩丢弃的内容归档到 Hippocampus 保留
    pub archive_to_hippocampus: bool,
}

impl WindowProfile {
    /// 从压缩方式构建默认 profile
    pub fn from_scheme(scheme: CompressionScheme) -> Self {
        let trigger_threshold = default_trigger_threshold(&scheme);
        Self {
            scheme,
            cooperation_mode: CooperationMode::default(),
            trigger_threshold,
            archive_to_hippocampus: true,
        }
    }

    /// 设置协作模式
    pub fn with_cooperation_mode(mut self, mode: CooperationMode) -> Self {
        self.cooperation_mode = mode;
        self
    }

    /// 覆盖触发阈值
    pub fn with_trigger_threshold(mut self, threshold: usize) -> Self {
        self.trigger_threshold = threshold;
        self
    }

    /// 设置是否归档到 Hippocampus
    pub fn with_archive_to_hippocampus(mut self, archive: bool) -> Self {
        self.archive_to_hippocampus = archive;
        self
    }

    /// 校验合法性
    ///
    /// - 协作模式必须在 MVP 支持范围内
    /// - 触发阈值不能为 0（NoCompression 除外）
    pub fn validate(&self) -> Result<(), String> {
        if !self.cooperation_mode.is_supported() {
            return Err(format!(
                "协作模式 {} 未在 MVP 中实现",
                self.cooperation_mode.display_name()
            ));
        }
        // NoCompression 的 trigger_threshold 为 usize::MAX，不算 0
        if self.trigger_threshold == 0 && self.scheme.compresses() {
            return Err("触发阈值不能为 0".to_string());
        }
        Ok(())
    }

    /// Claude Code 默认 profile
    pub fn claude_code() -> Self {
        Self::from_scheme(CompressionScheme::ClaudeCodeCompact)
    }

    /// Cursor 默认 profile
    pub fn cursor() -> Self {
        Self::from_scheme(CompressionScheme::CursorChat)
    }

    /// Trae 默认 profile
    pub fn trae() -> Self {
        Self::from_scheme(CompressionScheme::TraeConversation)
    }

    /// Codex 默认 profile
    pub fn codex() -> Self {
        Self::from_scheme(CompressionScheme::CodexRolling)
    }

    /// 无压缩 profile（由 Hippocampus 归档阈值控制）
    pub fn no_compression() -> Self {
        Self::from_scheme(CompressionScheme::NoCompression)
    }
}

impl Default for WindowProfile {
    fn default() -> Self {
        Self::from_scheme(CompressionScheme::default())
    }
}

/// 默认触发阈值
///
/// 不同 Agent 工具的上下文窗口大小不同，触发阈值也不同：
/// - Claude Code：200K 窗口的 90%（180K）
/// - Cursor：约 150K
/// - Trae：约 120K
/// - Codex：约 100K
fn default_trigger_threshold(scheme: &CompressionScheme) -> usize {
    match scheme {
        // Claude Code：200K 窗口的 90%
        CompressionScheme::ClaudeCodeCompact => 180_000,
        // Cursor：约 150K
        CompressionScheme::CursorChat => 150_000,
        // Trae：约 120K
        CompressionScheme::TraeConversation => 120_000,
        // Codex：约 100K
        CompressionScheme::CodexRolling => 100_000,
        // 通用滑动：默认 100K
        CompressionScheme::GenericSliding { .. } => 100_000,
        // 无压缩：无意义，设为 usize::MAX
        CompressionScheme::NoCompression => usize::MAX,
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claude_code_profile() {
        let p = WindowProfile::claude_code();
        assert_eq!(p.trigger_threshold, 180_000);
        assert!(p.archive_to_hippocampus);
        assert!(p.validate().is_ok());
    }

    #[test]
    fn test_cursor_profile() {
        let p = WindowProfile::cursor();
        assert_eq!(p.trigger_threshold, 150_000);
        assert!(p.validate().is_ok());
    }

    #[test]
    fn test_codex_profile() {
        let p = WindowProfile::codex();
        assert_eq!(p.trigger_threshold, 100_000);
        assert!(p.validate().is_ok());
    }

    #[test]
    fn test_no_compression_profile() {
        let p = WindowProfile::no_compression();
        assert_eq!(p.trigger_threshold, usize::MAX);
        assert!(p.validate().is_ok());
    }

    #[test]
    fn test_with_trigger_threshold_override() {
        let p = WindowProfile::claude_code().with_trigger_threshold(200_000);
        assert_eq!(p.trigger_threshold, 200_000);
    }

    #[test]
    fn test_with_archive_disabled() {
        let p = WindowProfile::claude_code().with_archive_to_hippocampus(false);
        assert!(!p.archive_to_hippocampus);
    }

    #[test]
    fn test_cooperative_mode_not_supported() {
        let p = WindowProfile::claude_code()
            .with_cooperation_mode(CooperationMode::Cooperative);
        assert!(p.validate().is_err());
    }

    #[test]
    fn test_independent_mode_supported() {
        let p = WindowProfile::claude_code()
            .with_cooperation_mode(CooperationMode::Independent);
        assert!(p.validate().is_ok());
    }

    #[test]
    fn test_generic_sliding_profile() {
        let p = WindowProfile::from_scheme(CompressionScheme::GenericSliding {
            keep_recent_turns: 8,
            summary_on_compress: true,
        });
        assert_eq!(p.trigger_threshold, 100_000);
        assert!(p.validate().is_ok());
    }

    #[test]
    fn test_all_builtin_profiles_valid() {
        for scheme in CompressionScheme::all_builtin() {
            let name = scheme.display_name().to_string();
            let p = WindowProfile::from_scheme(scheme);
            assert!(p.validate().is_ok(), "{} profile 不合法", name);
        }
    }

    #[test]
    fn test_default_profile() {
        let p = WindowProfile::default();
        assert!(p.validate().is_ok());
    }

    #[test]
    fn test_serialize_deserialize() {
        let p = WindowProfile::claude_code();
        let json = serde_json::to_string(&p).unwrap();
        let de: WindowProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(p.trigger_threshold, de.trigger_threshold);
    }
}
