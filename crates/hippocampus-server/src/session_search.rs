//! # Session 级索引隔离路由器（v2.8）
//!
//! 解决 v2.5 批次 7 遗留的全局单例问题：BM25 索引和向量索引原为全局共享，
//! 任意 session 的 /search 会返回其他 session 的结果。
//!
//! ## 架构
//!
//! ```text
//! archive handler                          search handler
//!   │                                        │
//!   └─→ SessionSearchRouter.index_hook(sid, hook)
//!         │                                  └─→ SessionSearchRouter.search(sid, query, top_k)
//!         │                                        │
//!         ├─→ 获取/创建 sid 的 SessionIndices      └─→ 获取/创建 sid 的 SessionIndices
//!         │                                        │
//!         ├─→ keyword.index(hook_id, text)         └─→ retriever.search(query, top_k)
//!         └─→ embedder.embed → vector.add
//! ```
//!
//! ## 隔离策略
//!
//! - 每个 session_id 拥有独立的 `Bm25Searcher` + `InMemoryVectorIndex`
//! - 索引和查询完全隔离，不跨 session 返回结果
//! - session 首次访问时懒加载创建索引器
//! - 未配置 Embedder 时降级为 `KeywordOnlyRetriever`
//!
//! ## 内存管理
//!
//! - 当前实现：session 索引常驻内存，不自动清理
//! - v2.9 计划：LRU 淘汰 + TTL 过期（长时间未访问的 session 索引释放）

use dashmap::DashMap;
use hippocampus_core::bm25::Bm25Searcher;
use hippocampus_core::hybrid::{HybridRetriever, KeywordOnlyRetriever};
use hippocampus_core::model::IndexHook;
use hippocampus_core::semantic::{
    Embedder, KeywordSearcher, SearchHit, SemanticRetriever, VectorIndex,
};
use hippocampus_core::vector::InMemoryVectorIndex;
use std::sync::Arc;

// ============================================================================
// SessionIndices：单个 session 的索引器集合
// ============================================================================

/// 单个 session 的索引器集合
///
/// 每个 session 独立持有：
/// - `keyword`：BM25 关键词索引器（写入 + 查询共享）
/// - `vector`：向量索引器（写入 + 查询共享，未配置 Embedder 时为 None）
/// - `retriever`：语义检索器（Hybrid 或 KeywordOnly，内部共享同一组 keyword/vector）
struct SessionIndices {
    /// 关键词索引器（index_hook 写入 + retriever 查询共享）
    keyword: Arc<dyn KeywordSearcher>,
    /// 向量索引器（index_hook 写入 + retriever 查询共享，降级模式为 None）
    vector: Option<Arc<dyn VectorIndex>>,
    /// 语义检索器（Hybrid 或 KeywordOnly）
    retriever: Arc<dyn SemanticRetriever>,
}

// ============================================================================
// SessionSearchRouter
// ============================================================================

/// Session 级索引隔离路由器
///
/// 按 session_id 路由到独立的子索引器，实现 session 间完全隔离。
/// 替代 v2.5 的全局单例 `SearchIndexer` + `SemanticRetriever`。
///
/// ## 创建
///
/// 通常由 `main.rs` 从环境变量构造，注入到 `AppState.session_search`：
///
/// ```rust,ignore
/// let router = SessionSearchRouter::new(
///     Some(embedder),   // None 时降级为仅关键词
///     dim,              // 向量维度
/// );
/// ```
pub struct SessionSearchRouter {
    /// Embedder（可选，None 时降级为仅关键词检索）
    embedder: Option<Arc<dyn Embedder>>,
    /// 向量维度（embedder 存在时使用）
    dim: usize,
    /// session → 独立索引器集合
    sessions: DashMap<String, SessionIndices>,
}

impl SessionSearchRouter {
    /// 创建 Session 级索引路由器
    ///
    /// - `embedder`：文本向量化器（None 时降级为仅关键词检索）
    /// - `dim`：向量维度（embedder 存在时使用）
    pub fn new(embedder: Option<Arc<dyn Embedder>>, dim: usize) -> Self {
        Self {
            embedder,
            dim,
            sessions: DashMap::new(),
        }
    }

    /// 获取或创建指定 session 的索引器集合
    ///
    /// 首次访问时懒加载创建独立的 keyword + vector + retriever。
    /// `KeywordSearcher` 和 `VectorIndex` 在 indexer（写入）与 retriever（查询）间共享 Arc。
    fn get_or_create(&self, sid: &str) -> dashmap::mapref::one::Ref<'_, String, SessionIndices> {
        // fast path：已存在直接返回
        if let Some(entry) = self.sessions.get(sid) {
            return entry;
        }
        // slow path：创建新 session 索引
        let keyword: Arc<dyn KeywordSearcher> = Arc::new(Bm25Searcher::new());

