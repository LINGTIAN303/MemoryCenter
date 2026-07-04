//! # Hippocampus LLM 模型特配库
//!
//! 提供 LLM 模型家族识别、型号参数、Tokenizer 抽象与实现，
//! 驱动 Hippocampus 的针对化记忆工作流程。
//!
//! ## 架构定位
//!
//! - **家族（Family）**：稳定的模型大类（Claude/GPT/Gemini/...），低频迭代
//! - **型号（Variant）**：具体版本（Opus 4.6/Sonnet 4.5/GPT-5.2），高频迭代
//! - **Tokenizer**：token 计数方式，基于 tiktoken-rs 实现 + 字符级兜底
//!
//! ## 设计原则
//!
//! - **家族/型号分离**：家族稳定（enum），型号可配置（struct + 构造器）
//! - **重依赖隔离**：tiktoken-rs 仅在此 crate，不污染 presets/core
//! - **无依赖兜底**：未启用 tiktoken 时降级为 CharTokenizer（中文 1 字 ≈ 1.5 token）
//! - **可扩展**：Custom 变体支持用户自定义新型号，无需等发版

pub mod char_impl;
pub mod family;
pub mod registry;
pub mod tiktoken_impl;
pub mod tokenizer;
pub mod variant;

// 公开导出核心类型
pub use char_impl::CharTokenizer;
pub use family::ModelFamily;
pub use registry::ModelRegistry;
pub use tiktoken_impl::TiktokenTokenizer;
pub use tokenizer::{Tokenizer, TokenizerKind};
pub use variant::{ArchiveStrategy, ModelVariant, ToolCallFormat};
