//! # 搜索索引器（v2.5 批次 7）
//!
//! 封装归档后自动索引逻辑：将归档的 [`IndexHook`] 摘要文本同步到
//! 关键词索引（BM25）和向量索引（Embedding），供 `/search` 端点检索。
//!
//! ## 架构
//!
//! ```text
//! archive handler
//!   │
//!   ├─→ Archiver.archive() → IndexHook
//!   │
//!   └─→ SearchIndexer.index_hook(&hook)
//!         │
//!         ├─→ extract_index_text(hook)  → 组合 title + abstract + key_facts + key_entities + tags
//!         │
//!         ├─→ KeywordSearcher.index(hook_id, memory_id, text)   ← 关键词索引
//!         │
//!         └─→ Embedder.embed(text) → Vec<f32>
//!               │
//!               └─→ VectorIndex.add(hook_id, memory_id, vector) ← 向量索引
//! ```
//!
//! ## 降级策略
//!
//! - 未配置 `embedder` 或 `vector_index`：仅索引关键词（降级模式）
//! - Embedder 调用失败：跳过向量索引，记录 warn 日志（不影响关键词索引）
//!
//! ## 共享设计
//!
//! `SearchIndexer` 与 `HybridRetriever` 共享同一组 `Arc<dyn KeywordSearcher>`
//! 和 `Arc<dyn VectorIndex>`，确保归档后索引的数据能被检索器立即访问。

use hippocampus_core::model::IndexHook;
use hippocampus_core::semantic::{Embedder, KeywordSearcher, VectorIndex};
use std::sync::Arc;

// ============================================================================
// SearchIndexer
// ============================================================================

/// 搜索索引器
///
/// 归档后自动将摘要文本索引到关键词索引和向量索引。
///
/// ## 创建
///
/// 通常由 `main.rs` 从环境变量配置构造，注入到 `AppState`：
///
/// ```rust,ignore
/// let keyword = Arc::new(Bm25Searcher::new());
/// let embedder = Arc::new(HttpEmbedder::new(config)?);
/// let vector_index = Arc::new(InMemoryVectorIndex::new(embedder.dim()));
/// let indexer = Arc::new(SearchIndexer::new(
///     keyword.clone(),
///     Some(embedder.clone()),
///     Some(vector_index.clone()),
/// ));
/// let retriever = Arc::new(HybridRetriever::new(keyword, embedder, vector_index));
/// ```
pub struct SearchIndexer {
    /// 关键词索引器（必填，降级模式下也启用）
    keyword: Arc<dyn KeywordSearcher>,
    /// 文本向量化器（可选，None 时仅索引关键词）
    embedder: Option<Arc<dyn Embedder>>,
    /// 向量索引器（可选，None 时仅索引关键词）
    vector_index: Option<Arc<dyn VectorIndex>>,
}

impl SearchIndexer {
    /// 创建搜索索引器
    ///
    /// - `keyword`：关键词索引器（必填）
    /// - `embedder`：文本向量化器（None 时仅关键词索引）
    /// - `vector_index`：向量索引器（None 时仅关键词索引）
    pub fn new(
        keyword: Arc<dyn KeywordSearcher>,
        embedder: Option<Arc<dyn Embedder>>,
        vector_index: Option<Arc<dyn VectorIndex>>,
    ) -> Self {
        Self {
            keyword,
            embedder,
            vector_index,
        }
    }

    /// 从 IndexHook 提取用于索引的文本
    ///
    /// 组合摘要的多维信息：
    /// - title（标题）
    /// - abstract_text（抽象摘要，如有）
    /// - key_facts（关键事实，join）
    /// - key_entities（关键实体，join）
    /// - tags（标签中文显示名，join）
    fn extract_index_text(hook: &IndexHook) -> String {
        let mut parts: Vec<String> = Vec::new();

        // 标题（日级启发式生成，必填）
        parts.push(hook.summary.title.clone());

        // 抽象摘要（周级/月级 LLM 生成）
        if let Some(abs) = &hook.summary.abstract_text {
            if !abs.trim().is_empty() {
                parts.push(abs.clone());
            }
        }

        // 关键事实
        if !hook.summary.key_facts.is_empty() {
            parts.push(hook.summary.key_facts.join(" "));
        }

        // 关键实体
        if !hook.summary.key_entities.is_empty() {
            parts.push(hook.summary.key_entities.join(" "));
        }

        // 标签（中文显示名）
        if !hook.tags.is_empty() {
            let tag_str: Vec<String> = hook.tags.iter().map(|t| t.to_string()).collect();
            parts.push(tag_str.join(" "));
        }

        parts.join(" | ")
    }

    /// 归档后触发索引
    ///
    /// 将 hook 的摘要文本索引到关键词索引和向量索引。
    /// Embedder 失败时跳过向量索引，不影响关键词索引。
    pub async fn index_hook(&self, hook: &IndexHook) {
        let text = Self::extract_index_text(hook);
        let hook_id = hook.id.to_string();
        let memory_id = hook.memory_id.clone();

        // 1. 关键词索引（必执行）
        self.keyword.index(&hook_id, &memory_id, &text);

        // 2. 向量索引（仅当 embedder 和 vector_index 都存在时执行）
        if let (Some(embedder), Some(vi)) = (&self.embedder, &self.vector_index) {
            match embedder.embed(&text).await {
                Ok(vector) => {
                    vi.add(&hook_id, &memory_id, vector);
                }
                Err(e) => {
                    tracing::warn!(
                        hook_id = %hook_id,
                        error = %e,
                        "Embedder 失败，跳过向量索引（关键词索引已更新）"
                    );
                }
            }
        }

        tracing::debug!(
            hook_id = %hook_id,
            memory_id = %memory_id,
            text_len = text.len(),
            "索引完成"
        );
    }

