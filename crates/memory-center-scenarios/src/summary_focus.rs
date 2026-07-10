//! # 摘要 focus 枚举 + 7 套摘要模板
//!
//! 摘要 prompt 优先级链：
//! 用户 custom_summary_template > SummaryFocus 预设模板 > 默认硬编码模板
//!
//! ## 设计原则
//!
//! 不同场景关注的信息维度不同，摘要模板引导 LLM 提取场景相关的事实。
//! 例如编码场景关注"代码片段/技术决策"，科研场景关注"假设/方法/数据/结论"。

use crate::scenario::Scenario;
use serde::{Deserialize, Serialize};

/// 摘要 focus（对应 7 场景 + General 兜底）
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SummaryFocus {
    /// 编码场景：代码片段/技术决策/bug 修复/架构变更
    Coding,
    /// 写作场景：观点/论据/素材/结构
    Writing,
    /// 科研场景：假设/方法/数据/结论/引用
    Research,
    /// 日常场景：事件/地点/人物/情感
    Daily,
    /// 金融场景：交易/金额/时间/风险/收益
    Finance,
    /// 设计场景：设计决策/用户反馈/迭代版本
    Design,
    /// 工作场景：会议决议/待办/文档变更
    OfficeWork,
    /// Agent 协作场景：Agent 决策/工具调用/上下文迁移/协作流程
    AgentCollaboration,
    /// 知识库场景：知识主题/定义/分类/引用/标签
    KnowledgeBase,
    /// 长项目场景：项目阶段/里程碑/决策/风险/待办
    LongProject,
    /// 通用兜底（默认，Custom 场景降级为此）
    General,
}

impl SummaryFocus {
    /// 从场景推导摘要 focus
    ///
    /// Custom 场景降级为 General
    pub fn from_scenario(scenario: &Scenario) -> Self {
        match scenario {
            Scenario::Coding => Self::Coding,
            Scenario::Writing => Self::Writing,
            Scenario::Research => Self::Research,
            Scenario::Daily => Self::Daily,
            Scenario::Finance => Self::Finance,
            Scenario::Design => Self::Design,
            Scenario::OfficeWork => Self::OfficeWork,
            Scenario::AgentCollaboration => Self::AgentCollaboration,
            Scenario::KnowledgeBase => Self::KnowledgeBase,
            Scenario::LongProject => Self::LongProject,
            Scenario::Custom(_) => Self::General,
        }
    }

    /// 返回该 focus 关注的信息维度列表
    pub fn focus_dimensions(&self) -> &'static [&'static str] {
        match self {
            Self::Coding => &["代码片段", "技术决策", "bug 修复", "架构变更", "依赖变更"],
            Self::Writing => &["核心观点", "论据", "素材", "文章结构", "风格"],
            Self::Research => &["假设", "研究方法", "实验数据", "结论", "引用文献"],
            Self::Daily => &["事件", "地点", "人物", "时间", "情感"],
            Self::Finance => &["交易明细", "金额", "时间", "风险", "收益", "标的"],
            Self::Design => &["设计决策", "用户反馈", "迭代版本", "视觉要素", "交互流程"],
            Self::OfficeWork => &["会议决议", "待办事项", "文档变更", "责任人", "截止日期"],
            Self::AgentCollaboration => &["Agent决策", "工具调用", "上下文迁移", "协作流程", "会话边界"],
            Self::KnowledgeBase => &["知识主题", "定义", "分类", "引用", "标签"],
            Self::LongProject => &["项目阶段", "里程碑", "决策", "风险", "待办"],
            Self::General => &["主题", "关键事实", "关键实体"],
        }
    }

    /// 中文显示名
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Coding => "编码",
            Self::Writing => "写作",
            Self::Research => "科研",
            Self::Daily => "日常",
            Self::Finance => "金融",
            Self::Design => "设计",
            Self::OfficeWork => "工作",
            Self::AgentCollaboration => "Agent协作",
            Self::KnowledgeBase => "知识库",
            Self::LongProject => "长项目",
            Self::General => "通用",
        }
    }
}

