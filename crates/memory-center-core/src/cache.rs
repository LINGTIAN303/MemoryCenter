//! # 缓存层（CachedStorage）
//!
//! 装饰器模式：包装任意 Storage 后端，提供 LRU + TTL 缓存能力。
//!
//! ## 设计
//!
//! - [`CachedStorage<T>`]：泛型装饰器，实现 `Storage` trait
//! - 用 [`moka`] crate 的高性能并发缓存（无锁、线程安全）
//! - write-through 策略：写入同时更新缓存
//! - 读取时先查缓存，未命中则查底层并填充
//! - 写入/删除/更新时同步失效对应缓存
//!
//! ## 缓存层级
//!
//! | 缓存 | Key | Value | 容量占比 |
//! |------|-----|-------|----------|
//! | 记忆文件 | `memory:{memory_id}` | `MemoryFile` | 100% |
//! | 会话索引 | `sidx:{session}:{project}:{period}` | `IndexDocument` | 10% |
//! | 项目索引 | `pidx:{project}:{period}` | `IndexDocument` | 10% |
//!
//! ## 使用方式
//!
//! ```rust,ignore
//! use memory_center_core::storage::LocalStorage;
//! use memory_center_core::cache::{CachedStorage, CacheConfig};
//!
//! let local = LocalStorage::new("./data");
//! let cached = CachedStorage::new(local);
//! // cached 实现了 Storage trait，可直接替代原后端
//! ```
//!
//! ## 缓存策略说明
//!
//! - **write_memory**：写底层 → 更新缓存（write-through）
//! - **read_memory**：先查缓存 → 未命中查底层 → 填充缓存
//! - **delete_memory**：删底层 → 失效缓存
//! - **update_memory**：更新底层 → 失效缓存（下次读时重新加载，确保看到 updates 字段）
//! - **write_index**：写底层 → 失效旧缓存 + 插入新文档
//! - **append_hook**：调底层 → 失效对应索引缓存（内容已变）
//! - **list_memories**：不缓存（结果可能变化，且调用频率低）

use crate::model::{ArchivePeriod, IndexDocument, IndexHook, MemoryFile, MemoryUpdate};
use crate::storage::{SessionMeta, Storage};
use moka::future::Cache;
use std::sync::Arc;
use std::time::Duration;

/// 缓存配置
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// 最大缓存条目数（默认 1000，记忆文件层）
    pub capacity: u64,
    /// TTL 过期时间（秒，默认 3600 = 1 小时）
    pub ttl_secs: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            capacity: 1000,
            ttl_secs: 3600,
        }
    }
}

impl CacheConfig {
    /// 创建配置
    pub fn new(capacity: u64, ttl_secs: u64) -> Self {
        Self { capacity, ttl_secs }
    }
}

/// 缓存键生成工具
///
/// 用字符串作为缓存 key，确保唯一性和可读性。
struct CacheKey;

impl CacheKey {
    /// 记忆文件缓存键
    fn memory(memory_id: &str) -> String {
        format!("memory:{}", memory_id)
    }

    /// 会话索引缓存键
    fn session_index(
        session_id: &str,
        project_id: Option<&str>,
        period: ArchivePeriod,
    ) -> String {
        format!(
            "sidx:{}:{}:{}",
            session_id,
            project_id.unwrap_or("_"),
            period.as_str()
        )
    }

    /// 项目索引缓存键
    fn project_index(project_id: &str, period: ArchivePeriod) -> String {
        format!("pidx:{}:{}", project_id, period.as_str())
    }
}

/// 缓存统计信息
#[derive(Debug, Clone)]
pub struct CacheStats {
    /// 记忆文件缓存条目数
    pub memory_entries: u64,
    /// 会话索引缓存条目数
    pub session_index_entries: u64,
    /// 项目索引缓存条目数
    pub project_index_entries: u64,
}

