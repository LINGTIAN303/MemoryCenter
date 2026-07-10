//! # 标签优先级
//!
//! 对应 core 的 17 类 [`Tag`]，不同场景优先保留的标签类型不同。
//! 用于月级评分淘汰时，高优先级标签的记忆文件加分。
//!
//! ## 场景优先级设计
//!
//! - Coding：CodeBlock > ToolCall > Thinking > Plan（代码相关优先）
//! - Writing：Text > Citation > Url（文本与引用优先）
//! - Research：Citation > Url > Text（引用文献优先）
//! - Daily：Text > Image > Voice（多媒体生活记录）
//! - Finance：CodeBlock > Text > Status（交易代码与状态）
//! - Design：Image > Text > FileAttachment（设计稿优先）
//! - OfficeWork：FileAttachment > Text > Plan（文档与待办优先）

use crate::scenario::Scenario;
use memory_center_core::model::Tag;

/// 返回场景对应的标签优先级（从高到低）
///
/// 不在列表中的标签优先级最低（分值 0.0）
pub fn priority_tags_for(scenario: &Scenario) -> Vec<Tag> {
    match scenario {
        // 编码：代码块 > 工具调用 > 思考过程 > 计划 > URL（文档链接）
        Scenario::Coding => vec![
            Tag::CodeBlock,
            Tag::ToolCall,
            Tag::Thinking,
            Tag::Plan,
            Tag::Url,
            Tag::FileAttachment,
        ],
        // 写作：文本 > 引用 > URL > 计划
        Scenario::Writing => vec![Tag::Text, Tag::Citation, Tag::Url, Tag::Plan, Tag::FileAttachment],
        // 科研：引用 > URL > 文本 > 计划 > 思考过程
        Scenario::Research => vec![
            Tag::Citation,
            Tag::Url,
            Tag::Text,
            Tag::Plan,
            Tag::Thinking,
            Tag::FileAttachment,
        ],
        // 日常：文本 > 图片 > 语音 > URL
        Scenario::Daily => vec![Tag::Text, Tag::Image, Tag::Voice, Tag::Url, Tag::Video],
        // 金融：代码块（交易代码）> 文本 > URL > 状态
        Scenario::Finance => vec![Tag::CodeBlock, Tag::Text, Tag::Url, Tag::Status, Tag::Citation],
        // 设计：图片 > 文本 > 文件附件 > UI
        Scenario::Design => vec![Tag::Image, Tag::Text, Tag::FileAttachment, Tag::Ui, Tag::Video],
        // 工作场景：文件附件 > 文本 > 计划 > 状态 > URL
        Scenario::OfficeWork => vec![
            Tag::FileAttachment,
            Tag::Text,
            Tag::Plan,
            Tag::Status,
            Tag::Url,
            Tag::Citation,
        ],
        // Agent 协作：工具调用 > 思考过程 > 代码块 > 文本 > 计划
        Scenario::AgentCollaboration => vec![
            Tag::ToolCall,
            Tag::Thinking,
            Tag::CodeBlock,
            Tag::Text,
            Tag::Plan,
        ],
        // 知识库：引用 > 文本 > URL > 代码块 > 文件附件
        Scenario::KnowledgeBase => vec![
            Tag::Citation,
            Tag::Text,
            Tag::Url,
            Tag::CodeBlock,
            Tag::FileAttachment,
        ],
        // 长项目：计划 > 状态 > 文件附件 > 文本 > 代码块
        Scenario::LongProject => vec![
            Tag::Plan,
            Tag::Status,
            Tag::FileAttachment,
            Tag::Text,
            Tag::CodeBlock,
        ],
        // 自定义：空列表（不优先任何标签）
        Scenario::Custom(_) => Vec::new(),
    }
}

/// 计算标签的优先级分值（0-1）
///
/// - 在优先级列表中：1.0 - (idx / len)
/// - 不在列表中：0.0
/// - Custom 场景：0.0（不优先）
pub fn tag_priority_score(tag: &Tag, scenario: &Scenario) -> f32 {
    let priorities = priority_tags_for(scenario);
    if priorities.is_empty() {
        return 0.0;
    }
    let len = priorities.len() as f32;
    for (idx, t) in priorities.iter().enumerate() {
        if t == tag {
            return 1.0 - (idx as f32 / len);
        }
    }
    0.0
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_coding_priority_codeblock_first() {
        let tags = priority_tags_for(&Scenario::Coding);
        assert_eq!(tags.first(), Some(&Tag::CodeBlock));
    }

    #[test]
    fn test_research_priority_citation_first() {
        let tags = priority_tags_for(&Scenario::Research);
        assert_eq!(tags.first(), Some(&Tag::Citation));
    }

    #[test]
    fn test_design_priority_image_first() {
        let tags = priority_tags_for(&Scenario::Design);
        assert_eq!(tags.first(), Some(&Tag::Image));
    }

    #[test]
    fn test_office_work_priority_file_attachment_first() {
        let tags = priority_tags_for(&Scenario::OfficeWork);
        assert_eq!(tags.first(), Some(&Tag::FileAttachment));
    }

    #[test]
    fn test_custom_empty_priority() {
        let tags = priority_tags_for(&Scenario::Custom("xxx".into()));
        assert!(tags.is_empty());
    }

    #[test]
    fn test_tag_priority_score_high_for_first() {
        // Coding 场景第一个是 CodeBlock，分值应接近 1.0
        let score = tag_priority_score(&Tag::CodeBlock, &Scenario::Coding);
        assert!(score > 0.8);
        assert!(score <= 1.0);
    }

    #[test]
    fn test_tag_priority_score_decreasing() {
        // 优先级递减：第一个 > 第二个
        let first = tag_priority_score(&Tag::CodeBlock, &Scenario::Coding);
        let second = tag_priority_score(&Tag::ToolCall, &Scenario::Coding);
        assert!(first > second);
    }

    #[test]
    fn test_tag_priority_score_zero_for_not_in_list() {
        // Voice 不在 Coding 优先级列表中
        let score = tag_priority_score(&Tag::Voice, &Scenario::Coding);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_tag_priority_score_zero_for_custom() {
        let score = tag_priority_score(&Tag::Text, &Scenario::Custom("xxx".into()));
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_all_builtin_scenarios_have_non_empty_priorities() {
        for s in Scenario::all_builtin() {
            let tags = priority_tags_for(&s);
            assert!(!tags.is_empty(), "{} 优先级列表为空", s.display_name());
        }
    }
}
