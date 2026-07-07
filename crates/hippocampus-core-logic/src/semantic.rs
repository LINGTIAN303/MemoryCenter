//! # 语义检索模块（v2.5 批次 7）
//!
//! 可插拔的语义检索架构，支持关键词检索 + 向量语义检索 + 混合检索（RRF 融合）。
//!
//! ## 架构
//!
//! ```text
//! query
//!   │
//!   ├─→ KeywordSearcher.search(query, top_k)  ─→ results_kw  (BM25 top-K)
//!   │
//!   └─→ Embedder.embed(query) ─→ Vec<f32>
//!                              │
//!                              └─→ VectorIndex.search(vec, top_k) ─→ results_sem (cosine top-K)
//!                                                                    │
//!                                              ┌─────────────────────┘
//!                                              ▼
//!                                    HybridRetriever（RRF 融合）
//!                                              │
//!                                              ▼
//!                                        final top-K
//! ```
//!
//! ## 4 个核心 trait
//!
//! - [`Embedder`]：文本向量化接口（远程 API / 本地模型）
//! - [`KeywordSearcher`]：关键词检索接口（BM25 / TF-IDF）
//! - [`VectorIndex`]：向量索引接口（内存暴力 / HNSW）
//! - [`SemanticRetriever`]：统一检索接口（关键词 / 语义 / 混合）
//!
//! ## 降级策略
//!
//! Embedder API 失败时，[`HybridRetriever`] 自动降级为仅关键词检索，
//! 返回结果的 `source` 字段标记为 `Keyword`。

use std::collections::HashMap;

// ============================================================================
// 数据结构
// ============================================================================

/// 检索命中结果
///
/// 表示一次检索的命中项，包含钩子 ID、分数和来源。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchHit {
    /// 钩子 ID（用于后续检索完整记忆）
    pub hook_id: String,
    /// 指向的记忆文件 ID
    pub memory_id: String,
    /// 相关性分数（0.0-1.0，越高越相关）
    pub score: f32,
    /// 检索来源（关键词 / 语义 / 混合）
    pub source: RetrievalSource,
}

/// 检索来源标识
///
/// 用于标记检索结果来自哪个检索器，便于调试和降级处理。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalSource {
    /// 关键词检索（BM25）
    Keyword,
    /// 语义检索（向量相似度）
    Semantic,
    /// 混合检索（RRF 融合）
    Hybrid,
}

// ============================================================================
// Embedder trait（文本向量化）
// ============================================================================

/// 文本向量化 trait（可插拔：远程 API / 本地模型）
///
/// 将文本转换为向量，用于语义检索。
///
/// ## 实现
///
/// - `HttpEmbedder`（llm 层）：调用 OpenAI 兼容的 `/v1/embeddings` API
/// - 未来可扩展 `LocalEmbedder`（ort / candle 本地推理）
///
/// ## 注意
///
/// - 必须 `Send + Sync`：多 session 并发检索
/// - 必须异步：远程 API 是网络 IO
/// - `dim()` 必须暴露：不同模型维度不同，存储和相似度计算都需感知
#[async_trait::async_trait]
pub trait Embedder: Send + Sync {
    /// 向量维度（如 OpenAI text-embedding-3-small 为 1536）
    fn dim(&self) -> usize;

    /// 单条文本嵌入
    async fn embed(&self, text: &str) -> crate::Result<Vec<f32>>;

    /// 批量嵌入（远程 API 推荐，省 token 省 QPS）
    ///
    /// 默认实现：循环调用 `embed`。后端可覆写为单次 API 批量请求。
    async fn embed_batch(&self, texts: &[&str]) -> crate::Result<Vec<Vec<f32>>> {
        let mut results = Vec::with_capacity(texts.len());
        for text in texts {
            results.push(self.embed(text).await?);
        }
        Ok(results)
    }

    /// 向量是否已归一化（模长 = 1）
    ///
    /// OpenAI text-embedding-3 系列返回的向量已归一化，
    /// 可直接用点积替代 cosine 相似度（省两个 sqrt）。
    /// 默认返回 `true`（多数现代 embedding 模型都归一化）。
    fn is_normalized(&self) -> bool {
        true
    }
}

// ============================================================================
// KeywordSearcher trait（关键词检索）
// ============================================================================

