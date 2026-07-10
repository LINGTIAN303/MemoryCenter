//! # 工作场景枚举
//!
//! 7 个内置场景 + Custom 兜底。
//!
//! 场景识别由调用方（Agent 框架/前端）决定，本 crate 仅提供场景对应的特配参数。

use serde::{Deserialize, Serialize};

/// 工作场景
///
/// 对应不同场景的针对化记忆工作流程：
/// - 摘要 focus 不同
/// - 评分权重不同
/// - 标签优先级不同
/// - 检索策略不同
/// - 归档阈值不同
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
pub enum Scenario {
    /// 编码场景：编程/调试/架构设计/code review
    Coding,
    /// 写作场景：文章/文档/创意写作
    Writing,
    /// 科研场景：论文/实验/数据分析
    Research,
    /// 日常场景：闲聊/咨询/生活
    Daily,
    /// 金融场景：交易/投资/风险分析
    Finance,
    /// 设计场景：UI/UX/视觉/产品设计
    Design,
    /// 工作场景：会议/文档/项目协作
    OfficeWork,
    /// Agent 协作场景：多 Agent 共享记忆/跨 session 检索
    AgentCollaboration,
    /// 知识库场景：长期知识积累/评分偏向访问频率
    KnowledgeBase,
    /// 长项目场景：跨数周/月的项目/评分偏向时效性+用户标记
    LongProject,
    /// 自定义场景（兜底）
    Custom(String),
}

impl Scenario {
    /// 返回所有内置场景（不含 Custom）
    pub fn all_builtin() -> [Self; 10] {
        [
            Self::Coding,
            Self::Writing,
            Self::Research,
            Self::Daily,
            Self::Finance,
            Self::Design,
            Self::OfficeWork,
            Self::AgentCollaboration,
            Self::KnowledgeBase,
            Self::LongProject,
        ]
    }

    /// 中文显示名
    pub fn display_name(&self) -> String {
        match self {
            Self::Coding => "编码场景".to_string(),
            Self::Writing => "写作场景".to_string(),
            Self::Research => "科研场景".to_string(),
            Self::Daily => "日常场景".to_string(),
            Self::Finance => "金融场景".to_string(),
            Self::Design => "设计场景".to_string(),
            Self::OfficeWork => "工作场景".to_string(),
            Self::AgentCollaboration => "Agent协作场景".to_string(),
            Self::KnowledgeBase => "知识库场景".to_string(),
            Self::LongProject => "长项目场景".to_string(),
            Self::Custom(s) => s.clone(),
        }
    }

    /// 是否为内置场景
    pub fn is_builtin(&self) -> bool {
        !matches!(self, Self::Custom(_))
    }
}

impl std::fmt::Display for Scenario {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

impl Default for Scenario {
    fn default() -> Self {
        // 默认日常场景（最通用）
        Self::Daily
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_builtin_count() {
        assert_eq!(Scenario::all_builtin().len(), 10);
    }

    #[test]
    fn test_display_name_coding() {
        assert_eq!(Scenario::Coding.display_name(), "编码场景");
    }

    #[test]
    fn test_display_name_custom() {
        assert_eq!(
            Scenario::Custom("游戏场景".into()).display_name(),
            "游戏场景"
        );
    }

    #[test]
    fn test_is_builtin() {
        assert!(Scenario::Coding.is_builtin());
        assert!(!Scenario::Custom("xxx".into()).is_builtin());
    }

    #[test]
    fn test_default_is_daily() {
        assert_eq!(Scenario::default(), Scenario::Daily);
    }

    #[test]
    fn test_serialize_deserialize() {
        let s = Scenario::Finance;
        let json = serde_json::to_string(&s).unwrap();
        let de: Scenario = serde_json::from_str(&json).unwrap();
        assert_eq!(s, de);
    }
}
