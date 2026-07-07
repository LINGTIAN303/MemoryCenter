//! # 向量索引（v2.5 批次 7）
//!
//! 内存暴力扫描的向量索引实现，支持余弦相似度检索。
//!
//! ## 算法
//!
//! **余弦相似度**（Cosine Similarity）：
//!
//! ```text
//! sim(a, b) = dot(a, b) / (|a| * |b|)
//!           = Σ(a_i * b_i) / (sqrt(Σ a_i²) * sqrt(Σ b_i²))
//! ```
//!
//! 范围 `[-1, 1]`，越接近 1 越相似。语义检索中 embedding 通常已归一化（|v|=1），
//! 此时 `sim(a, b) = dot(a, b)`，省两个 sqrt。
//!
//! ## 复杂度
//!
//! - 内存：O(N * d)，N 为向量数，d 为维度
//! - 查询：O(N * d)，暴力扫描所有向量
//!
//! 适合 MVP 阶段（记忆数 < 10K，维度 1536）。后续可替换为 HNSW（O(log N)）。
//!
//! ## 线程安全
//!
//! 内部用 `RwLock<HashMap>` 保证并发安全：读操作（search）可并发，写操作（add/remove）串行化。

use crate::semantic::{RetrievalSource, SearchHit, VectorIndex};
use std::collections::HashMap;
use std::sync::RwLock;

// ============================================================================
// 余弦相似度
// ============================================================================

/// 计算余弦相似度
///
/// `sim(a, b) = dot(a, b) / (|a| * |b|)`
///
/// 范围 `[-1, 1]`，越接近 1 越相似。
///
/// ## 边界情况
///
/// - 维度不一致：返回 0.0（视为不相似）
/// - 零向量（|v|=0）：返回 0.0（避免除零）
/// - 空向量：返回 0.0
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0_f32;
    let mut norm_a = 0.0_f32;
    let mut norm_b = 0.0_f32;

    for i in 0..a.len() {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }

    let denom = (norm_a.sqrt()) * (norm_b.sqrt());
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
    }
}

// ============================================================================
// InMemoryVectorIndex
// ============================================================================

/// 索引项
#[derive(Debug, Clone)]
struct VectorEntry {
    /// 记忆文件 ID
    memory_id: String,
    /// embedding 向量
    vector: Vec<f32>,
}

/// 内存向量索引（暴力扫描）
///
/// 默认实现 [`VectorIndex`] trait。
///
/// ## 参数
///
/// - `dim`：向量维度（构造时固定，add 时校验）
///
/// ## 并发
///
/// 内部用 `RwLock` 保证并发安全：读操作（search）可并发，写操作（add/remove）串行化。
///
/// ## 适用场景
///
/// - 记忆数 < 10K
/// - 维度 1536（OpenAI text-embedding-3-small）
/// - 单机内存充足
///
/// 大规模场景请使用 HNSW 索引（v2 路线图）。
pub struct InMemoryVectorIndex {
    /// 向量维度
    dim: usize,
    /// 索引：hook_id → VectorEntry
    entries: RwLock<HashMap<String, VectorEntry>>,
}

impl InMemoryVectorIndex {
    /// 创建新的向量索引
    ///
    /// - `dim`：向量维度（如 1536）
    pub fn new(dim: usize) -> Self {
        Self {
            dim,
            entries: RwLock::new(HashMap::new()),
        }
    }

    /// 计算 |v|²（模长平方，省一个 sqrt）
    fn norm_squared(v: &[f32]) -> f32 {
        v.iter().map(|x| x * x).sum()
    }
}

impl VectorIndex for InMemoryVectorIndex {
    fn add(&self, hook_id: &str, memory_id: &str, vector: Vec<f32>) {
        // 维度校验：维度不符直接忽略（避免污染索引）
        if vector.len() != self.dim {
            tracing::warn!(
                hook_id,
                memory_id,
                expected = self.dim,
                got = vector.len(),
                "向量维度不匹配，忽略此条目"
            );
            return;
        }

        let mut entries = self.entries.write().unwrap();
        entries.insert(
            hook_id.to_string(),
            VectorEntry {
                memory_id: memory_id.to_string(),
                vector,
            },
        );
    }

