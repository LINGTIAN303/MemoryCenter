//! # CombinedProfile（组合后的最终配置）
//!
//! 由 [`crate::PresetBuilder::build`] 生成，包含：
//! - 5 个原始 Profile（可选，用于追溯）
//! - 解析后的最终生效值（archive_threshold / summary_template / session_prefix 等）

use hippocampus_agents::AgentProfile;
use hippocampus_core::model::Tag;
use hippocampus_models::ModelVariant;
use hippocampus_scenarios::{
    RetrievalStrategy, ScoreWeights, ScenarioProfile, SummaryFocus,
};
use hippocampus_skills::SkillProfile;
use hippocampus_windows::WindowProfile;
use serde::{Deserialize, Serialize};

/// 默认归档阈值（token 数）
pub const DEFAULT_ARCHIVE_THRESHOLD: usize = 400_000;

/// 默认摘要模板（兜底，当 scenario 和 user 都未指定时）
pub const DEFAULT_SUMMARY_TEMPLATE: &str = r#"你是一个记忆摘要生成器。请为以下对话生成结构化摘要。

摘要要求：
- title: 一句话标题（≤30 字），概括对话主题
- abstract: 2-3 句话的摘要，提炼核心内容
- key_facts: 2-5 条关键事实（可被直接引用的陈述）
- key_entities: 1-5 个关键实体（人名/项目名/技术名词等）

对话内容：
{conversation}

请以严格 JSON 格式返回（不要包含其他文本）：
{{"title": "标题", "abstract": "摘要", "key_facts": ["事实1"], "key_entities": ["实体1"]}}"#;

/// 组合后的最终配置
///
/// 由 [`crate::PresetBuilder::build`] 生成，不可直接构造。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CombinedProfile {
    // ========================================================================
    // 原始 5 个 Profile（可选，用于追溯）
    // ========================================================================

    /// 模型型号（可选）
    #[serde(default)]
    pub model: Option<ModelVariant>,
    /// 场景配置（可选）
    #[serde(default)]
    pub scenario: Option<ScenarioProfile>,
    /// 窗口配置（可选，可能由 Agent 联动推导）
    #[serde(default)]
    pub window: Option<WindowProfile>,
    /// Agent 配置（可选）
    #[serde(default)]
    pub agent: Option<AgentProfile>,
    /// 技能列表（可为空）
    #[serde(default)]
    pub skills: Vec<SkillProfile>,

    // ========================================================================
    // 解析后的最终生效值（叠加引擎产出）
    // ========================================================================

    /// 归档阈值（token 数）
    ///
    /// 优先级：用户 > scenario > model > 默认 400K
    archive_threshold: usize,

    /// 摘要模板（含 {conversation} 占位符）
    ///
    /// 优先级：用户 > scenario.custom_summary_template > SummaryFocus 预设 > 默认硬编码
    summary_template: String,

    /// session ID 前缀（来自 Agent，可选）
    session_prefix: Option<String>,

    /// 是否归档到 Hippocampus
    ///
    /// Agent 和 Window 任一显式禁用则不归档
    archive_to_hippocampus: bool,
}

impl CombinedProfile {
    /// 构造最终配置（仅 [`crate::PresetBuilder::build`] 调用）
    pub(crate) fn new(
        model: Option<ModelVariant>,
        scenario: Option<ScenarioProfile>,
        window: Option<WindowProfile>,
        agent: Option<AgentProfile>,
        skills: Vec<SkillProfile>,
        archive_threshold: usize,
        summary_template: String,
        session_prefix: Option<String>,
        archive_to_hippocampus: bool,
    ) -> Self {
        Self {
            model,
            scenario,
            window,
            agent,
            skills,
            archive_threshold,
            summary_template,
            session_prefix,
            archive_to_hippocampus,
        }
    }

    /// 归档阈值（token 数）
    pub fn archive_threshold(&self) -> usize {
        self.archive_threshold
    }

    /// 摘要模板（含 {conversation} 占位符）
    pub fn summary_template(&self) -> &str {
        &self.summary_template
    }

    /// session ID 前缀（来自 Agent，可选）
    pub fn session_prefix(&self) -> Option<&str> {
        self.session_prefix.as_deref()
    }

    /// 是否归档到 Hippocampus
    pub fn archive_to_hippocampus(&self) -> bool {
        self.archive_to_hippocampus
    }

    /// 评分权重（来自 Scenario，可选）
    pub fn score_weights(&self) -> Option<&ScoreWeights> {
        self.scenario.as_ref().map(|s| &s.score_weights)
    }

    /// 检索策略（来自 Scenario，可选）
    pub fn retrieval_strategy(&self) -> Option<&RetrievalStrategy> {
        self.scenario.as_ref().map(|s| &s.retrieval_strategy)
    }

