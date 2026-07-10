//! # 4 维评分权重
//!
//! 对应月级 4 维加权评分（MemoryCenter-core 的 Scorer trait）：
//! 1. 时效性（recency）
//! 2. 访问频率（access_frequency）
//! 3. 主题相关性（topic_relevance）
//! 4. 用户显式标记（user_marked）
//!
//! 不同场景权重不同，权重之和应 = 1.0。
//!
//! ## 场景权重设计原则
//!
//! - Coding/Research：topic_relevance 最高（代码/研究主题相关性重要）
//! - Daily：recency 最高（近期事件重要）
//! - Finance：topic_relevance + user_marked（交易决策关键）
//! - Design：user_marked 最高（设计迭代主观性强）
//! - OfficeWork：recency + access_frequency（近期待办重要）

use crate::scenario::Scenario;
use serde::{Deserialize, Serialize};

/// 4 维评分权重
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoreWeights {
    /// 时效性权重（0-1）
    pub recency: f32,
    /// 访问频率权重（0-1）
    pub access_frequency: f32,
    /// 主题相关性权重（0-1）
    pub topic_relevance: f32,
    /// 用户显式标记权重（0-1）
    pub user_marked: f32,
}

impl ScoreWeights {
    /// 默认均衡权重（各 0.25）
    pub const fn balanced() -> Self {
        Self {
            recency: 0.25,
            access_frequency: 0.25,
            topic_relevance: 0.25,
            user_marked: 0.25,
        }
    }

    /// 从场景推导权重
    pub fn from_scenario(scenario: &Scenario) -> Self {
        match scenario {
            // 编码：主题相关性最重要（代码相关性），用户标记次之
            Scenario::Coding => Self {
                recency: 0.15,
                access_frequency: 0.15,
                topic_relevance: 0.50,
                user_marked: 0.20,
            },
            // 写作：主题相关性 + 用户标记
            Scenario::Writing => Self {
                recency: 0.15,
                access_frequency: 0.15,
                topic_relevance: 0.40,
                user_marked: 0.30,
            },
            // 科研：主题相关性最高（研究主题稳定）
            Scenario::Research => Self {
                recency: 0.10,
                access_frequency: 0.20,
                topic_relevance: 0.50,
                user_marked: 0.20,
            },
            // 日常：时效性最高（近期事件重要）
            Scenario::Daily => Self {
                recency: 0.50,
                access_frequency: 0.20,
                topic_relevance: 0.15,
                user_marked: 0.15,
            },
            // 金融：主题相关性 + 用户标记（交易决策关键）
            Scenario::Finance => Self {
                recency: 0.20,
                access_frequency: 0.15,
                topic_relevance: 0.35,
                user_marked: 0.30,
            },
            // 设计：用户标记 + 主题相关性（设计迭代主观性强）
            Scenario::Design => Self {
                recency: 0.15,
                access_frequency: 0.15,
                topic_relevance: 0.35,
                user_marked: 0.35,
            },
            // 工作场景：时效性 + 访问频率（近期待办重要）
            Scenario::OfficeWork => Self {
                recency: 0.35,
                access_frequency: 0.25,
                topic_relevance: 0.20,
                user_marked: 0.20,
            },
            // Agent 协作：访问频率最高（跨 Agent 频繁访问的记忆重要）+ 主题相关性
            Scenario::AgentCollaboration => Self {
                recency: 0.15,
                access_frequency: 0.40,
                topic_relevance: 0.30,
                user_marked: 0.15,
            },
            // 知识库：访问频率最高（常用知识重要）+ 用户标记
            Scenario::KnowledgeBase => Self {
                recency: 0.10,
                access_frequency: 0.35,
                topic_relevance: 0.25,
                user_marked: 0.30,
            },
            // 长项目：时效性 + 用户标记（近期里程碑 + 用户标记的决策重要）
            Scenario::LongProject => Self {
                recency: 0.35,
                access_frequency: 0.15,
                topic_relevance: 0.20,
                user_marked: 0.30,
            },
            // 自定义：均衡
            Scenario::Custom(_) => Self::balanced(),
        }
    }

    /// 校验权重之和是否接近 1.0
    pub fn validate(&self) -> Result<(), String> {
        let sum = self.recency + self.access_frequency + self.topic_relevance + self.user_marked;
        if (sum - 1.0).abs() > 0.01 {
            return Err(format!("权重之和应为 1.0，实际为 {:.4}", sum));
        }
        Ok(())
    }

    /// 加权评分
    ///
    /// 各维度分值应为 0-1
    pub fn weighted_score(&self, recency: f32, access: f32, relevance: f32, user: f32) -> f32 {
        self.recency * recency
            + self.access_frequency * access
            + self.topic_relevance * relevance
            + self.user_marked * user
    }
}

impl Default for ScoreWeights {
    fn default() -> Self {
        Self::balanced()
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_balanced_weights_sum_to_one() {
        let w = ScoreWeights::balanced();
        assert!(w.validate().is_ok());
    }

    #[test]
    fn test_coding_weights_topic_relevance_highest() {
        let w = ScoreWeights::from_scenario(&Scenario::Coding);
        assert!(w.topic_relevance > w.recency);
        assert!(w.topic_relevance > w.access_frequency);
        assert!(w.topic_relevance > w.user_marked);
        assert!(w.validate().is_ok());
    }

    #[test]
    fn test_daily_weights_recency_highest() {
        let w = ScoreWeights::from_scenario(&Scenario::Daily);
        assert!(w.recency > w.topic_relevance);
        assert!(w.recency > w.user_marked);
        assert!(w.validate().is_ok());
    }

    #[test]
    fn test_design_weights_user_marked_highest() {
        let w = ScoreWeights::from_scenario(&Scenario::Design);
        assert!(w.user_marked >= w.topic_relevance);
        assert!(w.validate().is_ok());
    }

    #[test]
    fn test_all_builtin_weights_valid() {
        for s in Scenario::all_builtin() {
            let w = ScoreWeights::from_scenario(&s);
            assert!(w.validate().is_ok(), "{} 权重不合法", s.display_name());
        }
    }

    #[test]
    fn test_custom_weights_balanced() {
        let w = ScoreWeights::from_scenario(&Scenario::Custom("xxx".into()));
        assert_eq!(w, ScoreWeights::balanced());
    }

    #[test]
    fn test_weighted_score() {
        let w = ScoreWeights::balanced();
        let score = w.weighted_score(1.0, 0.5, 0.8, 0.0);
        // 0.25*(1.0+0.5+0.8+0.0) = 0.575
        assert!((score - 0.575).abs() < 0.001);
    }

    #[test]
    fn test_validate_failure() {
        let w = ScoreWeights {
            recency: 0.5,
            access_frequency: 0.5,
            topic_relevance: 0.5,
            user_marked: 0.5,
        };
        assert!(w.validate().is_err());
    }
}