/// 缓存装饰器
///
/// 包装任意 `Storage` 后端，提供 LRU + TTL 缓存。
/// 实现 `Storage` trait，可透明替换原后端。
///
/// ## 泛型参数
///
/// - `T: Storage`：底层存储后端类型（如 `LocalStorage` / `SqliteStorage`）
///
/// ## 线程安全
///
/// 内部用 `moka::future::Cache` 并发缓存 + `Arc<T>` 共享底层，线程安全。
pub struct CachedStorage<T: Storage> {
    /// 底层存储后端（Arc 共享，因为装饰器可能被 clone 到多个 task）
    inner: Arc<T>,
    /// 记忆文件缓存（热点记忆，LRU + TTL）
    memory_cache: Cache<String, MemoryFile>,
    /// 会话索引缓存（索引文档数量远少于记忆文件，容量按 10% 分配）
    session_index_cache: Cache<String, IndexDocument>,
    /// 项目索引缓存
    project_index_cache: Cache<String, IndexDocument>,
}

impl<T: Storage> CachedStorage<T> {
    /// 创建缓存装饰器（默认配置：容量 1000，TTL 1 小时）
    pub fn new(storage: T) -> Self {
        Self::with_config(storage, CacheConfig::default())
    }

    /// 创建缓存装饰器（自定义配置）
    pub fn with_config(storage: T, config: CacheConfig) -> Self {
        let ttl = Duration::from_secs(config.ttl_secs);
        // 索引缓存容量按记忆缓存的 10% 分配（索引数量远少于记忆文件）
        let index_capacity = (config.capacity / 10).max(10);

        let memory_cache = Cache::builder()
            .max_capacity(config.capacity)
            .time_to_live(ttl)
            .build();

        let session_index_cache = Cache::builder()
            .max_capacity(index_capacity)
            .time_to_live(ttl)
            .build();

        let project_index_cache = Cache::builder()
            .max_capacity(index_capacity)
            .time_to_live(ttl)
            .build();

        Self {
            inner: Arc::new(storage),
            memory_cache,
            session_index_cache,
            project_index_cache,
        }
    }

    /// 获取底层存储引用（用于需要直接访问底层的场景）
    pub fn inner(&self) -> &T {
        &self.inner
    }

    /// 清空所有缓存（不影底层数据）
    ///
    /// 用于需要强制刷新缓存的场景（如手动触发数据同步后）。
    pub fn invalidate_all(&self) {
        self.memory_cache.invalidate_all();
        self.session_index_cache.invalidate_all();
        self.project_index_cache.invalidate_all();
    }

    /// 失效指定 memory_id 的缓存
    pub async fn invalidate_memory(&self, memory_id: &str) {
        self.memory_cache
            .invalidate(&CacheKey::memory(memory_id))
            .await;
    }

    /// 失效指定会话索引的缓存
    pub async fn invalidate_session_index(
        &self,
        session_id: &str,
        project_id: Option<&str>,
        period: ArchivePeriod,
    ) {
        let key = CacheKey::session_index(session_id, project_id, period);
        self.session_index_cache.invalidate(&key).await;
    }

    /// 获取缓存统计信息
    ///
    /// **注意**：此方法会调用 `run_pending_tasks()` 强制同步 moka 内部待处理任务，
    /// 确保 `entry_count()` 返回最新值。仅用于测试/调试，不要在热点路径调用。
    pub async fn stats(&self) -> CacheStats {
        // moka 的 insert 是惰性提交的，entry_count() 可能不反映刚插入的条目。
        // 调用 run_pending_tasks() 强制同步，确保统计准确。
        self.memory_cache.run_pending_tasks().await;
        self.session_index_cache.run_pending_tasks().await;
        self.project_index_cache.run_pending_tasks().await;
        CacheStats {
            memory_entries: self.memory_cache.entry_count(),
            session_index_entries: self.session_index_cache.entry_count(),
            project_index_entries: self.project_index_cache.entry_count(),
        }
    }
}

#[async_trait::async_trait]
impl<T: Storage> Storage for CachedStorage<T> {
    async fn write_memory(&self, file: &MemoryFile) -> crate::Result<String> {
        let memory_id = self.inner.write_memory(file).await?;
        // write-through：写入成功后更新缓存
        self.memory_cache
            .insert(CacheKey::memory(&memory_id), file.clone())
            .await;
        Ok(memory_id)
    }

