//! # 周期任务模块
//!
//! 实现三级索引周期任务：
//!
//! - **天级**：持续归档（由 [`archive`] 模块处理）
//! - **周级**：无损去重合并（7 个天级文件 → 1 个周级文件）
//! - **月级**：评分淘汰（4 个周级文件 → 1 个主记忆文件 + 高价值片段）
//!
//! ## 周级合并
//!
//! 7 个天级记忆文件**无损去重合并**为 1 个周级文件：
//! - 去除重复信息 / 无效寒暄
//! - 保留所有实质内容原样拼接
//! - 索引文档同步合并
//!
//! ## 月级评分淘汰
//!
//! 4 个周级记忆文件按 4 维加权评分：
//! - 时效性（需要语义理解，LLM 可插拔）
//! - 访问频率（纯算法）
//! - 主题相关性（需要语义理解，LLM 可插拔）
//! - 用户显式标记（纯算法）
//!
//! 选最高分为主记忆，其余高价值片段保留到主记忆，索引同步合并。
//!
//! TODO: P3 阶段实现

use crate::model::{ArchivePeriod, IndexDocument, MemoryFile};
use crate::score::Scorer;

/// 周期任务执行器
#[allow(dead_code)]
pub struct Compactor {
    scorer: Box<dyn Scorer>,
}

impl Compactor {
    /// 创建新的周期任务执行器
    pub fn new(scorer: Box<dyn Scorer>) -> Self {
        Self { scorer }
    }

    /// 周级合并：7 个天级文件无损去重合并为 1 个周级文件
    ///
    /// TODO: P3 阶段实现
    pub fn weekly_merge(&self, _daily_files: Vec<MemoryFile>) -> crate::Result<MemoryFile> {
        Err(crate::Error::Storage("weekly_merge() 待 P3 实现".into()))
    }

    /// 月级评分淘汰：4 个周级文件 → 1 个主记忆 + 高价值片段
    ///
    /// TODO: P3 阶段实现
    pub fn monthly_evict(&self, _weekly_files: Vec<MemoryFile>) -> crate::Result<MemoryFile> {
        Err(crate::Error::Storage("monthly_evict() 待 P3 实现".into()))
    }

    /// 同步合并索引文档
    ///
    /// TODO: P3 阶段实现
    pub fn merge_index(&self, _period: ArchivePeriod, _docs: Vec<IndexDocument>) -> crate::Result<IndexDocument> {
        Err(crate::Error::Index("merge_index() 待 P3 实现".into()))
    }
}
