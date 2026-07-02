//! # 评分模块
//!
//! 可插拔评分架构。
//!
//! ## 4 维加权评分
//!
//! 月级评分淘汰时，对每个周级记忆文件按以下 4 维加权评分：
//!
//! 1. **时效性**：内容是否仍与当前任务/项目相关。过期内容降权。
//!    （需要语义理解，LLM 可插拔实现）
//! 2. **访问频率**：被索引钩子引用/检索命中的次数。常被访问=有价值。
//!    （纯算法，默认实现）
//! 3. **主题相关性**：与用户当前活跃项目/会话主题的匹配度。
//!    （需要语义理解，LLM 可插拔实现）
//! 4. **用户显式标记**：用户手动标记为"重要"的内容加权。
//!    （纯算法，默认实现）
//!
//! ## 架构
//!
//! - [`Scorer`] trait：评分器接口，可插拔
//! - [`DefaultScorer`]：默认启发式实现（关键词/时间衰减/TF-IDF）
//! - LLM 评分作为可选实现（v2 路线图）

use crate::model::MemoryFile;

/// 评分器 trait
///
/// 评分器对记忆文件打分，用于月级评分淘汰。
/// 默认实现使用启发式算法，LLM 评分可作为可选实现。
pub trait Scorer: Send + Sync {
    /// 对记忆文件评分，返回 0.0-100.0 的分数
    fn score(&self, file: &MemoryFile) -> f64;
}

/// 评分权重配置
#[derive(Debug, Clone)]
pub struct ScoreWeights {
    /// 时效性权重（0.0-1.0）
    pub timeliness: f64,
    /// 访问频率权重（0.0-1.0）
    pub access_frequency: f64,
    /// 主题相关性权重（0.0-1.0）
    pub topic_relevance: f64,
    /// 用户显式标记权重（0.0-1.0）
    pub user_marked: f64,
}

impl Default for ScoreWeights {
    fn default() -> Self {
        // 默认权重：四维均等
        Self {
            timeliness: 0.25,
            access_frequency: 0.25,
            topic_relevance: 0.25,
            user_marked: 0.25,
        }
    }
}

/// 默认启发式评分器
///
/// 使用启发式算法：
/// - 时效性：时间衰减（越新分越高）
/// - 访问频率：`access_count` 归一化
/// - 主题相关性：TODO（P3 用 TF-IDF 实现）
/// - 用户显式标记：`importance` 字段归一化
pub struct DefaultScorer {
    weights: ScoreWeights,
}

impl DefaultScorer {
    /// 用默认权重创建
    pub fn new() -> Self {
        Self { weights: ScoreWeights::default() }
    }

    /// 用自定义权重创建
    pub fn with_weights(weights: ScoreWeights) -> Self {
        Self { weights }
    }
}

impl Default for DefaultScorer {
    fn default() -> Self {
        Self::new()
    }
}

impl Scorer for DefaultScorer {
    fn score(&self, file: &MemoryFile) -> f64 {
        // 时效性：时间衰减（占位实现，P3 完善）
        let timeliness_score = 50.0; // TODO: 基于 archived_at 计算时间衰减

        // 访问频率：归一化（占位实现，P3 完善）
        let access_score = (file.access_count as f64).min(100.0);

        // 主题相关性：TODO（P3 用 TF-IDF 实现）
        let topic_score = 50.0;

        // 用户显式标记：归一化
        let user_score = file.importance as f64;

        let w = &self.weights;
        let total = timeliness_score * w.timeliness
            + access_score * w.access_frequency
            + topic_score * w.topic_relevance
            + user_score * w.user_marked;

        total.min(100.0)
    }
}