    async fn read_memory(&self, memory_id: &str) -> crate::Result<MemoryFile> {
        let key = CacheKey::memory(memory_id);
        // 先查缓存
        if let Some(cached) = self.memory_cache.get(&key).await {
            tracing::trace!(memory_id = %memory_id, "缓存命中：read_memory");
            return Ok(cached);
        }
        // 未命中，查底层
        let file = self.inner.read_memory(memory_id).await?;
        // 填充缓存
        self.memory_cache.insert(key, file.clone()).await;
        Ok(file)
    }

    async fn delete_memory(&self, memory_id: &str) -> crate::Result<()> {
        self.inner.delete_memory(memory_id).await?;
        // 失效缓存
        self.memory_cache
            .invalidate(&CacheKey::memory(memory_id))
            .await;
        Ok(())
    }

    async fn write_index(&self, doc: &IndexDocument) -> crate::Result<String> {
        let doc_id = self.inner.write_index(doc).await?;
        // 失效对应会话索引缓存（全量覆盖写，旧缓存失效）
        let key = CacheKey::session_index(&doc.session_id, doc.project_id.as_deref(), doc.period);
        self.session_index_cache.invalidate(&key).await;
        // 同时插入新文档到缓存
        self.session_index_cache.insert(key, doc.clone()).await;
        Ok(doc_id)
    }

    async fn read_index(
        &self,
        session_id: &str,
        project_id: Option<&str>,
        period: ArchivePeriod,
    ) -> crate::Result<Option<IndexDocument>> {
        let key = CacheKey::session_index(session_id, project_id, period);
        // 先查缓存
        if let Some(cached) = self.session_index_cache.get(&key).await {
            tracing::trace!(session_id = %session_id, period = ?period, "缓存命中：read_index");
            return Ok(Some(cached));
        }
        // 未命中，查底层
        let doc = self.inner.read_index(session_id, project_id, period).await?;
        // 填充缓存（仅当文档存在时）
        if let Some(ref d) = doc {
            self.session_index_cache.insert(key, d.clone()).await;
        }
        Ok(doc)
    }

    async fn append_hook(
        &self,
        session_id: &str,
        project_id: Option<&str>,
        period: ArchivePeriod,
        hook: IndexHook,
    ) -> crate::Result<()> {
        self.inner
            .append_hook(session_id, project_id, period, hook)
            .await?;
        // 失效对应会话索引缓存（内容已变，下次读时重新加载）
        let key = CacheKey::session_index(session_id, project_id, period);
        self.session_index_cache.invalidate(&key).await;
        Ok(())
    }

    async fn list_memories(
        &self,
        session_id: &str,
        project_id: Option<&str>,
        period: ArchivePeriod,
    ) -> crate::Result<Vec<String>> {
        // 不缓存列表查询（结果可能频繁变化，且调用频率低）
        self.inner.list_memories(session_id, project_id, period).await
    }

    // ========================================================================
    // project 层聚合索引
    // ========================================================================

    async fn read_project_index(
        &self,
        project_id: &str,
        period: ArchivePeriod,
    ) -> crate::Result<Option<IndexDocument>> {
        let key = CacheKey::project_index(project_id, period);
        // 先查缓存
        if let Some(cached) = self.project_index_cache.get(&key).await {
            tracing::trace!(project_id = %project_id, period = ?period, "缓存命中：read_project_index");
            return Ok(Some(cached));
        }
        // 未命中，查底层
        let doc = self.inner.read_project_index(project_id, period).await?;
        // 填充缓存
        if let Some(ref d) = doc {
            self.project_index_cache.insert(key, d.clone()).await;
        }
        Ok(doc)
    }

    async fn append_project_hook(
        &self,
        project_id: &str,
        period: ArchivePeriod,
        hook: IndexHook,
    ) -> crate::Result<()> {
        self.inner.append_project_hook(project_id, period, hook).await?;
        // 失效对应项目索引缓存
        let key = CacheKey::project_index(project_id, period);
        self.project_index_cache.invalidate(&key).await;
        Ok(())
    }