    /// 优先级标签（来自 Scenario，可选）
    pub fn priority_tags(&self) -> Option<&Vec<Tag>> {
        self.scenario.as_ref().map(|s| &s.priority_tags)
    }

    /// 摘要 focus（来自 Scenario，可选）
    pub fn summary_focus(&self) -> Option<&SummaryFocus> {
        self.scenario.as_ref().map(|s| &s.summary_focus)
    }

    /// 是否启用某个技能
    pub fn is_skill_enabled(&self, skill_name: &str) -> bool {
        self.skills
            .iter()
            .any(|s| s.enabled && s.skill.display_name() == skill_name)
    }

    /// 查找技能配置
    pub fn find_skill(&self, skill_name: &str) -> Option<&SkillProfile> {
        self.skills
            .iter()
            .find(|s| s.skill.display_name() == skill_name)
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use hippocampus_agents::AgentProfile;
    use hippocampus_scenarios::{Scenario, ScenarioProfile};

    #[test]
    fn test_default_constants() {
        assert_eq!(DEFAULT_ARCHIVE_THRESHOLD, 400_000);
        assert!(DEFAULT_SUMMARY_TEMPLATE.contains("{conversation}"));
        assert!(DEFAULT_SUMMARY_TEMPLATE.contains("title"));
    }

    #[test]
    fn test_combined_profile_accessors() {
        let combined = CombinedProfile::new(
            None,
            Some(ScenarioProfile::from_scenario(Scenario::Coding)),
            None,
            Some(AgentProfile::claude_code()),
            Vec::new(),
            500_000,
            "custom template {conversation}".into(),
            Some("claude-code".into()),
            true,
        );

        assert_eq!(combined.archive_threshold(), 500_000);
        assert_eq!(combined.summary_template(), "custom template {conversation}");
        assert_eq!(combined.session_prefix(), Some("claude-code"));
        assert!(combined.archive_to_hippocampus());
        assert!(combined.score_weights().is_some());
        assert!(combined.retrieval_strategy().is_some());
        assert!(combined.priority_tags().is_some());
        assert!(combined.summary_focus().is_some());
    }

    #[test]
    fn test_combined_profile_none_optionals() {
        let combined = CombinedProfile::new(
            None,
            None,
            None,
            None,
            Vec::new(),
            DEFAULT_ARCHIVE_THRESHOLD,
            DEFAULT_SUMMARY_TEMPLATE.to_string(),
            None,
            true,
        );

        assert_eq!(combined.archive_threshold(), DEFAULT_ARCHIVE_THRESHOLD);
        assert!(combined.score_weights().is_none());
        assert!(combined.retrieval_strategy().is_none());
        assert!(combined.priority_tags().is_none());
        assert!(combined.summary_focus().is_none());
        assert!(combined.session_prefix().is_none());
    }

    #[test]
    fn test_is_skill_enabled() {
        use hippocampus_skills::{BuiltinSkill, SkillProfile};

        let skills = vec![
            SkillProfile::new(BuiltinSkill::Read),
            SkillProfile::new(BuiltinSkill::Bash).with_disabled(),
        ];
        let combined = CombinedProfile::new(
            None,
            None,
            None,
            None,
            skills,
            DEFAULT_ARCHIVE_THRESHOLD,
            DEFAULT_SUMMARY_TEMPLATE.into(),
            None,
            true,
        );

        assert!(combined.is_skill_enabled("读取文件")); // Read enabled
        assert!(!combined.is_skill_enabled("执行命令")); // Bash disabled
        assert!(!combined.is_skill_enabled("不存在"));
    }

    #[test]
    fn test_find_skill() {
        use hippocampus_skills::{BuiltinSkill, SkillProfile};

        let skills = vec![SkillProfile::new(BuiltinSkill::Read).with_note("test")];
        let combined = CombinedProfile::new(
            None,
            None,
            None,
            None,
            skills,
            DEFAULT_ARCHIVE_THRESHOLD,
            DEFAULT_SUMMARY_TEMPLATE.into(),
            None,
            true,
        );

        let found = combined.find_skill("读取文件");
        assert!(found.is_some());
        assert_eq!(found.unwrap().note.as_deref(), Some("test"));
    }

    #[test]
    fn test_serialize_deserialize() {
        let combined = CombinedProfile::new(
            None,
            None,
            None,
            Some(AgentProfile::claude_code()),
            Vec::new(),
            400_000,
            "template {conversation}".into(),
            Some("claude-code".into()),
            true,
        );

        let json = serde_json::to_string(&combined).unwrap();
        let back: CombinedProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.archive_threshold(), 400_000);
        assert_eq!(back.session_prefix(), Some("claude-code"));
    }
}
