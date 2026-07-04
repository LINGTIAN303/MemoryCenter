//! # 压缩方式枚举
//!
//! 6 种 Agent 工具的上下文压缩机制：
//! - ClaudeCodeCompact：Claude Code 的 /compact（高压缩比 + 摘要）
//! - CursorChat：Cursor 的 chat 压缩
//! - TraeConversation：Trae 的对话压缩
//! - CodexRolling：Codex 的滚动窗口（直接丢弃，不摘要）
//! - GenericSliding：通用滑动窗口（可配置）
//! - NoCompression：无压缩
//!
//! ## 设计原则
//!
//! 不同 Agent 工具的压缩机制不同，Hippocampus 需要适配：
//! - 高压缩比工具（Claude Code）：被压缩内容多，归档价值高
//! - 低压缩比工具（Codex）：被丢弃内容少，但需保留完整上下文
//! - 无摘要工具（Codex）：Hippocampus 需自己生成摘要

use serde::{Deserialize, Serialize};

/// 压缩方式
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum CompressionScheme {
    /// Claude Code 的 /compact 机制
    ///
    /// - 压缩比高（约 10:1）
    /// - 保留最近 5 轮
    /// - 压缩时生成摘要
    /// - 压缩时归档到 Hippocampus
    ClaudeCodeCompact,

    /// Cursor 的 chat 压缩
    ///
    /// - 中等压缩比（约 5:1）
    /// - 保留最近 3 轮
    /// - 压缩时生成摘要
    /// - 压缩时归档到 Hippocampus
    CursorChat,

    /// Trae 的对话压缩
    ///
    /// - 中等压缩比（约 5:1）
    /// - 保留最近 4 轮
    /// - 压缩时生成摘要
    /// - 压缩时归档到 Hippocampus
    TraeConversation,

    /// Codex 的滚动窗口
    ///
    /// - 低压缩比（约 3:1）
    /// - 保留最近 10 轮
    /// - 不生成摘要（直接丢弃旧内容）
    /// - 压缩时归档到 Hippocampus（保留被丢弃的内容）
    CodexRolling,

    /// 通用滑动窗口（可配置）
    GenericSliding {
        /// 保留最近 N 轮
        keep_recent_turns: usize,
        /// 压缩时是否生成摘要
        summary_on_compress: bool,
    },

    /// 无压缩（不触发压缩，由 Hippocampus 归档阈值控制）
    NoCompression,
}

impl CompressionScheme {
    /// 返回所有内置压缩方式（不含 GenericSliding/NoCompression）
    pub fn all_builtin() -> [Self; 4] {
        [
            Self::ClaudeCodeCompact,
            Self::CursorChat,
            Self::TraeConversation,
            Self::CodexRolling,
        ]
    }

    /// 压缩比（压缩前 token / 压缩后 token）
    ///
    /// 值越大表示压缩越激进
    pub fn compression_ratio(&self) -> f32 {
        match self {
            Self::ClaudeCodeCompact => 10.0,
            Self::CursorChat | Self::TraeConversation => 5.0,
            Self::CodexRolling => 3.0,
            Self::GenericSliding { .. } => 4.0, // 默认 4:1
            Self::NoCompression => 1.0,         // 无压缩
        }
    }

    /// 保留最近 N 轮
    ///
    /// NoCompression 返回 usize::MAX（保留全部）
    pub fn keep_recent_turns(&self) -> usize {
        match self {
            Self::ClaudeCodeCompact => 5,
            Self::CursorChat => 3,
            Self::TraeConversation => 4,
            Self::CodexRolling => 10,
            Self::GenericSliding {
                keep_recent_turns, ..
            } => *keep_recent_turns,
            Self::NoCompression => usize::MAX, // 保留全部
        }
    }

    /// 压缩时是否生成摘要
    pub fn summary_on_compress(&self) -> bool {
        match self {
            Self::ClaudeCodeCompact
            | Self::CursorChat
            | Self::TraeConversation => true,
            Self::CodexRolling => false,
            Self::GenericSliding {
                summary_on_compress,
                ..
            } => *summary_on_compress,
            Self::NoCompression => false,
        }
    }

    /// 是否触发压缩
    pub fn compresses(&self) -> bool {
        !matches!(self, Self::NoCompression)
    }

    /// 中文显示名
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::ClaudeCodeCompact => "Claude Code /compact",
            Self::CursorChat => "Cursor Chat 压缩",
            Self::TraeConversation => "Trae 对话压缩",
            Self::CodexRolling => "Codex 滚动窗口",
            Self::GenericSliding { .. } => "通用滑动窗口",
            Self::NoCompression => "无压缩",
        }
    }
}

impl Default for CompressionScheme {
    fn default() -> Self {
        // 默认通用滑动窗口（5 轮 + 摘要）
        Self::GenericSliding {
            keep_recent_turns: 5,
            summary_on_compress: true,
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
    fn test_all_builtin_count() {
        assert_eq!(CompressionScheme::all_builtin().len(), 4);
    }

    #[test]
    fn test_claude_code_high_compression() {
        let s = CompressionScheme::ClaudeCodeCompact;
        assert!(s.compression_ratio() > 5.0);
        assert_eq!(s.keep_recent_turns(), 5);
        assert!(s.summary_on_compress());
        assert!(s.compresses());
    }

    #[test]
    fn test_codex_no_summary() {
        let s = CompressionScheme::CodexRolling;
        assert!(!s.summary_on_compress());
        assert_eq!(s.keep_recent_turns(), 10);
    }

    #[test]
    fn test_no_compression_keeps_all() {
        let s = CompressionScheme::NoCompression;
        assert!(!s.compresses());
        assert_eq!(s.keep_recent_turns(), usize::MAX);
        assert_eq!(s.compression_ratio(), 1.0);
    }

    #[test]
    fn test_generic_sliding_configurable() {
        let s = CompressionScheme::GenericSliding {
            keep_recent_turns: 8,
            summary_on_compress: false,
        };
        assert_eq!(s.keep_recent_turns(), 8);
        assert!(!s.summary_on_compress());
    }

    #[test]
    fn test_default_is_generic_sliding() {
        let s = CompressionScheme::default();
        assert!(matches!(s, CompressionScheme::GenericSliding { .. }));
    }

    #[test]
    fn test_display_name() {
        assert_eq!(
            CompressionScheme::ClaudeCodeCompact.display_name(),
            "Claude Code /compact"
        );
        assert_eq!(
            CompressionScheme::NoCompression.display_name(),
            "无压缩"
        );
    }

    #[test]
    fn test_serialize_deserialize() {
        let s = CompressionScheme::CursorChat;
        let json = serde_json::to_string(&s).unwrap();
        let de: CompressionScheme = serde_json::from_str(&json).unwrap();
        assert_eq!(de.compression_ratio(), s.compression_ratio());
    }
}