    async fn list_project_memories(
        &self,
        project_id: &str,
        period: ArchivePeriod,
    ) -> crate::Result<Vec<String>> {
        // 不缓存列表查询
        self.inner.list_project_memories(project_id, period).await
    }

    // ========================================================================
    // 记忆迭代更新
    // ========================================================================

    async fn update_memory(
        &self,
        memory_id: &str,
        updates: MemoryUpdate,
    ) -> crate::Result<()> {
        // 委托给 update_memory_with_conflicts（传空 conflicts）
        self.update_memory_with_conflicts(memory_id, updates, vec![]).await
    }

    async fn update_memory_with_conflicts(
        &self,
        memory_id: &str,
        updates: MemoryUpdate,
        conflicts: Vec<crate::conflict::ConflictRecord>,
    ) -> crate::Result<()> {
        self.inner
            .update_memory_with_conflicts(memory_id, updates, conflicts)
            .await?;
        // 失效缓存（下次读取时从底层重新加载，确保看到 updates 字段的变化）
        self.memory_cache
            .invalidate(&CacheKey::memory(memory_id))
            .await;
        Ok(())
    }

    /// 透传 session 元数据写入到 inner（v2.33 新增）
    ///
    /// CachedStorage 不单独缓存 session_meta（读取频率低，每个 session 仅首次 archive 时读一次）。
    async fn write_session_meta(
        &self,
        session_id: &str,
        meta: &SessionMeta,
    ) -> crate::Result<()> {
        self.inner.write_session_meta(session_id, meta).await
    }

    /// 透传 session 元数据读取到 inner（v2.33 新增）
    async fn read_session_meta(
        &self,
        session_id: &str,
    ) -> crate::Result<Option<SessionMeta>> {
        self.inner.read_session_meta(session_id).await
    }

    // ========================================================================
    // raw_context 原始上下文（v2.34 新增，透传给 inner）
    // ========================================================================

    /// 透传 raw_context 写入到 inner（v2.34 新增）
    ///
    /// CachedStorage 不单独缓存 raw_context（体积大，且仅在压缩后重建时读取一次）。
    async fn write_raw_context(
        &self,
        session_id: &str,
        hook_id: &str,
        content: &str,
    ) -> crate::Result<String> {
        self.inner
            .write_raw_context(session_id, hook_id, content)
            .await
    }

    /// 透传 raw_context 读取到 inner（v2.34 新增）
    async fn read_raw_context(
        &self,
        session_id: &str,
        hook_id: &str,
    ) -> crate::Result<String> {
        self.inner.read_raw_context(session_id, hook_id).await
    }

    /// 透传 raw_context 删除到 inner（v2.34 新增）
    async fn delete_raw_context(
        &self,
        session_id: &str,
        hook_id: &str,
    ) -> crate::Result<()> {
        self.inner.delete_raw_context(session_id, hook_id).await
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        ArchiveConfig, ArchivePeriod, IndexHook, MemoryFile, MessageContent,
        MessageTurn, Tag,
    };
    use crate::storage::LocalStorage;
    use chrono::Utc;
    use tempfile::TempDir;
    use uuid::Uuid;

    /// 构造测试用 MessageTurn
    fn make_turn(text: &str, token_count: usize) -> MessageTurn {
        MessageTurn {
            id: Uuid::new_v4(),
            user_message: MessageContent {
                text: Some(text.into()),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                thinking: None,
                file_changes: Vec::new(),
            },
            llm_message: MessageContent {
                text: Some("LLM 回复".into()),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                thinking: None,
                file_changes: Vec::new(),
            },
            tags: vec![Tag::Text],
            timestamp: Utc::now(),
            token_count,
            stop_reason: None,
            cost: None,
        }
    }

    /// 构造测试用 MemoryFile
    fn make_memory(session_id: &str, project_id: Option<&str>) -> MemoryFile {
        let turn = make_turn("测试消息", 100);
        MemoryFile::new(
            String::from(session_id),
            project_id.map(String::from),
            vec![turn],
            ArchivePeriod::Daily,
        )
    }

