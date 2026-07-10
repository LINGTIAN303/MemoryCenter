//! # 检索策略
//!
//! 不同场景适合不同的检索方式：
//! - Coding/Research：Hybrid（BM25 + 语义，代码/术语需要语义理解）
//! - Writing/Daily：BM25Only（关键词足够，语言自然）
//! - Finance/Design：偏 Semantic（语义相似性重要）
//! - OfficeWork：Hybrid（文档/会议需要语义 + 关键词）
//!
//! ## 降级策略
//!
//! 未配置 Embedder 时，所有 Semantic/Hybrid 策略降级为 BM25Only
//! （由 MemoryCenter-search 层处理，本 crate 仅声明意图）

use crate::scenario::Scenario;
use serde::{Deserialize, Serialize};

/// 检索策略
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RetrievalStrategy {
    /// 仅关键词（BM25）
    ///
    /// 默认策略，无 Embedder 时降级为此
    BM25Only,
    /// 仅语义检索（需 Embedder）
    ///
    /// 纯向量相似度，无关键词加权
    Semantic,
    /// 混合检索（BM25 + 语义 RRF 融合）
    ///
    /// bm25_weight + semantic_weight 应 = 1.0
    Hybrid {
        /// BM25 权重（0-1）
        bm25_weight: f32,
        /// 语义权重（0-1）
        semantic_weight: f32,
    },
}

impl RetrievalStrategy {
    /// 默认混合策略（BM25 0.4 + 语义 0.6）
    pub const fn default_hybrid() -> Self {
        Self::Hybrid {
            bm25_weight: 0.4,
            semantic_weight: 0.6,
        }
    }

    /// 从场景推导检索策略
    pub fn from_scenario(scenario: &Scenario) -> Self {
        match scenario {
            // 编码/科研：混合（代码/术语需语义理解，但关键词也重要）
            Scenario::Coding | Scenario::Research => Self::Hybrid {
                bm25_weight: 0.45,
                semantic_weight: 0.55,
            },
            // 写作/日常：仅关键词（语言自然，关键词足够）
            Scenario::Writing | Scenario::Daily => Self::BM25Only,
            // 金融/设计：偏语义（语义相似性重要）
            Scenario::Finance | Scenario::Design => Self::Hybrid {
                bm25_weight: 0.3,
                semantic_weight: 0.7,
            },
            // 工作场景：混合（文档/会议需要语义 + 关键词）
            Scenario::OfficeWork => Self::default_hybrid(),
            // Agent 协作：偏语义（跨 Agent 语义理解重要）
            Scenario::AgentCollaboration => Self::Hybrid {
                bm25_weight: 0.3,
                semantic_weight: 0.7,
            },
            // 知识库：偏语义（知识检索语义相似性重要）
            Scenario::KnowledgeBase => Self::Semantic,
            // 长项目：混合（项目文档需要语义 + 关键词）
            Scenario::LongProject => Self::default_hybrid(),
            // 自定义：默认混合
            Scenario::Custom(_) => Self::default_hybrid(),
        }
    }

    /// 是否需要 Embedder
    pub fn requires_embedder(&self) -> bool {
        matches!(self, Self::Semantic | Self::Hybrid { .. })
    }

    /// 校验权重合法性
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Hybrid {
                bm25_weight,
                semantic_weight,
            } => {
                let sum = bm25_weight + semantic_weight;
                if (sum - 1.0).abs() > 0.01 {
                    return Err(format!("Hybrid 权重之和应为 1.0，实际为 {:.4}", sum));
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

impl Default for RetrievalStrategy {
    fn default() -> Self {
        Self::BM25Only
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_coding_requires_embedder() {
        let s = RetrievalStrategy::from_scenario(&Scenario::Coding);
        assert!(s.requires_embedder());
        assert!(s.validate().is_ok());
    }

    #[test]
    fn test_writing_no_embedder() {
        let s = RetrievalStrategy::from_scenario(&Scenario::Writing);
        assert!(!s.requires_embedder());
    }

    #[test]
    fn test_daily_no_embedder() {
        let s = RetrievalStrategy::from_scenario(&Scenario::Daily);
        assert!(!s.requires_embedder());
    }

    #[test]
    fn test_finance_requires_embedder() {
        let s = RetrievalStrategy::from_scenario(&Scenario::Finance);
        assert!(s.requires_embedder());
        assert!(s.validate().is_ok());
    }

    #[test]
    fn test_all_builtin_strategies_valid() {
        for s in Scenario::all_builtin() {
            let strategy = RetrievalStrategy::from_scenario(&s);
            assert!(strategy.validate().is_ok(), "{} 策略不合法", s.display_name());
        }
    }

    #[test]
    fn test_default_hybrid_valid() {
        let s = RetrievalStrategy::default_hybrid();
        assert!(s.validate().is_ok());
        assert!(s.requires_embedder());
    }

    #[test]
    fn test_custom_uses_default_hybrid() {
        let s = RetrievalStrategy::from_scenario(&Scenario::Custom("xxx".into()));
        assert!(s.requires_embedder());
    }

    #[test]
    fn test_validate_failure_for_invalid_weights() {
        let s = RetrievalStrategy::Hybrid {
            bm25_weight: 0.5,
            semantic_weight: 0.6,
        };
        assert!(s.validate().is_err());
    }
}
