//! # BM25 关键词检索（v2.5 批次 7）
//!
//! 基于 BM25 算法的关键词检索实现，支持中英文混合分词。
//!
//! ## 算法
//!
//! BM25（Best Matching 25）是经典的文本检索算法：
//!
//! ```text
//! score(q, d) = Σ_{qi in q} IDF(qi) * (f(qi, d) * (k1 + 1)) /
//!               (f(qi, d) + k1 * (1 - b + b * |d| / avgdl))
//!
//! IDF(qi) = ln((N - n(qi) + 0.5) / (n(qi) + 0.5) + 1)
//! ```
//!
//! - `N`：文档总数
//! - `n(qi)`：包含 qi 的文档数
//! - `f(qi, d)`：qi 在文档 d 中的词频
//! - `|d|`：文档 d 长度（词数）
//! - `avgdl`：平均文档长度
//! - `k1`：词频饱和参数（默认 1.2）
//! - `b`：长度归一化参数（默认 0.75）
//!
//! ## 分词
//!
//! 使用 `jieba-rs` 进行中文分词，英文按空格切分。
//! jieba 词典首次加载约 5MB 内存 + 50ms 启动开销（懒加载）。

use crate::semantic::{KeywordSearcher, RetrievalSource, SearchHit};
use jieba_rs::Jieba;
use std::collections::HashMap;
use std::sync::RwLock;

// ============================================================================
// 分词
// ============================================================================

/// 分词器（jieba 中文分词 + 英文空格切分）
///
/// jieba 首次使用时懒加载词典（约 5MB 内存 + 50ms 开销）。
/// 加载后线程安全，可并发使用。
struct Tokenizer {
    jieba: Jieba,
}

impl Tokenizer {
    fn new() -> Self {
        Self {
            jieba: Jieba::new(),
        }
    }

    /// 分词：中文用 jieba，英文按非字母数字字符切分
    ///
    /// 返回小写化的 token 列表（去停用词）。
    fn tokenize(&self, text: &str) -> Vec<String> {
        let mut tokens = Vec::new();

        // jieba 分词（中英文混合）
        let words = self.jieba.cut(text, false);

        for word in words {
            let word = word.trim();
            if word.is_empty() {
                continue;
            }

            // 中文词：直接保留（长度 >= 1）
            // 英文词：转小写，过滤纯标点
            if word.chars().any(|c| c.is_ascii_alphanumeric()) {
                // 英文/数字：转小写
                tokens.push(word.to_lowercase());
            } else if word.chars().count() >= 1 {
                // 中文词（可能含标点，但 jieba 通常已切分）
                // 过滤纯标点
                if word.chars().any(|c| c.is_alphanumeric()) {
                    tokens.push(word.to_string());
                }
            }
        }

        // 去停用词
        tokens
            .into_iter()
            .filter(|t| !STOPWORDS.contains(&t.as_str()))
            .collect()
    }
}

/// 中文停用词表（常见无意义词）
const STOPWORDS: &[&str] = &[
    "的", "了", "在", "是", "我", "有", "和", "就", "不", "人", "都", "一", "一个",
    "上", "也", "很", "到", "说", "要", "去", "你", "会", "着", "没有", "看", "好",
    "这", "那", "它", "他", "她", "们", "与", "或", "但", "而", "如果", "因为",
    "所以", "但是", "然后", "可以", "什么", "怎么", "为什么", "哪里", "哪个",
    "the", "a", "an", "is", "are", "was", "were", "be", "been", "being",
    "have", "has", "had", "do", "does", "did", "will", "would", "could",
    "should", "may", "might", "must", "can", "to", "of", "in", "on", "at",
    "for", "with", "by", "from", "as", "into", "through", "during",
    "and", "or", "but", "if", "then", "else", "when", "where", "why",
    "how", "all", "any", "both", "each", "few", "more", "most", "other",
    "some", "such", "no", "not", "only", "own", "same", "so", "than",
    "too", "very", "just", "this", "that", "these", "those",
];

// ============================================================================
// Bm25Searcher
// ============================================================================

