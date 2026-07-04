//! # Hippocampus 上下文窗口特配库
//!
//! 适配 Agent 工具自身的上下文压缩机制，针对化记忆工作流程。
//!
//! ## 6 种压缩方式
//!
//! | 方式 | 描述 | 压缩比 | 保留轮次 | 摘要 | 归档 |
//! |------|------|--------|----------|------|------|
//! | ClaudeCodeCompact | Claude Code 的 /compact | 高(10:1) | 5 | 是 | 是 |
//! | CursorChat | Cursor 的 chat 压缩 | 中(5:1) | 3 | 是 | 是 |
//! | TraeConversation | Trae 的对话压缩 | 中(5:1) | 4 | 是 | 是 |
//! | CodexRolling | Codex 的滚动窗口 | 低(3:1) | 10 | 否 | 是 |
//! | GenericSliding | 通用滑动窗口 | 可配 | 可配 | 可配 | 是 |
//! | NoCompression | 无压缩 | - | - | - | - |
//!
//! ## 协作模式（MVP 仅 Independent）
//!
//! - Independent：Agent 工具独立管理上下文，Hippocampus 被动接收归档
//! - Cooperative（v2）：协同管理，Hippocampus 可建议保留记忆
//!
//! ## 架构定位
//!
//! 本 crate 是 5 个特配 crate 之一，与 hippocampus-models/scenarios/agents/skills 平行，
//! 不依赖其他特配 crate，联动由 hippocampus-presets 组合层处理。

pub mod compression;
pub mod cooperation;
pub mod window_profile;

pub use compression::CompressionScheme;
pub use cooperation::CooperationMode;
pub use window_profile::WindowProfile;
