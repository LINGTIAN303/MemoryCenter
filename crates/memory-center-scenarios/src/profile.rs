//! # 场景特配组合（ScenarioProfile）
//!
//! 将 Scenario + SummaryFocus + ScoreWeights + 优先级标签 + 检索策略 + 归档阈值
//! 组合为完整的场景特配。
//!
//! ## 使用方式
//!
//! ```rust,ignore
//! use memory_center_scenarios::{ScenarioProfile, Scenario};
//!
//! // 1. 从场景构建默认 profile
//! let profile = ScenarioProfile::from_scenario(Scenario::Coding);
//!
//! // 2. 自定义摘要模板（覆盖预设）
//! let profile = ScenarioProfile::from_scenario(Scenario::Finance)
//!     .with_custom_summary_template("我的金融摘要模板 {conversation}");
//!
//! // 3. 获取摘要模板（优先级链：自定义 > 预设 > 默认）
//! let template = profile.summary_template();
//! ```

use crate::priority_tags::priority_tags_for;
use crate::retrieval_strategy::RetrievalStrategy;
use crate::scenario::Scenario;
use crate::score_weights::ScoreWeights;
use crate::summary_focus::{SummaryFocus, summary_template_for};
use memory_center_core::model::Tag;
use serde::{Deserialize, Serialize};

/// 场景特配（完整组合）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioProfile {
    /// 场景
    pub scenario: Scenario,
    /// 摘要 focus
    pub summary_focus: SummaryFocus,
    /// 用户自定义摘要模板（覆盖预设，None 时使用预设）
    #[serde(default)]
    pub custom_summary_template: Option<String>,
    /// 评分权重
    pub score_weights: ScoreWeights,
    /// 标签优先级（从高到低）
    #[serde(default)]
    pub priority_tags: Vec<Tag>,
    /// 检索策略
    pub retrieval_strategy: RetrievalStrategy,
    /// 归档阈值（token 数，不同场景可不同）
    pub archive_threshold: usize,
}

impl ScenarioProfile {
    /// 从场景构建默认 profile
    ///
    /// 自动推导：
    /// - summary_focus = SummaryFocus::from_scenario
    /// - score_weights = ScoreWeights::from_scenario
    /// - priority_tags = priority_tags_for
    /// - retrieval_strategy = RetrievalStrategy::from_scenario
    /// - archive_threshold = default_archive_threshold
    pub fn from_scenario(scenario: Scenario) -> Self {
        let summary_focus = SummaryFocus::from_scenario(&scenario);
        let score_weights = ScoreWeights::from_scenario(&scenario);
        let priority_tags = priority_tags_for(&scenario);
        let retrieval_strategy = RetrievalStrategy::from_scenario(&scenario);
        let archive_threshold = default_archive_threshold(&scenario);
        Self {
            scenario,
            summary_focus,
            custom_summary_template: None,
            score_weights,
            priority_tags,
            retrieval_strategy,
            archive_threshold,
        }
    }

    /// 设置自定义摘要模板（覆盖预设）
    pub fn with_custom_summary_template(mut self, template: impl Into<String>) -> Self {
        self.custom_summary_template = Some(template.into());
        self
    }

    /// 获取摘要模板（优先级链：自定义 > 预设 > 默认）
    ///
    /// 模板中的 `{conversation}` 占位符由调用方替换
    pub fn summary_template(&self) -> String {
        summary_template_for(&self.scenario, self.custom_summary_template.as_deref())
    }

    /// 覆盖评分权重
    pub fn with_score_weights(mut self, weights: ScoreWeights) -> Self {
        self.score_weights = weights;
        self
    }

    /// 覆盖检索策略
    pub fn with_retrieval_strategy(mut self, strategy: RetrievalStrategy) -> Self {
        self.retrieval_strategy = strategy;
        self
    }

    /// 覆盖归档阈值
    pub fn with_archive_threshold(mut self, threshold: usize) -> Self {
        self.archive_threshold = threshold;
        self
    }

    /// 追加标签优先级（在预设之后）
    pub fn with_extra_priority_tags(mut self, tags: Vec<Tag>) -> Self {
        self.priority_tags.extend(tags);
        self
    }

    /// 校验合法性
    pub fn validate(&self) -> Result<(), String> {
        self.score_weights.validate()?;
        self.retrieval_strategy.validate()?;
        Ok(())
    }
}

impl Default for ScenarioProfile {
    fn default() -> Self {
        Self::from_scenario(Scenario::default())
    }
}