/// 文档索引项
#[derive(Debug, Clone)]
struct DocEntry {
    /// 记忆文件 ID
    memory_id: String,
    /// 文档长度（词数）
    doc_len: usize,
    /// 词频表：term → tf
    term_freq: HashMap<String, u32>,
}

/// BM25 关键词检索器
///
/// 默认实现 [`KeywordSearcher`] trait。
///
/// ## 参数
///
/// - `k1`：词频饱和参数（1.2-2.0，默认 1.2）
/// - `b`：长度归一化参数（0-1，默认 0.75）
///
/// ## 并发
///
/// 内部用 `RwLock` 保证并发安全：读操作（search）可并发，写操作（index/remove）串行化。
pub struct Bm25Searcher {
    /// BM25 参数 k1
    k1: f64,
    /// BM25 参数 b
    b: f64,
    /// 分词器（线程安全）
    tokenizer: Tokenizer,
    /// 文档索引：hook_id → DocEntry
    docs: RwLock<HashMap<String, DocEntry>>,
    /// 倒排索引：term → [(hook_id, tf)]
    inverted_index: RwLock<HashMap<String, Vec<(String, u32)>>>,
}

impl Bm25Searcher {
    /// 创建新的 BM25 检索器（默认参数 k1=1.2, b=0.75）
    pub fn new() -> Self {
        Self::with_params(1.2, 0.75)
    }

    /// 创建新的 BM25 检索器（自定义参数）
    pub fn with_params(k1: f64, b: f64) -> Self {
        Self {
            k1,
            b,
            tokenizer: Tokenizer::new(),
            docs: RwLock::new(HashMap::new()),
            inverted_index: RwLock::new(HashMap::new()),
        }
    }

    /// 计算 IDF（Inverse Document Frequency）
    ///
    /// `IDF(qi) = ln((N - n(qi) + 0.5) / (n(qi) + 0.5) + 1)`
    ///
    /// 加 1 防止负值（BM25+ 变体）。
    fn idf(&self, n_qi: usize, n_total: usize) -> f64 {
        let numerator = (n_total as f64) - (n_qi as f64) + 0.5;
        let denominator = (n_qi as f64) + 0.5;
        (numerator / denominator + 1.0).ln()
    }
}

impl Default for Bm25Searcher {
    fn default() -> Self {
        Self::new()
    }
}

impl KeywordSearcher for Bm25Searcher {
    fn index(&self, hook_id: &str, memory_id: &str, text: &str) {
        // 分词
        let tokens = self.tokenizer.tokenize(text);
        let doc_len = tokens.len();

        // 统计词频
        let mut term_freq: HashMap<String, u32> = HashMap::new();
        for token in &tokens {
            *term_freq.entry(token.clone()).or_insert(0) += 1;
        }

        // 写入文档索引
        {
            let mut docs = self.docs.write().unwrap();
            // 若已存在，先移除旧索引（避免重复）
            if docs.contains_key(hook_id) {
                drop(docs);
                self.remove(hook_id);
                docs = self.docs.write().unwrap();
            }
            docs.insert(
                hook_id.to_string(),
                DocEntry {
                    memory_id: memory_id.to_string(),
                    doc_len,
                    term_freq: term_freq.clone(),
                },
            );
        }

        // 写入倒排索引
        {
            let mut inverted = self.inverted_index.write().unwrap();
            for (term, tf) in &term_freq {
                inverted
                    .entry(term.clone())
                    .or_insert_with(Vec::new)
                    .push((hook_id.to_string(), *tf));
            }
        }
    }