    /// 构造测试用 IndexHook（手动构造，避免依赖 MemoryFile）
    ///
    /// IndexHook 没有 new() 构造函数，只有 from_memory_file()。
    /// 测试场景下需要自定义 memory_id / tags / token_count，手动构造更直观。
    fn make_hook(
        memory_id: &str,
        title: &str,
        tags: Vec<Tag>,
        period: ArchivePeriod,
        token_count: usize,
    ) -> IndexHook {
        IndexHook {
            id: Uuid::new_v4(),
            memory_id: memory_id.to_string(),
            summary: crate::model::Summary::from_title(title),
            tags,
            archived_at: Utc::now(),
            period,
            token_count,
            file_status: crate::model::FileStatus::Normal,
            // v2.34：测试辅助函数默认 None
            archive_reason: None,
            raw_context_path: None,
        }
    }

    /// 测试：缓存命中（read_memory 第二次应命中缓存）
    #[tokio::test]
    async fn test_cache_hit_read_memory() {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());
        let cached = CachedStorage::new(storage);

        let memory = make_memory("sess-1", None);
        let memory_id = cached.write_memory(&memory).await.unwrap();

        // 第一次读取（可能命中 write-through 插入的缓存）
        let _first = cached.read_memory(&memory_id).await.unwrap();
        // 第二次读取（应命中缓存）
        let second = cached.read_memory(&memory_id).await.unwrap();

