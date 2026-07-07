//! # Schema 迁移模块
//!
//! 记忆文件格式未来演进时的迁移支持。
//!
//! ## 设计
//!
//! - 记忆文件头包含 `schema_version` 字段
//! - 提供 [`Migrator`] trait，支持从旧版本迁移到新版本
//! - 已归档的旧格式文件可按需迁移
//!
//! ## 迁移策略
//!
//! - 读取时检测版本，若旧版本则按需迁移
//! - 迁移可配置为：惰性（读取时）/ 主动（批量）
//! - 迁移是单向的（旧→新），不可逆
//!
//! TODO: 后续阶段实现（当 schema 演进时）

use crate::model::MemoryFile;

/// 迁移器 trait
pub trait Migrator: Send + Sync {
    /// 迁移记忆文件到最新版本
    fn migrate(&self, file: &mut MemoryFile) -> crate::Result<()>;

    /// 当前支持的 schema 版本
    fn current_version(&self) -> u32 {
        crate::model::SCHEMA_VERSION
    }
}

/// 默认迁移器（当前版本无需迁移）
pub struct DefaultMigrator;

impl Migrator for DefaultMigrator {
    fn migrate(&self, file: &mut MemoryFile) -> crate::Result<()> {
        if file.schema_version == crate::model::SCHEMA_VERSION {
            Ok(())
        } else {
            // 未来版本演进时在此添加迁移逻辑
            Err(crate::Error::Migrate(format!(
                "不支持的 schema 版本: {} (当前: {})",
                file.schema_version,
                crate::model::SCHEMA_VERSION
            )))
        }
    }
}