    fn search(&self, query: &[f32], top_k: usize) -> Vec<SearchHit> {
        // 维度校验
        if query.len() != self.dim {
            tracing::warn!(
                expected = self.dim,
                got = query.len(),
                "查询向量维度不匹配，返回空结果"
            );
            return Vec::new();
        }

        let entries = self.entries.read().unwrap();
        if entries.is_empty() {
            return Vec::new();
        }

        // 查询向量的模长（所有文档共用）
        let query_norm = Self::norm_squared(query).sqrt();
        if query_norm == 0.0 {
            return Vec::new();
        }

        // 暴力扫描：计算每个向量的相似度
        let mut scores: Vec<(String, String, f32)> = Vec::with_capacity(entries.len());

        for (hook_id, entry) in entries.iter() {
            // 维度已校验，这里直接用
            let sim = if entry.vector.len() == query.len() {
                cosine_similarity(query, &entry.vector)
            } else {
                0.0
            };

            // 只保留相似度 > 0 的结果（语义检索通常只关心正相关）
            if sim > 0.0 {
                scores.push((hook_id.clone(), entry.memory_id.clone(), sim));
            }
        }

        // 按相似度降序排列，取 top_k
        scores.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        scores.truncate(top_k);

        scores
            .into_iter()
            .map(|(hook_id, memory_id, score)| SearchHit {
                hook_id,
                memory_id,
                score,
                source: RetrievalSource::Semantic,
            })
            .collect()
    }

    fn remove(&self, hook_id: &str) {
        self.entries.write().unwrap().remove(hook_id);
    }

    fn len(&self) -> usize {
        self.entries.read().unwrap().len()
    }

    fn clear(&self) {
        self.entries.write().unwrap().clear();
    }