        assert_eq!(second.session_id, "sess-1");
        assert_eq!(second.turns.len(), 1);
        // 缓存应有 1 条
        assert_eq!(cached.stats().await.memory_entries, 1);
    }

    /// 测试：写入后缓存更新（write-through）
    #[tokio::test]
    async fn test_write_through_cache() {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());
        let cached = CachedStorage::new(storage);

        let memory = make_memory("sess-wt", None);
        let memory_id = cached.write_memory(&memory).await.unwrap();

        // write-through：写入后缓存应立即有数据
        assert_eq!(cached.stats().await.memory_entries, 1);

        // 读取应命中缓存
        let read = cached.read_memory(&memory_id).await.unwrap();
        assert_eq!(read.session_id, "sess-wt");
    }

    /// 测试：删除后缓存失效
    #[tokio::test]
    async fn test_delete_invalidates_cache() {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());
        let cached = CachedStorage::new(storage);

        let memory = make_memory("sess-del", None);
        let memory_id = cached.write_memory(&memory).await.unwrap();
        assert_eq!(cached.stats().await.memory_entries, 1);

        // 删除
        cached.delete_memory(&memory_id).await.unwrap();

        // 缓存应被失效
        assert_eq!(cached.stats().await.memory_entries, 0);

        // 再次读取应失败（底层已删除）
        let result = cached.read_memory(&memory_id).await;
        assert!(result.is_err());
    }

    /// 测试：update_memory 后缓存失效
    #[tokio::test]
    async fn test_update_invalidates_cache() {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());
        let cached = CachedStorage::new(storage);

        let memory = make_memory("sess-upd", None);
        let memory_id = cached.write_memory(&memory).await.unwrap();

        // 读取填充缓存
        let _ = cached.read_memory(&memory_id).await.unwrap();
        assert_eq!(cached.stats().await.memory_entries, 1);

        // 更新记忆
        let updates = MemoryUpdate::new().add_fact("新事实");
        cached.update_memory(&memory_id, updates).await.unwrap();

        // 缓存应被失效（下次读取时重新从底层加载）
        assert_eq!(cached.stats().await.memory_entries, 0);

        // 再次读取应看到更新
        let updated = cached.read_memory(&memory_id).await.unwrap();
        assert_eq!(updated.updates.len(), 1);
        assert_eq!(updated.updates[0].update.added_facts[0], "新事实");
    }

    /// 测试：read_index 缓存
    #[tokio::test]
    async fn test_cache_read_index() {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());
        let cached = CachedStorage::new(storage);

        // 创建索引文档并写入
        let hook = make_hook(
            "memory-test-1",
            "测试摘要标题",
            vec![Tag::Text],
            ArchivePeriod::Daily,
            100,
        );
        cached
            .append_hook("sess-idx", None, ArchivePeriod::Daily, hook)
            .await
            .unwrap();

        // 第一次读取（未命中，查底层并填充缓存）
        let doc1 = cached
            .read_index("sess-idx", None, ArchivePeriod::Daily)
            .await
            .unwrap();
        assert!(doc1.is_some());
        assert_eq!(doc1.as_ref().unwrap().hooks.len(), 1);

        // 第二次读取（应命中缓存）
        let doc2 = cached
            .read_index("sess-idx", None, ArchivePeriod::Daily)
            .await
            .unwrap();
        assert!(doc2.is_some());
        assert_eq!(doc2.as_ref().unwrap().hooks.len(), 1);

        // 会话索引缓存应有 1 条
        assert_eq!(cached.stats().await.session_index_entries, 1);
    }

    /// 测试：append_hook 失效索引缓存
    #[tokio::test]
    async fn test_append_hook_invalidates_cache() {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());
        let cached = CachedStorage::new(storage);

        // 写入第一个 hook
        let hook1 = make_hook(
            "memory-1",
            "摘要1",
            vec![Tag::Text],
            ArchivePeriod::Daily,
            100,
        );
        cached
            .append_hook("sess-ah", None, ArchivePeriod::Daily, hook1)
            .await
            .unwrap();

        // 读取填充缓存
        let _ = cached
            .read_index("sess-ah", None, ArchivePeriod::Daily)
            .await
            .unwrap();

        // 追加第二个 hook（应失效缓存）
        let hook2 = make_hook(
            "memory-2",
            "摘要2",
            vec![Tag::CodeBlock],
            ArchivePeriod::Daily,
            200,
        );
        cached
            .append_hook("sess-ah", None, ArchivePeriod::Daily, hook2)
            .await
            .unwrap();

        // 再次读取应看到 2 个 hook（缓存已失效，从底层重新加载）
        let doc = cached
            .read_index("sess-ah", None, ArchivePeriod::Daily)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(doc.hooks.len(), 2);
    }

    /// 测试：invalidate_all 清空所有缓存
    #[tokio::test]
    async fn test_invalidate_all() {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());
        let cached = CachedStorage::new(storage);

        // 写入数据填充缓存
        let memory = make_memory("sess-ia", None);
        let memory_id = cached.write_memory(&memory).await.unwrap();
        let _ = cached.read_memory(&memory_id).await.unwrap();

        let hook = make_hook(
            "memory-ia",
            "摘要",
            vec![Tag::Text],
            ArchivePeriod::Daily,
            100,
        );
        cached
            .append_hook("sess-ia", None, ArchivePeriod::Daily, hook)
            .await
            .unwrap();
        let _ = cached
            .read_index("sess-ia", None, ArchivePeriod::Daily)
            .await
            .unwrap();

        // 验证缓存有数据
        assert!(cached.stats().await.memory_entries > 0);
        assert!(cached.stats().await.session_index_entries > 0);

        // 清空所有缓存
        cached.invalidate_all();

        // 缓存应清空
        assert_eq!(cached.stats().await.memory_entries, 0);
        assert_eq!(cached.stats().await.session_index_entries, 0);
    }

    /// 测试：自定义配置
    #[tokio::test]
    async fn test_custom_config() {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());
        let config = CacheConfig::new(100, 60); // 容量 100，TTL 60s
        let cached = CachedStorage::with_config(storage, config);

        let memory = make_memory("sess-cfg", None);
        let _ = cached.write_memory(&memory).await.unwrap();

        assert_eq!(cached.stats().await.memory_entries, 1);
    }

    /// 测试：并发读写缓存安全
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_concurrent_cache_safety() {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());
        let cached = Arc::new(CachedStorage::new(storage));

        // 预置一个记忆
        let memory = make_memory("sess-conc", None);
        let memory_id = cached.write_memory(&memory).await.unwrap();
        let memory_id = memory_id.clone();

        // 10 个并发读 + 2 个并发更新
        let mut handles = Vec::new();

        // 10 个并发读取
        for _ in 0..10 {
            let c = cached.clone();
            let mid = memory_id.clone();
            handles.push(tokio::spawn(async move {
                let m = c.read_memory(&mid).await.unwrap();
                assert_eq!(m.session_id, "sess-conc");
            }));
        }

        // 2 个并发更新
        for i in 0..2 {
            let c = cached.clone();
            let mid = memory_id.clone();
            handles.push(tokio::spawn(async move {
                let updates = MemoryUpdate::new().add_fact(format!("并发事实 #{}", i));
                c.update_memory(&mid, updates).await.unwrap();
            }));
        }

        // 所有任务应无 panic 完成
        for handle in handles {
            handle.await.unwrap();
        }

        // 最终读取应看到 2 条更新（LocalStorage 有 DashMap 写锁保护）
        let final_memory = cached.read_memory(&memory_id).await.unwrap();
        assert_eq!(final_memory.updates.len(), 2);
    }

    /// 测试：与 LocalStorage 组合，端到端归档流程
    #[tokio::test]
    async fn test_end_to_end_with_archiver() {
        use crate::archive::Archiver;

        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());
        let cached = Arc::new(CachedStorage::new(storage));

        // 用 CachedStorage 归档
        let config = ArchiveConfig::default();
        let mut archiver = Archiver::new(config, cached.clone(), "sess-e2e", None);
        for i in 0..3 {
            archiver.push_turn(make_turn(&format!("消息{}", i), 100));
        }
        let (_memory_file, hook) = archiver.archive().await.unwrap();
        // hook.memory_id 是底层存储返回的 memory_id（路径或 UUID），用它读取
        let memory_id = hook.memory_id;

        // 通过缓存读取（应命中 write-through 插入的缓存）
        let read1 = cached.read_memory(&memory_id).await.unwrap();
        assert_eq!(read1.turns.len(), 3);

        // 再次读取（应命中缓存）
        let read2 = cached.read_memory(&memory_id).await.unwrap();
        assert_eq!(read2.turns.len(), 3);

        // 缓存应有 1 条（write_memory 时插入）
        assert!(cached.stats().await.memory_entries > 0);
    }

    /// 测试：与 SqliteStorage 组合
    #[tokio::test]
    async fn test_with_sqlite_storage() {
        use crate::sqlite::SqliteStorage;

        let tmp = TempDir::new().unwrap();
        let storage = SqliteStorage::new(tmp.path(), None).unwrap();
        let cached = CachedStorage::new(storage);

        let memory = make_memory("sess-sql", None);
        let memory_id = cached.write_memory(&memory).await.unwrap();

        // 读取（应命中 write-through 缓存）
        let read = cached.read_memory(&memory_id).await.unwrap();
        assert_eq!(read.session_id, "sess-sql");

        // 更新
        let updates = MemoryUpdate::new().add_fact("SQLite 缓存测试");
        cached.update_memory(&memory_id, updates).await.unwrap();

        // 读取应看到更新（缓存已失效，从底层重新加载）
        let updated = cached.read_memory(&memory_id).await.unwrap();
        assert_eq!(updated.updates.len(), 1);
    }

    /// 测试：TTL 过期（用极短 TTL 验证）
    #[tokio::test]
    async fn test_ttl_expiry() {
        let tmp = TempDir::new().unwrap();
        let storage = LocalStorage::new(tmp.path());
        // TTL = 1 秒
        let config = CacheConfig::new(100, 1);
        let cached = CachedStorage::with_config(storage, config);

        let memory = make_memory("sess-ttl", None);
        let memory_id = cached.write_memory(&memory).await.unwrap();

        // 立即读取（应命中缓存）
        let _ = cached.read_memory(&memory_id).await.unwrap();
        assert!(cached.stats().await.memory_entries > 0);

        // 等待 TTL 过期（2 秒，确保过期）
        tokio::time::sleep(Duration::from_secs(2)).await;

        // 缓存应已过期（moka 的过期清理是惰性的，entry_count 可能不立即更新）
        // 但 get 应返回 None
        let key = CacheKey::memory(&memory_id);
        let result = cached.memory_cache.get(&key).await;
        assert!(result.is_none(), "TTL 过期后缓存应失效");
    }
}