    fn search(&self, query: &str, top_k: usize) -> Vec<SearchHit> {
        let query_tokens = self.tokenizer.tokenize(query);
        if query_tokens.is_empty() {
            return Vec::new();
        }

        let docs = self.docs.read().unwrap();
        let inverted = self.inverted_index.read().unwrap();

        let n_total = docs.len();
        if n_total == 0 {
            return Vec::new();
        }

        // 计算平均文档长度
        let avgdl: f64 = docs.values().map(|d| d.doc_len as f64).sum::<f64>() / n_total as f64;

        // 对每个文档计算 BM25 分数
        let mut scores: Vec<(String, String, f32)> = Vec::new();

        for (hook_id, doc) in docs.iter() {
            let mut score = 0.0_f64;

            for term in &query_tokens {
                // 词频
                let tf = doc.term_freq.get(term).copied().unwrap_or(0);
                if tf == 0 {
                    continue;
                }

                // 包含该词的文档数
                let n_qi = inverted.get(term).map(|v| v.len()).unwrap_or(0);
                if n_qi == 0 {
                    continue;
                }

                // IDF
                let idf = self.idf(n_qi, n_total);

                // BM25 分数
                let tf_f = tf as f64;
                let doc_len = doc.doc_len as f64;
                let denom = tf_f + self.k1 * (1.0 - self.b + self.b * doc_len / avgdl);
                let term_score = idf * (tf_f * (self.k1 + 1.0)) / denom;

                score += term_score;
            }

            if score > 0.0 {
                scores.push((hook_id.clone(), doc.memory_id.clone(), score as f32));
            }
        }

        // 按分数降序排列，取 top_k
        scores.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        scores.truncate(top_k);

        scores
            .into_iter()
            .map(|(hook_id, memory_id, score)| SearchHit {
                hook_id,
                memory_id,
                score,
                source: RetrievalSource::Keyword,
            })
            .collect()
    }

    fn remove(&self, hook_id: &str) {
        // 从文档索引移除，并获取旧文档的 term_freq
        let old_doc = {
            let mut docs = self.docs.write().unwrap();
            docs.remove(hook_id)
        };

        // 从倒排索引移除对应条目
        if let Some(doc) = old_doc {
            let mut inverted = self.inverted_index.write().unwrap();
            for term in doc.term_freq.keys() {
                if let Some(postings) = inverted.get_mut(term) {
                    postings.retain(|(hid, _)| hid != hook_id);
                    if postings.is_empty() {
                        inverted.remove(term);
                    }
                }
            }
        }
    }

    fn len(&self) -> usize {
        self.docs.read().unwrap().len()
    }

    fn clear(&self) {
        self.docs.write().unwrap().clear();
        self.inverted_index.write().unwrap().clear();
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bm25_basic_search() {
        let searcher = Bm25Searcher::new();

        // 索引 3 个文档
        searcher.index("h1", "m1", "Rust 是一门系统编程语言，强调安全性和性能");
        searcher.index("h2", "m2", "Python 是动态语言，适合数据分析和机器学习");
        searcher.index("h3", "m3", "Rust 的所有权机制保证内存安全");

        // 搜索 "Rust"
        let results = searcher.search("Rust 语言", 3);
        assert!(!results.is_empty(), "应返回结果");

        // h1 和 h3 都含 "Rust"，应排在前面
        let top_ids: Vec<&str> = results.iter().map(|h| h.hook_id.as_str()).collect();
        assert!(top_ids.contains(&"h1") || top_ids.contains(&"h3"));
        assert_eq!(results[0].source, RetrievalSource::Keyword);
    }

    #[test]
    fn test_bm25_chinese_search() {
        let searcher = Bm25Searcher::new();

        searcher.index("h1", "m1", "记忆库是 Agent 的核心组件，负责存储和检索历史对话");
        searcher.index("h2", "m2", "向量检索通过 embedding 实现语义匹配");
        searcher.index("h3", "m3", "Agent 记忆库的归档机制基于 token 阈值触发");

        // 搜索 "记忆库"
        let results = searcher.search("记忆库 Agent", 3);
        assert!(!results.is_empty());

        // h1 和 h3 都含 "记忆库" 和 "Agent"，应排前面
        let top_ids: Vec<&str> = results.iter().map(|h| h.hook_id.as_str()).collect();
        assert!(top_ids.contains(&"h1"));
        assert!(top_ids.contains(&"h3"));
    }

    #[test]
    fn test_bm25_no_match() {
        let searcher = Bm25Searcher::new();
        searcher.index("h1", "m1", "Rust 编程语言");

        let results = searcher.search("Python 数据分析", 5);
        assert!(results.is_empty(), "无匹配应返回空");
    }

    #[test]
    fn test_bm25_remove() {
        let searcher = Bm25Searcher::new();
        searcher.index("h1", "m1", "Rust 语言");
        searcher.index("h2", "m2", "Rust 安全");

        assert_eq!(searcher.len(), 2);

        // 删除 h1
        searcher.remove("h1");
        assert_eq!(searcher.len(), 1);

        // 搜索应只剩 h2
        let results = searcher.search("Rust", 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].hook_id, "h2");
    }

