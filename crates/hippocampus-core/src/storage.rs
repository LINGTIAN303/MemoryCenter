//! # 存储模块
//!
//! 可插拔存储后端 trait。
//!
//! ## 设计
//!
//! - [`Storage`] trait：存储后端接口，可插拔
//! - [`LocalStorage`]：默认实现，本地文件树结构
//! - 单写多读（写入串行化），WAL 留作 v2
//!
//! ## 记忆库文件树结构
//!
//! ```text
//! memory_store/
//! ├── sessions/
//! │   └── {session_id}/
//! │       ├── daily/
//! │       │   ├── 2026-07-02_001.json      # 天级记忆文件
//! │       │   └── 2026-07-02_002.json
//! │       ├── weekly/
//! │       │   └── 2026-W27.json            # 周级合并文件
//! │       ├── monthly/
//! │       │   └── 2026-07.json             # 月级主记忆文件
//! │       └── index/
//! │           ├── daily_index.json         # 天级索引文档
//! │           ├── weekly_index.json        # 周级索引文档
//! │           └── monthly_index.json       # 月级索引文档
//! └── projects/
//!     └── {project_id}/
//!         └── ... (同 sessions 结构)
//! ```
//!
//! TODO: P1/P2 阶段实现

use crate::model::{IndexDocument, MemoryFile};

/// 存储后端 trait
///
/// 所有存储后端（本地文件树、SQLite、S3 等）需实现此 trait。
/// 单写多读：写入操作串行化，读取操作可并发。
#[async_trait::async_trait]
pub trait Storage: Send + Sync {
    /// 写入记忆文件，返回存储路径
    async fn write_memory(&self, file: &MemoryFile) -> crate::Result<String>;

    /// 读取记忆文件
    async fn read_memory(&self, path: &str) -> crate::Result<MemoryFile>;

    /// 删除记忆文件
    async fn delete_memory(&self, path: &str) -> crate::Result<()>;

    /// 写入索引文档
    async fn write_index(&self, doc: &IndexDocument) -> crate::Result<String>;

    /// 读取索引文档
    async fn read_index(&self, path: &str) -> crate::Result<IndexDocument>;

    /// 列出指定会话/项目下某周期层级的所有记忆文件
    async fn list_memories(
        &self,
        session_id: &str,
        period: crate::model::ArchivePeriod,
    ) -> crate::Result<Vec<String>>;
}

/// 本地文件树存储后端
///
/// 将记忆文件以 JSON 格式存储在本地文件系统中。
/// 文件树结构见模块文档。
///
/// TODO: P1 阶段实现
pub struct LocalStorage {
    /// 根目录路径
    root: std::path::PathBuf,
}

impl LocalStorage {
    /// 创建新的本地存储后端
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// 根目录
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }
}

#[async_trait::async_trait]
impl Storage for LocalStorage {
    async fn write_memory(&self, _file: &MemoryFile) -> crate::Result<String> {
        Err(crate::Error::Storage("write_memory() 待 P1 实现".into()))
    }

    async fn read_memory(&self, _path: &str) -> crate::Result<MemoryFile> {
        Err(crate::Error::Storage("read_memory() 待 P1 实现".into()))
    }

    async fn delete_memory(&self, _path: &str) -> crate::Result<()> {
        Err(crate::Error::Storage("delete_memory() 待 P1 实现".into()))
    }

    async fn write_index(&self, _doc: &IndexDocument) -> crate::Result<String> {
        Err(crate::Error::Storage("write_index() 待 P1 实现".into()))
    }

    async fn read_index(&self, _path: &str) -> crate::Result<IndexDocument> {
        Err(crate::Error::Storage("read_index() 待 P1 实现".into()))
    }

    async fn list_memories(
        &self,
        _session_id: &str,
        _period: crate::model::ArchivePeriod,
    ) -> crate::Result<Vec<String>> {
        Err(crate::Error::Storage("list_memories() 待 P1 实现".into()))
    }
}
