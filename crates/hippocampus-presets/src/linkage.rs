//! # 联动规则（Agent → Window 自动推导）
//!
//! 当用户设置了 Agent 但未设置 Window 时，根据 Agent family 自动推导 WindowProfile。

use hippocampus_agents::{AgentFamily, AgentProfile};
use hippocampus_windows::WindowProfile;

/// 根据 Agent family 推导默认 WindowProfile
///
/// ## 映射表
///
/// | Agent | Window | 压缩方式 | trigger_threshold |
/// |---|---|---|---|
/// | ClaudeCode | claude_code() | ClaudeCodeCompact | 180K |
/// | Cursor | cursor() | CursorChat | 150K |
/// | Trae | trae() | TraeConversation | 120K |
/// | Codex | codex() | CodexRolling | 100K |
/// | 其他 | default() | GenericSliding | 400K |
pub fn derive_window_from_agent(agent: &AgentProfile) -> WindowProfile {
    match agent.family {
        AgentFamily::ClaudeCode => WindowProfile::claude_code(),
        AgentFamily::Cursor => WindowProfile::cursor(),
        AgentFamily::Trae => WindowProfile::trae(),
        AgentFamily::Codex => WindowProfile::codex(),
        _ => WindowProfile::default(),
    }
}

/// 判断是否需要联动推导（Agent 已设置 + Window 未设置）
pub fn should_derive_window(agent: Option<&AgentProfile>, window: Option<&WindowProfile>) -> bool {
    agent.is_some() && window.is_none()
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_claude_code() {
        let agent = AgentProfile::claude_code();
        let window = derive_window_from_agent(&agent);
        assert!(window.validate().is_ok());
        // claude_code 预设的 trigger_threshold = 180K
        assert_eq!(window.trigger_threshold, 180_000);
    }

    #[test]
    fn test_derive_cursor() {
        let agent = AgentProfile::cursor();
        let window = derive_window_from_agent(&agent);
        assert_eq!(window.trigger_threshold, 150_000);
    }

    #[test]
    fn test_derive_trae() {
        let agent = AgentProfile::trae();
        let window = derive_window_from_agent(&agent);
        assert_eq!(window.trigger_threshold, 120_000);
    }

    #[test]
    fn test_derive_codex() {
        let agent = AgentProfile::codex();
        let window = derive_window_from_agent(&agent);
        assert_eq!(window.trigger_threshold, 100_000);
    }

    #[test]
    fn test_derive_generic_for_other_builtin() {
        // 非 4 主流的内置 family 应返回 default（GenericSliding, 100K）
        let agent = AgentProfile::generic(AgentFamily::Zcode);
        let window = derive_window_from_agent(&agent);
        // default = GenericSliding { keep_recent_turns: 5, summary_on_compress: true }
        // trigger_threshold = 100_000（GenericSliding 默认）
        assert_eq!(window.trigger_threshold, 100_000);
    }

    #[test]
    fn test_derive_generic_for_custom() {
        let agent = AgentProfile::generic(AgentFamily::Custom("MyAgent".into()));
        let window = derive_window_from_agent(&agent);
        assert!(window.validate().is_ok());
    }

    #[test]
    fn test_should_derive_window_when_agent_set_window_none() {
        let agent = AgentProfile::claude_code();
        assert!(should_derive_window(Some(&agent), None));
    }

    #[test]
    fn test_should_not_derive_when_window_set() {
        let agent = AgentProfile::claude_code();
        let window = WindowProfile::default();
        assert!(!should_derive_window(Some(&agent), Some(&window)));
    }

    #[test]
    fn test_should_not_derive_when_agent_none() {
        assert!(!should_derive_window(None, None));
        assert!(!should_derive_window(None, Some(&WindowProfile::default())));
    }

    #[test]
    fn test_all_builtin_agents_derive_valid_window() {
        for family in AgentFamily::all_builtin() {
            let agent = AgentProfile::generic(family);
            let window = derive_window_from_agent(&agent);
            assert!(
                window.validate().is_ok(),
                "{} 推导的 window 校验失败",
                agent.family
            );
        }
    }
}
