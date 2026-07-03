//! # Hippocampus Core
//!
//! Agent 记忆库的核心逻辑库，无 IO 依赖。
//!
//! ## 模块组织
//!
//! - [`model`]：核心数据模型（记忆文件、索引钩子、标签等）
//! - [`archive`]：归档/冻结逻辑（达到阈值时将上下文冻结为记忆文件）
//! - [`retrieve`]：检索机制（摘要钩子注入 + tool 主动检索）
//! - [`compact`]：周期任务（周去重合并 / 月评分淘汰）
//! - [`score`]：评分 trait + 默认启发式实现
//! - [`storage`]：存储后端 trait + 默认本地文件树实现
//! - [`sqlite`]：SQLite 存储后端（rusqlite + r2d2 连接池 + WAL 模式）
//! - [`serialization`]：序列化格式（JSON / MessagePack 双格式支持）
//! - [`migrator`]：Schema 版本迁移
//! - [`cache`]：缓存装饰器（CachedStorage<T>，moka LRU + TTL）
//!
//! ## 索引管理职责分配
//!
//! 「索引文档」与「索引钩子」的职责由多个模块共同承担，不设独立的 IndexManager：
//! - **数据模型**：[`model::IndexDocument`] / [`model::IndexHook`]
//! - **持久化**：[`storage::Storage`] trait 的 `append_hook` / `read_index` / `write_index`
//! - **摘要渲染**：[`retrieve::Retriever`] 的 `render_to_system_prompt`
//! - **钩子检索**：[`retrieve::Retriever`] 的 `retrieve_memory`
//! - **周期合并**：[`compact::Compactor`] 的 `weekly_merge` / `monthly_evict`（钩子迁移）
//!
//! ## 核心概念
//!
//! - **归档（freeze）**：达到 token 阈值时，将完整上下文（用户消息+LLM消息）保存为记忆文件，非摘要
//! - **索引钩子（hook）**：指向记忆库中记忆文件的指针，带 17 类细粒度标签
//! - **三级周期**：天级归档 / 周级无损去重合并 / 月级评分淘汰

#![forbid(unsafe_code)]
#![warn(missing_docs, rust_2018_idioms)]

pub mod archive;
/// BM25 关键词检索（jieba-rs 中文分词 + 倒排索引）
pub mod bm25;
/// 缓存装饰器（CachedStorage<T>，moka LRU + TTL）
pub mod cache;
/// 记忆冲突检测（ConflictDetector trait + NoopDetector）
pub mod conflict;
pub mod compact;
/// 混合检索器（HybridRetriever + RRF 融合 + 降级策略）
pub mod hybrid;
/// 启发式冲突检测器（HeuristicDetector，反义词词典 + 三维度检测）
pub mod heuristic;
/// 语义检索（Embedder / KeywordSearcher / VectorIndex / SemanticRetriever trait + RRF 融合）
pub mod semantic;
/// 序列化格式（JSON / MessagePack 双格式支持）
pub mod serialization;
pub mod migrator;
pub mod model;
pub mod retrieve;
pub mod score;
/// SQLite 存储后端（rusqlite + r2d2 连接池 + WAL 模式）
pub mod sqlite;
/// SQLite 向量索引（BLOB 持久化 + InMemoryVectorIndex 缓存）
pub mod sqlite_vector;
pub mod storage;
/// 向量索引（InMemoryVectorIndex + cosine_similarity）
pub mod vector;

/// Crate 级错误类型
#[derive(Debug, Clone, thiserror::Error)]
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