/// 关键词检索 trait（可插拔：BM25 / TF-IDF / tantivy）
///
/// 对记忆文本建立倒排索引，支持关键词检索。
///
/// ## 实现
///
/// - [`crate::bm25::Bm25Searcher`]：默认实现，BM25 算法 + jieba 中文分词
///
/// ## 线程安全
///
/// 内部状态可变（索引增删），通过 `RwLock` 保证并发安全。
/// trait 本身 `Send + Sync`，实现需自行处理内部可变性。
pub trait KeywordSearcher: Send + Sync {
    /// 索引一条文档
    ///
    /// - `hook_id`：钩子 ID（用于关联检索结果）
    /// - `text`：待索引的文本（记忆摘要 + 关键事实 + 标签）
    fn index(&self, hook_id: &str, memory_id: &str, text: &str);

    /// 批量索引
    ///
    /// 默认实现：循环调用 `index`。
    fn index_batch(&self, docs: Vec<(String, String, String)>) {
        for (hook_id, memory_id, text) in docs {
            self.index(&hook_id, &memory_id, &text);
        }
    }

    /// 搜索 top-k 最相关的文档
    ///
    /// 返回按分数降序排列的 `SearchHit` 列表，`source` 为 `Keyword`。
    fn search(&self, query: &str, top_k: usize) -> Vec<SearchHit>;

    /// 删除索引项（记忆淘汰时同步）
    fn remove(&self, hook_id: &str);

    /// 索引文档数量
    fn len(&self) -> usize;

    /// 索引是否为空
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 清空索引
    fn clear(&self);
}

// ============================================================================
// VectorIndex trait（向量索引）
// ============================================================================

/// 向量索引 trait（可插拔：内存暴力 / HNSW / SQLite-vec）
///
/// 存储向量并支持相似度检索。
///
/// ## 实现
///
/// - [`crate::vector::InMemoryVectorIndex`]：默认实现，内存暴力扫描
/// - 未来可扩展 `HnswIndex`（instant-distance）
///
/// ## 线程安全
///
/// 同 `KeywordSearcher`，通过 `RwLock` 保证并发安全。
pub trait VectorIndex: Send + Sync {
    /// 添加向量
    ///
    /// - `hook_id`：钩子 ID
    /// - `memory_id`：记忆文件 ID
    /// - `vector`：embedding 向量
    fn add(&self, hook_id: &str, memory_id: &str, vector: Vec<f32>);

    /// 批量添加
    ///
    /// 默认实现：循环调用 `add`。
    fn add_batch(&self, items: Vec<(String, String, Vec<f32>)>) {
        for (hook_id, memory_id, vector) in items {
            self.add(&hook_id, &memory_id, vector);
        }
    }

    /// 查询 top-k 最相似的向量
    ///
    /// 返回按相似度降序排列的 `SearchHit` 列表，`source` 为 `Semantic`。
    fn search(&self, query: &[f32], top_k: usize) -> Vec<SearchHit>;

    /// 删除向量（记忆淘汰时同步）
    fn remove(&self, hook_id: &str);

    /// 向量数量
    fn len(&self) -> usize;

    /// 索引是否为空
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 清空索引
    fn clear(&self);

    /// 向量维度
    fn dim(&self) -> usize;
}

// ============================================================================
// SemanticRetriever trait（统一检索接口）
// ============================================================================

/// 语义检索 trait（统一接口，背后可能是关键词/语义/混合）
///
/// 这是面向调用方（HTTP handler / MCP tool）的统一接口。
/// 调用方无需关心底层是关键词检索、向量检索还是混合检索。
///
/// ## 实现
///
/// - [`crate::hybrid::HybridRetriever`]：混合检索（关键词 + 语义 + RRF 融合）
/// - 也可只包装 `KeywordSearcher` 或 `VectorIndex` 作为单模态检索器
#[async_trait::async_trait]
pub trait SemanticRetriever: Send + Sync {
    /// 语义检索
    ///
    /// - `query`：查询文本
    /// - `top_k`：返回 top-K 结果
    ///
    /// 返回按相关性降序排列的 `SearchHit` 列表。
    async fn search(&self, query: &str, top_k: usize) -> crate::Result<Vec<SearchHit>>;
}

// ============================================================================
// 辅助函数
// ============================================================================