/// 场景默认归档阈值
///
/// 不同场景对话长度特征不同：
/// - Coding/Research：长对话常见（500K）
/// - Daily：短对话为主（200K）
/// - 其他场景：中等（400K，对齐 core 默认值）
fn default_archive_threshold(scenario: &Scenario) -> usize {
    match scenario {
        // 编码/科研：大窗口（长对话常见）
        Scenario::Coding | Scenario::Research => 500_000,
        // 写作/设计：中等窗口
        Scenario::Writing | Scenario::Design => 400_000,
        // 日常：小窗口（短对话为主）
        Scenario::Daily => 200_000,
        // 金融：中等窗口
        Scenario::Finance => 400_000,
        // 工作场景：中等窗口
        Scenario::OfficeWork => 400_000,
        // Agent 协作：中等窗口（跨 Agent 对话）
        Scenario::AgentCollaboration => 400_000,
        // 知识库：大窗口（知识积累）
        Scenario::KnowledgeBase => 500_000,
        // 长项目：大窗口（跨数周/月）
        Scenario::LongProject => 500_000,
        // 自定义：默认 400K（对齐 core 默认值）
        Scenario::Custom(_) => 400_000,
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_from_coding_scenario() {
        let p = ScenarioProfile::from_scenario(Scenario::Coding);
        assert_eq!(p.summary_focus, SummaryFocus::Coding);
        assert_eq!(p.archive_threshold, 500_000);
        assert!(!p.priority_tags.is_empty());
        assert!(p.validate().is_ok());
    }

    #[test]
    fn test_profile_from_daily_scenario() {
        let p = ScenarioProfile::from_scenario(Scenario::Daily);
        assert_eq!(p.summary_focus, SummaryFocus::Daily);
        assert_eq!(p.archive_threshold, 200_000);
        assert!(p.validate().is_ok());
    }

    #[test]
    fn test_custom_template_overrides_preset() {
        let p = ScenarioProfile::from_scenario(Scenario::Coding)
            .with_custom_summary_template("自定义 {conversation}");
        let tpl = p.summary_template();
        assert_eq!(tpl, "自定义 {conversation}");
    }

    #[test]
    fn test_preset_template_when_no_custom() {
        let p = ScenarioProfile::from_scenario(Scenario::Finance);
        let tpl = p.summary_template();
        assert!(tpl.contains("金融"));
        assert!(tpl.contains("交易明细"));
    }

    #[test]
    fn test_all_builtin_profiles_valid() {
        for s in Scenario::all_builtin() {
            let p = ScenarioProfile::from_scenario(s);
            assert!(p.validate().is_ok());
        }
    }

    #[test]
    fn test_daily_small_threshold() {
        let p = ScenarioProfile::from_scenario(Scenario::Daily);
        assert!(p.archive_threshold < 400_000);
    }

    #[test]
    fn test_coding_large_threshold() {
        let p = ScenarioProfile::from_scenario(Scenario::Coding);
        assert!(p.archive_threshold > 400_000);
    }

    #[test]
    fn test_with_score_weights_override() {
        let custom_weights = ScoreWeights {
            recency: 0.4,
            access_frequency: 0.3,
            topic_relevance: 0.2,
            user_marked: 0.1,
        };
        let p = ScenarioProfile::from_scenario(Scenario::Coding).with_score_weights(custom_weights);
        assert!((p.score_weights.recency - 0.4).abs() < 0.001);
        assert!(p.validate().is_ok());
    }

    #[test]
    fn test_with_retrieval_strategy_override() {
        let p = ScenarioProfile::from_scenario(Scenario::Writing)
            .with_retrieval_strategy(RetrievalStrategy::Semantic);
        assert!(p.retrieval_strategy.requires_embedder());
    }

    #[test]
    fn test_with_extra_priority_tags() {
        let p = ScenarioProfile::from_scenario(Scenario::Coding)
            .with_extra_priority_tags(vec![Tag::Voice, Tag::Video]);
        // 原始 6 + 追加 2 = 8
        assert_eq!(p.priority_tags.len(), 8);
    }

    #[test]
    fn test_default_profile_is_daily() {
        let p = ScenarioProfile::default();
        assert_eq!(p.scenario, Scenario::Daily);
    }

    #[test]
    fn test_serialize_deserialize() {
        let p = ScenarioProfile::from_scenario(Scenario::Coding);
        let json = serde_json::to_string(&p).unwrap();
        let de: ScenarioProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(p.scenario, de.scenario);
        assert_eq!(p.archive_threshold, de.archive_threshold);
    }
}
