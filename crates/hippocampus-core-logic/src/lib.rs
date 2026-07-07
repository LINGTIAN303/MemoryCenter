// crates/hippocampus-core-logic/src/lib.rs
//! # Hippocampus Core Logic
//!
//! 核心逻辑 + Storage trait 定义，无原生 IO 依赖，可编译为 WASM。

#![forbid(unsafe_code)]
#![warn(missing_docs, rust_2018_idioms)]

pub mod archive;
// BM25 模块：native 模式用 jieba 中文分词，WASM 模式用简易字符分词
// 两者公共接口完全一致（Bm25Searcher + KeywordSearcher trait impl）
#[cfg(feature = "native")]
pub mod bm25;
#[cfg(not(feature = "native"))]
#[path = "bm25_wasm.rs"]
pub mod bm25;
pub mod compact;
pub mod conflict;
pub mod context_parser;
pub mod generate;
pub mod heuristic;
pub mod hybrid;
pub mod migrator;
pub mod model;
pub mod retrieve;
pub mod score;
pub mod semantic;
pub mod serialization;
pub mod storage;
pub mod vector;

#[cfg(test)]
pub mod test_support;

/// Crate 级错误类型
#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    /// 存储后端错误（读写失败、文件不存在、连接异常等）
    #[error("存储错误: {0}")]
    Storage(String),
    /// 序列化/反序列化错误（JSON、MessagePack 格式转换失败）
    #[error("序列化错误: {0}")]
    Serialize(String),
    /// 索引文档操作错误（追加钩子失败、索引损坏等）
    #[error("索引错误: {0}")]
    Index(String),
    /// 评分计算错误（Scorer trait 实现异常、数据不足等）
    #[error("评分错误: {0}")]
    Score(String),
    /// Schema 迁移错误（版本不兼容、迁移步骤失败等）
    #[error("迁移错误: {0}")]
    Migrate(String),
}

/// Crate 级 Result 类型别名（默认错误类型为 [`Error`]）
pub type Result<T> = std::result::Result<T, Error>;