        let (vector, retriever): (Option<Arc<dyn VectorIndex>>, Arc<dyn SemanticRetriever>) =
            match &self.embedder {
                Some(embedder) => {
                    // 完整模式：HybridRetriever（关键词 + 向量 + RRF 融合）
                    let vector_index: Arc<dyn VectorIndex> =
                        Arc::new(InMemoryVectorIndex::new(self.dim));
                    let retriever: Arc<dyn SemanticRetriever> = Arc::new(HybridRetriever::new(
                        keyword.clone(),
                        embedder.clone(),
                        vector_index.clone(),
                    ));
                    (Some(vector_index), retriever)
                }
                None => {
                    // 降级模式：KeywordOnlyRetriever（仅关键词）
                    let retriever: Arc<dyn SemanticRetriever> =
                        Arc::new(KeywordOnlyRetriever::new(keyword.clone()));
                    (None, retriever)
                }
            };

        self.sessions.insert(
            sid.to_string(),
            SessionIndices {
                keyword,
                vector,
                retriever,
            },
        );
        // 此时必成功（刚插入）
        self.sessions.get(sid).expect("刚插入的 session 不应缺失")
    }

    /// 归档后触发索引（按 session 隔离）
    ///
    /// 将 hook 的摘要文本索引到该 session 的关键词索引和向量索引。
    /// Embedder 失败时跳过向量索引，不影响关键词索引。
    pub async fn index_hook(&self, sid: &str, hook: &IndexHook) {
        let text = extract_index_text(hook);
        let hook_id = hook.id.to_string();
        let memory_id = hook.memory_id.clone();

        let indices = self.get_or_create(sid);

        // 1. 关键词索引（必执行）
        indices.keyword.index(&hook_id, &memory_id, &text);

        // 2. 向量索引（仅当 embedder 和 vector 都存在时执行）
        if let (Some(embedder), Some(vi)) = (&self.embedder, &indices.vector) {
            match embedder.embed(&text).await {
                Ok(vector) => {
                    vi.add(&hook_id, &memory_id, vector);
                }
                Err(e) => {
                    tracing::warn!(
                        session = %sid,
                        hook_id = %hook_id,
                        error = %e,
                        "Embedder 失败，跳过向量索引（关键词索引已更新）"
                    );
                }
            }
        }

        tracing::debug!(
            session = %sid,
            hook_id = %hook_id,
            memory_id = %memory_id,
            text_len = text.len(),
            "session 索引完成"
        );
    }

    /// 语义检索（按 session 隔离）
    ///
    /// 只搜索该 session 的索引，不返回其他 session 的结果。
    pub async fn search(
        &self,
        sid: &str,
        query: &str,
        top_k: usize,
    ) -> hippocampus_core::Result<Vec<SearchHit>> {
        let indices = self.get_or_create(sid);
        indices.retriever.search(query, top_k).await
    }

    /// 获取已注册的 session 数量（供监控/测试）
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// 移除指定 session 的索引（供周期清理使用，v2.9 计划）
    pub fn remove_session(&self, sid: &str) -> bool {
        self.sessions.remove(sid).is_some()
    }
}

// ============================================================================
// 辅助函数：提取索引文本
// ============================================================================

