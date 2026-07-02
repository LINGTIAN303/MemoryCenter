//! # Hippocampus Core
//!
//! Agent 记忆库的核心逻辑库，无 IO 依赖。
//!
//! ## 模块组织
//!
//! - [`model`]：核心数据模型（记忆文件、索引钩子、标签等）
//! - [`archive`]：归档/冻结逻辑（达到阈值时将上下文冻结为记忆文件）
//! - [`index`]：索引文档与钩子管理
//! - [`retrieve`]：检索机制（摘要钩子注入 + tool 主动检索）
//! - [`compact`]：周期任务（周去重合并 / 月评分淘汰）
//! - [`score`]：评分 trait + 默认启发式实现
//! - [`storage`]：存储后端 trait + 默认本地文件树实现
//! - [`migrator`]：Schema 版本迁移
//!
//! ## 核心概念
//!
//! - **归档（freeze）**：达到 token 阈值时，将完整上下文（用户消息+LLM消息）保存为记忆文件，非摘要
//! - **索引钩子（hook）**：指向记忆库中记忆文件的指针，带 17 类细粒度标签
//! - **三级周期**：天级归档 / 周级无损去重合并 / 月级评分淘汰

#![forbid(unsafe_code)]
#![warn(missing_docs, rust_2018_idioms)]

pub mod archive;
pub mod compact;
pub mod index;
pub mod migrator;
pub mod model;
pub mod retrieve;
pub mod score;
pub mod storage;

/// Crate 级错误类型
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 存储错误
    #[error("存储错误: {0}")]
    Storage(String),
    /// 序列化错误
    #[error("序列化错误: {0}")]
    Serialize(String),
    /// 索引错误
    #[error("索引错误: {0}")]
    Index(String),
    /// 评分错误
    #[error("评分错误: {0}")]
    Score(String),
    /// 迁移错误
    #[error("迁移错误: {0}")]
    Migrate(String),
}

/// Crate 级结果别名
pub type Result<T> = std::result::Result<T, Error>;
