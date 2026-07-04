//! # Hippocampus LLM 客户端组件库（v2.12 新增）
//!
//! 隔离 LLM HTTP 客户端依赖（reqwest），供 `hippocampus-server` 和 `hippocampus-mcp` 共享复用。
//!
//! ## 架构定位
//!
//! - **core**：定义 trait（`ConflictDetector` / `Embedder` / `AsyncScorer` / `AnchorGenerator` / `SummaryGenerator`）+ 配置结构（纯逻辑）
//! - **llm**（本 crate）：HTTP 实现（`HttpLlmDetector` / `HttpEmbedder` / `HttpLlmScorer` / `HttpAnchorGenerator` / `HttpSummaryGenerator`）
//! - **server**：axum handlers，依赖本 crate
//! - **mcp**：MCP tools，依赖本 crate（避免引入 axum/moka 重依赖）
//!
//! ## 模块
//!
//! | 模块 | 实现 | trait | 用途 |
//! |------|------|-------|------|
//! | [`detector`] | `HttpLlmDetector` | `ConflictDetector` | LLM 语义级冲突检测 |
//! | [`embedder`] | `HttpEmbedder` | `Embedder` | 文本向量化（语义检索） |
//! | [`scorer`] | `HttpLlmScorer` | `AsyncScorer` | LLM 评分（月级淘汰） |
//! | [`anchor_generator`] | `HttpAnchorGenerator` | `AnchorGenerator` | LLM 线索锚点生成（v2.16 IMP-05） |
//! | [`summary_generator`] | `HttpSummaryGenerator` | `SummaryGenerator` | LLM 结构化摘要生成（v2.16 IMP-10） |
//!
//! ## v2.12 迁移说明
//!
//! 本 crate 从 `hippocampus-server` 下沉而来，原 `server/llm_detector.rs` / `server/embedding.rs` /
//! `server/llm.rs` 已迁移到此处。`hippocampus-server` 通过 re-export 保持向后兼容。

pub mod anchor_generator;
pub mod detector;
pub mod embedder;
pub mod scorer;
pub mod summary_generator;

pub use anchor_generator::HttpAnchorGenerator;
pub use detector::{HttpLlmDetector, LlmDetectorConfig};
pub use embedder::{EmbedderConfig, HttpEmbedder};
pub use scorer::HttpLlmScorer;
pub use summary_generator::HttpSummaryGenerator;
