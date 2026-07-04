//! # Hippocampus LLM 模型特配库
//!
//! 提供 LLM 模型家族识别、型号参数、Tokenizer 抽象与实现，
//! 驱动 Hippocampus 的针对化记忆工作流程。
//!
//! ## 架构定位
//!
//! - **家族（Family）**：稳定的模型大类（Claude/GPT/Gemini/...），低频迭代
//! - **型号（Variant）**：具体版本（Opus 4.6 / Sonnet 5 / GPT-5.2 / Gemini 3.1 Pro），高频迭代
//! - **Tokenizer**：token 计数方式，基于 tiktoken-rs 实现 + 字符级兜底
//!
//! ## 2026 年 7 月最新型号（已核查官方文档）
//!
//! | 家族 | 型号 | 发布日期 | 上下文 | 关键特性 |
//! |---|---|---|---|---|
//! | Claude | claude-opus-4.6 | 2026-02 | 1M（Beta） | 思考链、多模态、超长上下文 |
//! | Claude | claude-opus-4.8 | 2026-05 | 200K | Opus 级稳定旗舰、思考链 |
//! | Claude | claude-sonnet-5 | 2026-06-30 | 200K | Agent 默认模型、思考链 |
//! | Claude | claude-fable-5 | 2026-06-10 | 200K | Mythos 级、防护版、7-02 全球恢复 |
//! | Claude | claude-mythos-5 | 2026-06-10 | 200K | Mythos 级、未防护版、面向合作方 |
//! | GPT | gpt-5.2 / gpt-5-codex | 2026 | 128K | function calling |
//! | Gemini | gemini-3.1-pro | 2026-02-20 | 1M | 推理 2x、ARC-AGI-2 77.1% |
//! | DeepSeek | deepseek-v4-pro | 2026-04-24 | 1M | MoE 1.6T/49B、思考链 |
//! | DeepSeek | deepseek-v4-flash | 2026-04-24 | 1M | MoE 284B/13B、轻量 |
//! | Qwen | qwen-3-coder | 2025-07-23 | 256K | 358 语言、Agentic Coding |
//! | Llama | llama-4-scout | 2025-04 | 1M* | MoE 109B、轻量 |
//! | Llama | llama-4-maverick | 2025-04 | 1M | MoE 400B、旗舰 |
//! | Grok | grok-4.1 | 2026 | 128K | 实时数据接入 |
//!
//! \* Llama 4 Scout 理论支持 10M 上下文，API 实际部署多为 1M，本库保守取 1M。
//!
//! **Claude 家族层级**（从高到低）：
//! ```text
//! Mythos 级（最高）: Fable 5（防护版）/ Mythos 5（未防护版）—— 共享底层模型
//! Opus 级（旗舰） : Opus 4.8（当前默认） / Opus 4.6
//! Sonnet 级（主力）: Sonnet 5
//! ```
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