    /// 获取关键词索引器引用（供 retriever 构造时共享）
    pub fn keyword(&self) -> &Arc<dyn KeywordSearcher> {
        &self.keyword
    }

    /// 获取向量索引器引用（供 retriever 构造时共享）
    pub fn vector_index(&self) -> Option<&Arc<dyn VectorIndex>> {
        self.vector_index.as_ref()
    }

    /// 获取 Embedder 引用（供 retriever 构造时共享）
    pub fn embedder(&self) -> Option<&Arc<dyn Embedder>> {
        self.embedder.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hippocampus_core::bm25::Bm25Searcher;
    use hippocampus_core::model::{ArchivePeriod, Summary, Tag};
    use hippocampus_core::vector::InMemoryVectorIndex;
    use chrono::Utc;
    use uuid::Uuid;

    // ============================================================================
    // Mock Embedder（参考 hybrid.rs 测试）
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

    /// 构造测试用 IndexHook
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
            file_status: hippocampus_core::model::FileStatus::Normal,
        }
    }

    // ============================================================================
    // 测试用例
    // ============================================================================

    #[test]
    fn test_extract_index_text_basic() {
        let hook = make_hook("Rust 编程语言", vec![]);
        let text = SearchIndexer::extract_index_text(&hook);
        assert!(text.contains("Rust 编程语言"));
    }

    #[test]
    fn test_extract_index_text_with_key_facts() {
        let hook = make_hook(
            "Rust 编程语言",
            vec!["Rust 强调安全性".into(), "所有权机制".into()],
        );
        let text = SearchIndexer::extract_index_text(&hook);
        assert!(text.contains("Rust 编程语言"));
        assert!(text.contains("Rust 强调安全性"));
        assert!(text.contains("所有权机制"));
        assert!(text.contains("文本消息")); // Tag::Text 的 Display
    }

    #[test]
    fn test_extract_index_text_with_abstract() {
        let mut hook = make_hook("Rust", vec![]);
        hook.summary.abstract_text = Some("Rust 是系统编程语言".into());
        let text = SearchIndexer::extract_index_text(&hook);
        assert!(text.contains("Rust 是系统编程语言"));
    }

    #[tokio::test]
    async fn test_index_hook_keyword_only() {
        let keyword = Arc::new(Bm25Searcher::new());
        let indexer = SearchIndexer::new(keyword.clone(), None, None);

        let hook = make_hook("Rust 安全编程", vec!["所有权机制".into()]);
        indexer.index_hook(&hook).await;

        // 验证关键词索引已更新
        assert_eq!(keyword.len(), 1);
        let results = keyword.search("Rust", 5);
        assert!(!results.is_empty());
        assert_eq!(results[0].hook_id, hook.id.to_string());
    }

    #[tokio::test]
    async fn test_index_hook_with_vector() {
        let keyword = Arc::new(Bm25Searcher::new());
        let embedder = Arc::new(MockEmbedder::new(8));
        let vector_index = Arc::new(InMemoryVectorIndex::new(8));
        let indexer = SearchIndexer::new(
            keyword.clone(),
            Some(embedder),
            Some(vector_index.clone()),
        );

        let hook = make_hook("Rust 安全编程", vec![]);
        indexer.index_hook(&hook).await;

        // 验证关键词索引
        assert_eq!(keyword.len(), 1);

        // 验证向量索引
        assert_eq!(vector_index.len(), 1);
    }

    #[tokio::test]
    async fn test_index_hook_multiple() {
        let keyword = Arc::new(Bm25Searcher::new());
        let indexer = SearchIndexer::new(keyword.clone(), None, None);

        // 索引 3 个 hook
        for i in 0..3 {
            let hook = make_hook(&format!("文档 {}", i), vec![]);
            indexer.index_hook(&hook).await;
        }

        assert_eq!(keyword.len(), 3);
    }

    #[tokio::test]
    async fn test_index_hook_embedder_failure_skips_vector() {
        use hippocampus_core::semantic::KeywordSearcher;

        /// 始终失败的 Embedder
        struct FailEmbedder;

        #[async_trait::async_trait]
        impl Embedder for FailEmbedder {
            fn dim(&self) -> usize {
                8
            }

            async fn embed(&self, _text: &str) -> hippocampus_core::Result<Vec<f32>> {
                Err(hippocampus_core::Error::Storage("mock failure".into()))
            }
        }

        let keyword = Arc::new(Bm25Searcher::new());
        let embedder = Arc::new(FailEmbedder);
        let vector_index = Arc::new(InMemoryVectorIndex::new(8));
        let indexer = SearchIndexer::new(
            keyword.clone(),
            Some(embedder),
            Some(vector_index.clone()),
        );

        let hook = make_hook("测试文档", vec![]);
        indexer.index_hook(&hook).await;

        // 关键词索引应成功
        assert_eq!(keyword.len(), 1);
        assert!(!keyword.search("测试", 5).is_empty());

        // 向量索引应为空（Embedder 失败）
        assert_eq!(vector_index.len(), 0);
    }
}
