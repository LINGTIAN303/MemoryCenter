//! # Hippocampus Agent 代理工具特配库
//!
//! 5 种特配 crate 之一（平行拓扑，仅依赖 hippocampus-core）。
//!
//! ## 定位
//!
//! 识别当前使用 Hippocampus 的 Agent 代理工具（Claude Code / Cursor / Trae 等），
//! 为 presets 组合层提供 agent 维度的默认配置：
//! - 是否归档到 Hippocampus（部分 Agent 有自己的记忆机制）
//! - 是否支持工具调用（影响标签策略）
//! - 是否有原生压缩机制（影响 window 策略选择）
//! - 默认 session ID 前缀（便于按 Agent 隔离记忆）
//!
//! ## family / variant 分离设计
//!
//! - **family**（家族）：稳定枚举，11 个主流 Agent + Custom 兜底
//! - **variant**（型号）：高频迭代字段，字符串保存（如 Cursor 1.45 / Claude Code 2.0）
//!
//! family 由本 crate 维护，variant 由调用方按需注入。
//!
//! ## 11 个主流 Agent family
//!
//! | family | 说明 | MVP 预设 |
//! |---|---|---|
//! | ClaudeCode | Anthropic Claude Code CLI | ✅ |
//! | Cursor | Cursor IDE | ✅ |
//! | Trae | ByteDance Trae IDE | ✅ |
//! | Codex | OpenAI Codex CLI | ✅ |
//! | Zcode | Zcode | generic |
//! | OpenCode | OpenCode | generic |
//! | Qoder | Qoder | generic |
//! | WorkBuddy | WorkBuddy | generic |
//! | CatPaw | CatPaw | generic |
//! | OpenClaw | OpenClaw | generic |
//! | Marvis | Marvis | generic |
//! | Custom(String) | 用户自定义兜底 | generic |
//!
//! ## 与其他特配 crate 的关系
//!
//! 本 crate **不依赖** windows / scenarios / skills / models，
//! Agent → 压缩方式 / 场景 / 技能的映射由 `hippocampus-presets` 组合层处理。
//!
//! ## 联动机制（由 presets 层实现）
//!
//! - Agent=ClaudeCode → 默认 window=CompressionScheme::ClaudeCodeCompact
//! - Agent=Cursor → 默认 window=CompressionScheme::CursorChat
//! - Agent=Trae → 默认 window=CompressionScheme::TraeConversation
//! - Agent=Codex → 默认 window=CompressionScheme::CodexRolling
//! - 其他 Agent → 默认 window=CompressionScheme::GenericSliding

pub mod agent_family;
pub mod agent_profile;

pub use agent_family::{AgentFamily, AgentFingerprint};
pub use agent_profile::AgentProfile;