/// RRF（Reciprocal Rank Fusion，倒数排名融合）算法
///
/// 将多个检索器的结果按排名融合，无需归一化分数。
///
/// ## 公式
///
/// `score(d) = Σ 1 / (k + rank_i(d))`
///
/// - `k`：平滑参数（默认 60，Elasticsearch 8.x 默认值）
/// - `rank_i(d)`：文档 d 在第 i 个检索器中的排名（从 1 开始）
///
/// ## 优势
///
/// - 无需归一化分数（BM25 分数 0-30，cosine 分数 0.5-1.0，尺度不同）
/// - 对分数分布不敏感
/// - 实现简单
///
/// ## 参数
///
/// - `results_list`：多个检索器的结果列表（每个已按分数降序排列）
/// - `top_k`：返回 top-K
/// - `k`：RRF 平滑参数（默认 60）
pub fn rrf_fusion(
    results_list: &[Vec<SearchHit>],
    top_k: usize,
    k: u32,
) -> Vec<SearchHit> {
    // hook_id → (累计分数, memory_id, 最佳来源)
    let mut scores: HashMap<String, (f32, String, RetrievalSource)> = HashMap::new();

    for results in results_list {
        for (rank, hit) in results.iter().enumerate() {
            let rrf_score = 1.0 / (k as f32 + (rank + 1) as f32);
            let entry = scores
                .entry(hit.hook_id.clone())
                .or_insert((0.0, hit.memory_id.clone(), RetrievalSource::Hybrid));
            entry.0 += rrf_score;
            entry.1 = hit.memory_id.clone();
        }
    }

    // 按累计分数降序排列，取 top_k
    let mut all: Vec<SearchHit> = scores
        .into_iter()
        .map(|(hook_id, (score, memory_id, source))| SearchHit {
            hook_id,
            memory_id,
            score,
            source,
        })
        .collect();
    all.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    all.truncate(top_k);
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rrf_fusion_basic() {
        // 两个检索器，结果有重叠
        let kw_results = vec![
            SearchHit {
                hook_id: "h1".into(),
                memory_id: "m1".into(),
                score: 10.0,
                source: RetrievalSource::Keyword,
            },
            SearchHit {
                hook_id: "h2".into(),
                memory_id: "m2".into(),
                score: 8.0,
                source: RetrievalSource::Keyword,
            },
        ];
        let sem_results = vec![
            SearchHit {
                hook_id: "h2".into(),
                memory_id: "m2".into(),
                score: 0.9,
                source: RetrievalSource::Semantic,
            },
            SearchHit {
                hook_id: "h3".into(),
                memory_id: "m3".into(),
                score: 0.8,
                source: RetrievalSource::Semantic,
            },
        ];

        let fused = rrf_fusion(&[kw_results, sem_results], 3, 60);

        // h2 在两个检索器中都出现，应排第一
        assert_eq!(fused[0].hook_id, "h2");
        assert!(fused[0].score > fused[1].score);
        assert_eq!(fused.len(), 3);
        assert_eq!(fused[0].source, RetrievalSource::Hybrid);
    }

    #[test]
    fn test_rrf_fusion_empty() {
        let fused = rrf_fusion(&[], 5, 60);
        assert!(fused.is_empty());
    }

    #[test]
    fn test_rrf_fusion_single_source() {
        let results = vec![SearchHit {
            hook_id: "h1".into(),
            memory_id: "m1".into(),
            score: 1.0,
            source: RetrievalSource::Keyword,
        }];
        let fused = rrf_fusion(&[results], 5, 60);
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].hook_id, "h1");
    }

    #[test]
    fn test_rrf_fusion_top_k_truncation() {
        let results = (0..10)
            .map(|i| SearchHit {
                hook_id: format!("h{}", i),
                memory_id: format!("m{}", i),
                score: 1.0,
                source: RetrievalSource::Keyword,
            })
            .collect();
        let fused = rrf_fusion(&[results], 3, 60);
        assert_eq!(fused.len(), 3);
    }

    #[test]
    fn test_retrieval_source_serialization() {
        let hit = SearchHit {
            hook_id: "h1".into(),
            memory_id: "m1".into(),
            score: 0.5,
            source: RetrievalSource::Hybrid,
        };
        let json = serde_json::to_string(&hit).unwrap();
        assert!(json.contains("\"hybrid\""));
        assert!(!json.contains("\"Hybrid\""));
    }
}
