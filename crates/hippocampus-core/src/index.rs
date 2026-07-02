//! # 索引模块
//!
//! 管理索引文档与索引钩子的生命周期。
//!
//! ## 职责
//!
//! - 创建/更新索引文档（[`IndexDocument`]
//! - 添加/删除索引钩子（[`IndexHook`]
//! - 按周期维护索引文档（天级追加 / 周级合并 / 月级评分淘汰后合并）
//! - 维护索引文档与记忆库的一致性
//!
//! TODO: P2 阶段实现

use crate::model::{ArchivePeriod, IndexDocument, IndexHook};

/// 索引管理器
pub struct IndexManager {
    /// 当前活跃的索引文档
    current: Option<IndexDocument>,
}

impl IndexManager {
    /// 创建新的索引管理器
    pub fn new() -> Self {
        Self { current: None }
    }

    /// 添加索引钩子到当前索引文档
    ///
    /// TODO: P2 阶段实现
    pub fn add_hook(&mut self, _hook: IndexHook) -> crate::Result<()> {
        Err(crate::Error::Index("add_hook() 待 P2 实现".into()))
    }

    /// 获取当前索引文档引用
    pub fn current(&self) -> Option<&IndexDocument> {
        self.current.as_ref()
    }

    /// 按周期合并索引文档（周级 / 月级）
    ///
    /// TODO: P3 阶段实现
    pub fn merge_by_period(&mut self, _period: ArchivePeriod) -> crate::Result<()> {
        Err(crate::Error::Index("merge_by_period() 待 P3 实现".into()))
    }
}

impl Default for IndexManager {
    fn default() -> Self {
        Self::new()
    }
}