/// 从 IndexHook 提取用于索引的文本
///
/// 组合摘要的多维信息：title + abstract + key_facts + key_entities + tags
fn extract_index_text(hook: &IndexHook) -> String {
    let mut parts: Vec<String> = Vec::new();

    parts.push(hook.summary.title.clone());

    if let Some(abs) = &hook.summary.abstract_text {
        if !abs.trim().is_empty() {
            parts.push(abs.clone());
        }
    }

    if !hook.summary.key_facts.is_empty() {
        parts.push(hook.summary.key_facts.join(" "));
    }

    if !hook.summary.key_entities.is_empty() {
        parts.push(hook.summary.key_entities.join(" "));
    }

    if !hook.tags.is_empty() {
        let tag_str: Vec<String> = hook.tags.iter().map(|t| t.to_string()).collect();
        parts.push(tag_str.join(" "));
    }

    parts.join(" | ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use hippocampus_core::model::{ArchivePeriod, Summary, Tag};
    use chrono::Utc;
    use uuid::Uuid;

    // ============================================================================
    // Mock Embedder
    // ============================================================================

    struct MockEmbedder {
        dim: usize,
    }

    impl MockEmbedder {
        fn new(dim: usize) -> Self {
            Self { dim }
        }
    }

    #[async_trait::async_trait]
    impl Embedder for MockEmbedder {
        fn dim(&self) -> usize {
            self.dim
        }

        async fn embed(&self, text: &str) -> hippocampus_core::Result<Vec<f32>> {
            let mut vector = vec![0.0_f32; self.dim];
            for (i, c) in text.chars().enumerate() {
                vector[i % self.dim] += c as u32 as f32;
            }
            let norm: f32 = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                for v in &mut vector {
                    *v /= norm;
                }
            }
            Ok(vector)
        }
    }

    // ============================================================================
    // 测试辅助
    // ============================================================================

    fn make_hook(title: &str, key_facts: Vec<String>) -> IndexHook {
        IndexHook {
            id: Uuid::new_v4(),
            memory_id: format!("mem-{}", Uuid::new_v4()),
            summary: Summary {
                title: title.to_string(),
                abstract_text: None,
                key_facts,
                key_entities: Vec::new(),
                clue_anchors: Vec::new(),
            },
            tags: vec![Tag::Text],
            archived_at: Utc::now(),
            period: ArchivePeriod::Daily,
            token_count: 100,
        }
    }

    // ============================================================================
    // 测试用例
    // ============================================================================

    #[test]
    fn test_extract_index_text_basic() {
        let hook = make_hook("测试标题", vec![]);
        let text = extract_index_text(&hook);
        assert!(text.contains("测试标题"));
    }

    #[test]
    fn test_router_session_count_initial() {
        let router = SessionSearchRouter::new(None, 0);
        assert_eq!(router.session_count(), 0);
    }

    #[tokio::test]
    async fn test_router_keyword_only_search() {
        // 未配置 Embedder → 降级为仅关键词检索
        let router = SessionSearchRouter::new(None, 0);

        let hook = make_hook("Rust 安全编程", vec!["所有权机制".into()]);
        router.index_hook("sess-1", &hook).await;

        let results = router.search("sess-1", "Rust", 5).await.unwrap();
        assert!(!results.is_empty(), "应能搜索到已索引的内容");
        assert_eq!(results[0].hook_id, hook.id.to_string());
    }

    #[tokio::test]
    async fn test_router_session_isolation() {
        // 核心：不同 session 的索引完全隔离
        let router = SessionSearchRouter::new(None, 0);

        let hook1 = make_hook("Rust 编程语言", vec![]);
        router.index_hook("sess-1", &hook1).await;

        let hook2 = make_hook("Python 编程语言", vec![]);
        router.index_hook("sess-2", &hook2).await;

        // session-1 搜索 "Rust" → 应找到 hook1
        let results1 = router.search("sess-1", "Rust", 5).await.unwrap();
        assert!(!results1.is_empty(), "sess-1 应找到 Rust");
        assert_eq!(results1[0].hook_id, hook1.id.to_string());

        // session-1 搜索 "Python" → 不应找到 hook2（隔离）
        let results1_py = router.search("sess-1", "Python", 5).await.unwrap();
        assert!(
            results1_py.is_empty()
                || !results1_py.iter().any(|r| r.hook_id == hook2.id.to_string()),
            "sess-1 不应搜到 sess-2 的 Python 内容"
        );

        // session-2 搜索 "Python" → 应找到 hook2
        let results2 = router.search("sess-2", "Python", 5).await.unwrap();
        assert!(!results2.is_empty(), "sess-2 应找到 Python");
        assert_eq!(results2[0].hook_id, hook2.id.to_string());

        // session-2 搜索 "Rust" → 不应找到 hook1（隔离）
        let results2_rs = router.search("sess-2", "Rust", 5).await.unwrap();
        assert!(
            results2_rs.is_empty()
                || !results2_rs.iter().any(|r| r.hook_id == hook1.id.to_string()),
            "sess-2 不应搜到 sess-1 的 Rust 内容"
        );
    }

    #[tokio::test]
    async fn test_router_session_count_after_index() {
        let router = SessionSearchRouter::new(None, 0);

        router.index_hook("sess-a", &make_hook("标题A", vec![])).await;
        router.index_hook("sess-b", &make_hook("标题B", vec![])).await;
        router.index_hook("sess-a", &make_hook("标题A2", vec![])).await;

        assert_eq!(router.session_count(), 2, "应有 2 个 session（a, b）");
    }

    #[tokio::test]
    async fn test_router_with_embedder() {
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(8));
        let router = SessionSearchRouter::new(Some(embedder), 8);

        let hook = make_hook("Rust 安全编程", vec![]);
        router.index_hook("sess-1", &hook).await;

        let results = router.search("sess-1", "Rust", 5).await.unwrap();
        assert!(!results.is_empty());
    }

    #[tokio::test]
    async fn test_router_remove_session() {
        let router = SessionSearchRouter::new(None, 0);

        router.index_hook("sess-1", &make_hook("标题", vec![])).await;
        assert_eq!(router.session_count(), 1);

        assert!(router.remove_session("sess-1"));
        assert_eq!(router.session_count(), 0);

        // 移除后重新搜索 → 应返回空（新建空索引）
        let results = router.search("sess-1", "标题", 5).await.unwrap();
        assert!(results.is_empty(), "移除后重建索引应为空");
    }

    #[tokio::test]
    async fn test_router_multiple_hooks_same_session() {
        let router = SessionSearchRouter::new(None, 0);

        for i in 0..3 {
            let hook = make_hook(&format!("文档 {}", i), vec![]);
            router.index_hook("sess-1", &hook).await;
        }

        let results = router.search("sess-1", "文档", 10).await.unwrap();
        assert_eq!(results.len(), 3, "应找到 3 个文档");
    }
}