    fn dim(&self) -> usize {
        self.dim
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造归一化向量（模长 = 1）
    fn normalize(v: Vec<f32>) -> Vec<f32> {
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm == 0.0 {
            return v;
        }
        v.into_iter().map(|x| x / norm).collect()
    }

    #[test]
    fn test_cosine_similarity_identical() {
        let a = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&a, &a);
        // 相同向量相似度 = 1.0
        assert!((sim - 1.0).abs() < 1e-5);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        // 正交向量相似度 = 0
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-5);
    }

    #[test]
    fn test_cosine_similarity_opposite() {
        // 反方向向量相似度 = -1
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim + 1.0).abs() < 1e-5);
    }

    #[test]
    fn test_cosine_similarity_dim_mismatch() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn test_cosine_similarity_zero_vector() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 3.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn test_cosine_similarity_empty() {
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
    }

    #[test]
    fn test_vector_index_basic_search() {
        let index = InMemoryVectorIndex::new(3);

        // 索引 3 个向量（归一化）
        index.add("h1", "m1", normalize(vec![1.0, 0.0, 0.0]));
        index.add("h2", "m2", normalize(vec![0.0, 1.0, 0.0]));
        index.add("h3", "m3", normalize(vec![1.0, 1.0, 0.0]));

        // 查询：与 (1,0,0) 最相似
        let query = normalize(vec![1.0, 0.0, 0.0]);
        let results = index.search(&query, 3);

        assert!(!results.is_empty());
        assert_eq!(results[0].hook_id, "h1");
        // h1 与查询完全一致，相似度 ≈ 1.0
        assert!((results[0].score - 1.0).abs() < 1e-5);
        assert_eq!(results[0].source, RetrievalSource::Semantic);
    }

    #[test]
    fn test_vector_index_top_k() {
        let index = InMemoryVectorIndex::new(2);

        // 索引 5 个向量
        for i in 0..5 {
            let v = normalize(vec![1.0, i as f32]);
            index.add(&format!("h{}", i), &format!("m{}", i), v);
        }

        let query = normalize(vec![1.0, 0.0]);
        let results = index.search(&query, 3);
        assert_eq!(results.len(), 3, "应返回 top 3");
    }

    #[test]
    fn test_vector_index_remove() {
        let index = InMemoryVectorIndex::new(3);
        index.add("h1", "m1", vec![1.0, 0.0, 0.0]);
        index.add("h2", "m2", vec![0.0, 1.0, 0.0]);

        assert_eq!(index.len(), 2);

        index.remove("h1");
        assert_eq!(index.len(), 1);

        let results = index.search(&[1.0, 0.0, 0.0], 5);
        assert_eq!(results.len(), 0, "h1 已删除，应无结果");
    }

    #[test]
    fn test_vector_index_clear() {
        let index = InMemoryVectorIndex::new(3);
        index.add("h1", "m1", vec![1.0, 0.0, 0.0]);
        index.add("h2", "m2", vec![0.0, 1.0, 0.0]);

        index.clear();
        assert!(index.is_empty());

        let results = index.search(&[1.0, 0.0, 0.0], 5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_vector_index_reindex_same_hook() {
        let index = InMemoryVectorIndex::new(3);

        // 同一 hook_id 重新索引应覆盖
        index.add("h1", "m1", vec![1.0, 0.0, 0.0]);
        index.add("h1", "m1", vec![0.0, 1.0, 0.0]);

        assert_eq!(index.len(), 1, "应只有 1 个向量");

        // 查询 (0,1,0) 应命中
        let results = index.search(&[0.0, 1.0, 0.0], 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].hook_id, "h1");
        assert!((results[0].score - 1.0).abs() < 1e-5);
    }

    #[test]
    fn test_vector_index_dim_mismatch_ignored() {
        let index = InMemoryVectorIndex::new(3);

        // 维度不符应被忽略
        index.add("h1", "m1", vec![1.0, 0.0]); // dim=2 != 3
        assert_eq!(index.len(), 0, "维度不符的条目应被忽略");

        // 正确维度
        index.add("h2", "m2", vec![1.0, 0.0, 0.0]);
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn test_vector_index_query_dim_mismatch() {
        let index = InMemoryVectorIndex::new(3);
        index.add("h1", "m1", vec![1.0, 0.0, 0.0]);

        // 查询维度不符应返回空
        let results = index.search(&[1.0, 0.0], 5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_vector_index_empty_query() {
        let index = InMemoryVectorIndex::new(3);
        index.add("h1", "m1", vec![1.0, 0.0, 0.0]);

        // 零向量查询应返回空
        let results = index.search(&[0.0, 0.0, 0.0], 5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_vector_index_score_ordering() {
        let index = InMemoryVectorIndex::new(3);

        // h1 与查询完全一致（sim=1.0）
        index.add("h1", "m1", vec![1.0, 0.0, 0.0]);
        // h2 与查询部分相似（sim=0.707）
        index.add("h2", "m2", normalize(vec![1.0, 1.0, 0.0]));
        // h3 正交（sim=0）
        index.add("h3", "m3", vec![0.0, 1.0, 0.0]);

        let query = vec![1.0, 0.0, 0.0];
        let results = index.search(&query, 3);

        // h3 正交（sim=0）应被过滤
        assert_eq!(results.len(), 2);
        // h1 应排第一（sim=1.0）
        assert_eq!(results[0].hook_id, "h1");
        assert!((results[0].score - 1.0).abs() < 1e-5);
        // h2 应排第二
        assert_eq!(results[1].hook_id, "h2");
        assert!(results[0].score > results[1].score);
    }

    #[test]
    fn test_vector_index_no_positive_match() {
        let index = InMemoryVectorIndex::new(2);
        index.add("h1", "m1", vec![1.0, 0.0]);
        index.add("h2", "m2", vec![0.0, 1.0]);

        // 查询与所有向量正交（sim=0）
        let query = vec![0.0, 0.0]; // 零向量
        let results = index.search(&query, 5);
        assert!(results.is_empty(), "零向量应返回空");
    }

    #[test]
    fn test_vector_index_dim() {
        let index = InMemoryVectorIndex::new(1536);
        assert_eq!(index.dim(), 1536);
    }

    #[test]
    fn test_vector_index_batch_add() {
        let index = InMemoryVectorIndex::new(3);

        let items = vec![
            ("h1".into(), "m1".into(), vec![1.0, 0.0, 0.0]),
            ("h2".into(), "m2".into(), vec![0.0, 1.0, 0.0]),
            ("h3".into(), "m3".into(), vec![0.0, 0.0, 1.0]),
        ];
        index.add_batch(items);

        assert_eq!(index.len(), 3);

        let results = index.search(&[1.0, 0.0, 0.0], 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].hook_id, "h1");
    }

    #[test]
    fn test_vector_index_concurrent_safe() {
        use std::sync::Arc;
        use std::thread;

        let index = Arc::new(InMemoryVectorIndex::new(3));

        let mut handles = Vec::new();
        for i in 0..4 {
            let idx = index.clone();
            handles.push(thread::spawn(move || {
                for j in 0..10 {
                    idx.add(
                        &format!("h-{}-{}", i, j),
                        &format!("m-{}-{}", i, j),
                        vec![1.0, i as f32, j as f32],
                    );
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(index.len(), 40);

        // 并发查询
        let results = index.search(&[1.0, 0.0, 0.0], 5);
        assert!(!results.is_empty());
    }

    #[test]
    fn test_vector_index_high_dim() {
        // 高维向量（模拟 OpenAI 1536 维）
        let dim = 1536;
        let index = InMemoryVectorIndex::new(dim);

        let mut v1 = vec![0.0; dim];
        v1[0] = 1.0;
        let mut v2 = vec![0.0; dim];
        v2[1] = 1.0;

        index.add("h1", "m1", v1.clone());
        index.add("h2", "m2", v2);

        let results = index.search(&v1, 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].hook_id, "h1");
        assert!((results[0].score - 1.0).abs() < 1e-5);
    }
}