    #[test]
    fn test_bm25_clear() {
        let searcher = Bm25Searcher::new();
        searcher.index("h1", "m1", "测试文档");
        searcher.index("h2", "m2", "另一个文档");

        searcher.clear();
        assert!(searcher.is_empty());

        let results = searcher.search("文档", 5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_bm25_reindex_same_hook() {
        // 同一 hook_id 重新索引应覆盖旧内容
        let searcher = Bm25Searcher::new();
        searcher.index("h1", "m1", "Rust 语言");
        searcher.index("h1", "m1", "Python 语言"); // 覆盖

        assert_eq!(searcher.len(), 1, "应只有 1 个文档");

        // 搜索 Rust 应无结果（已被覆盖）
        let rust_results = searcher.search("Rust", 5);
        assert!(rust_results.is_empty(), "Rust 应已被覆盖");

        // 搜索 Python 应有结果
        let py_results = searcher.search("Python", 5);
        assert!(!py_results.is_empty(), "Python 应可搜到");
    }

    #[test]
    fn test_bm25_empty_query() {
        let searcher = Bm25Searcher::new();
        searcher.index("h1", "m1", "测试文档");

        let results = searcher.search("", 5);
        assert!(results.is_empty(), "空查询应返回空");
    }

    #[test]
    fn test_bm25_top_k_limit() {
        let searcher = Bm25Searcher::new();

        // 索引 5 个含相同词的文档
        for i in 0..5 {
            searcher.index(
                &format!("h{}", i),
                &format!("m{}", i),
                &format!("Rust 编程 测试 {}", i),
            );
        }

        let results = searcher.search("Rust", 3);
        assert_eq!(results.len(), 3, "应返回 top 3");
    }

    #[test]
    fn test_bm25_score_ordering() {
        let searcher = Bm25Searcher::new();

        // h1 含 "Rust" 1 次
        searcher.index("h1", "m1", "Rust 是编程语言");
        // h2 含 "Rust" 3 次（词频更高）
        searcher.index("h2", "m2", "Rust Rust Rust 性能优秀");

        let results = searcher.search("Rust", 2);
        assert_eq!(results.len(), 2);
        // h2 词频更高，应排第一
        assert_eq!(results[0].hook_id, "h2");
        assert!(results[0].score > results[1].score);
    }

    #[test]
    fn test_tokenizer_mixed_content() {
        let tokenizer = Tokenizer::new();

        // 中英文混合
        let tokens = tokenizer.tokenize("Rust 是一门 systems programming 语言");
        assert!(tokens.contains(&"rust".to_string()));
        assert!(tokens.contains(&"systems".to_string()));
        assert!(tokens.contains(&"programming".to_string()));
        assert!(tokens.contains(&"语言".to_string()));
        // "是" 是停用词，应被过滤
        assert!(!tokens.contains(&"是".to_string()));
    }

    #[test]
    fn test_bm25_concurrent_safe() {
        use std::sync::Arc;
        use std::thread;

        let searcher = Arc::new(Bm25Searcher::new());

        let mut handles = Vec::new();
        for i in 0..4 {
            let s = searcher.clone();
            handles.push(thread::spawn(move || {
                for j in 0..10 {
                    s.index(
                        &format!("h-{}-{}", i, j),
                        &format!("m-{}-{}", i, j),
                        &format!("并发测试 Rust {}", j),
                    );
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(searcher.len(), 40);

        // 并发搜索
        let results = searcher.search("Rust", 5);
        assert!(!results.is_empty());
    }
}
