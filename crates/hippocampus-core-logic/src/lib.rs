// crates/hippocampus-core-logic/src/lib.rs
//! # Hippocampus Core Logic
//!
//! 核心逻辑 + Storage trait 定义，无原生 IO 依赖，可编译为 WASM。

#![forbid(unsafe_code)]
#![warn(missing_docs, rust_2018_idioms)]

// 暂时注释，Task 3-4 再启用
// pub mod archive;
pub mod bm25;
// pub mod compact;
pub mod conflict;
pub mod context_parser;
pub mod generate;
pub mod heuristic;
// pub mod hybrid;
pub mod migrator;
pub mod model;
// pub mod retrieve;
pub mod score;
pub mod semantic; // 额外迁移：bm25/conflict/heuristic/vector/model 依赖 semantic，不启用会导致 model.rs 业务逻辑被破坏
pub mod serialization;
// pub mod storage;
pub mod vector;

/// Crate 级错误类型
#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    #[error("存储错误: {0}")]
    Storage(String),
    #[error("序列化错误: {0}")]
    Serialize(String),
    #[error("索引错误: {0}")]
    Index(String),
    #[error("评分错误: {0}")]
    Score(String),
    #[error("迁移错误: {0}")]
    Migrate(String),
}

pub type Result<T> = std::result::Result<T, Error>;