impl Default for SummaryFocus {
    fn default() -> Self {
        Self::General
    }
}

/// 获取场景对应的摘要 prompt 模板
///
/// 优先级链：用户自定义 > 场景预设模板 > 默认硬编码
///
/// 模板中的 `{conversation}` 占位符由调用方（HttpSummaryGenerator）替换为实际对话内容
pub fn summary_template_for(scenario: &Scenario, custom_template: Option<&str>) -> String {
    if let Some(tpl) = custom_template {
        return tpl.to_string();
    }
    let focus = SummaryFocus::from_scenario(scenario);
    preset_template(&focus)
}

/// 预设模板（场景维度引导）
fn preset_template(focus: &SummaryFocus) -> String {
    let dims = focus.focus_dimensions();
    let dims_str = dims.join(" / ");
    let focus_name = focus.display_name();

    format!(
        r#"你是一个记忆摘要生成器。请为以下{focus_name}场景对话生成结构化摘要。

摘要要求：
- title: 一句话标题（≤30 字），概括对话主题
- abstract: 2-3 句话的摘要，提炼核心内容
- key_facts: 2-5 条关键事实（可被直接引用的陈述）
- key_entities: 1-5 个关键实体（人名/项目名/技术名词等）

场景关注维度：{dims_str}

对话内容：
{{conversation}}

请以严格 JSON 格式返回（不要包含其他文本）：
{{"title": "标题", "abstract": "摘要", "key_facts": ["事实1"], "key_entities": ["实体1"]}}"#,
        focus_name = focus_name,
        dims_str = dims_str,
    )
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_scenario_coding() {
        let focus = SummaryFocus::from_scenario(&Scenario::Coding);
        assert_eq!(focus, SummaryFocus::Coding);
    }

    #[test]
    fn test_from_scenario_custom_falls_back_general() {
        let focus = SummaryFocus::from_scenario(&Scenario::Custom("xxx".into()));
        assert_eq!(focus, SummaryFocus::General);
    }

    #[test]
    fn test_priority_chain_custom_overrides_preset() {
        let custom = "我的自定义模板 {conversation}";
        let tpl = summary_template_for(&Scenario::Coding, Some(custom));
        assert_eq!(tpl, custom);
    }

    #[test]
    fn test_priority_chain_preset_when_no_custom() {
        let tpl = summary_template_for(&Scenario::Coding, None);
        assert!(tpl.contains("编码"));
        assert!(tpl.contains("代码片段"));
        assert!(tpl.contains("{conversation}"));
    }

    #[test]
    fn test_focus_dimensions_finance() {
        let focus = SummaryFocus::Finance;
        let dims = focus.focus_dimensions();
        assert!(dims.contains(&"交易明细"));
        assert!(dims.contains(&"金额"));
        assert!(dims.contains(&"风险"));
    }

    #[test]
    fn test_focus_dimensions_research() {
        let focus = SummaryFocus::Research;
        let dims = focus.focus_dimensions();
        assert!(dims.contains(&"假设"));
        assert!(dims.contains(&"引用文献"));
    }

    #[test]
    fn test_preset_template_all_scenarios() {
        // 所有内置场景都应能生成模板
        for s in Scenario::all_builtin() {
            let tpl = summary_template_for(&s, None);
            assert!(!tpl.is_empty());
            assert!(tpl.contains("{conversation}"));
        }
    }

    #[test]
    fn test_display_name() {
        assert_eq!(SummaryFocus::Coding.display_name(), "编码");
        assert_eq!(SummaryFocus::General.display_name(), "通用");
    }

    #[test]
    fn test_default_is_general() {
        assert_eq!(SummaryFocus::default(), SummaryFocus::General);
    }
}
