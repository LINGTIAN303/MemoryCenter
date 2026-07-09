//! # 钩子模式（HookMode）与解析器（v2.40 新增）
//!
//! 根据 Agent family 自动决定运转模式：
//! - **真钩子（Real）**：开源 Agent（OpenCode/ClaudeCode），sidecar 进程自动监听压缩事件，
//!   LLM 无需感知 token 消耗
//! - **伪钩子（Pseudo）**：闭源 Agent（Trae/Cursor/Codex 等），LLM 自感知 token，
//!   主动调 archive 工具归档
//!
//! ## 设计目标
//!
//! 1. **专属适配**：不同 Agent 工具按自身特性选择最合适的归档机制
//! 2. **互不干扰**：真钩子 Agent 的 LLM 不需要看 token 反馈，伪钩子 Agent 的 LLM 不依赖 sidecar
//! 3. **可追溯**：SessionMeta 持久化 hook_mode 字符串，历史记忆可查"当时用什么模式归档的"
//!
//! ## 与 SessionMeta 的关系
//!
//! SessionMeta（core-logic crate）只存 `hook_mode: String`，不依赖本 crate。
//! 上层（mcp/server）调用 [`HookModeResolver::resolve`] 得到 [`HookMode`]，
//! 再用 [`HookMode::as_str`] 转为字符串写入 SessionMeta。

use serde::{Deserialize, Serialize};

/// 钩子模式枚举（v2.40 新增）
///
/// 决定 Agent 的归档触发机制：
/// - [`HookMode::Real`]：sidecar 进程自动归档（开源 Agent 专用）
/// - [`HookMode::Pseudo`]：LLM 自感知 token 主动归档（闭源 Agent 专用）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookMode {
    /// 真钩子：sidecar 进程自动监听压缩事件，LLM 无需感知 token
    ///
    /// 适用 Agent：OpenCode（sidecar 监听 compaction 消息）、ClaudeCode（可改源码）
    Real,

    /// 伪钩子：LLM 自感知 token 消耗，主动调 archive 工具归档
    ///
    /// 适用 Agent：Trae / Cursor / Codex / 其他闭源 Agent
    Pseudo,
}

impl HookMode {
    /// 转为稳定字符串（用于 SessionMeta.hook_mode 字段持久化）
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Real => "real",
            Self::Pseudo => "pseudo",
        }
    }

    /// 从字符串解析（与 [`as_str`](Self::as_str) 互逆）
    ///
    /// 用于读取旧 SessionMeta 时反序列化，未知值返回 None（调用方降级为 Pseudo）
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "real" => Some(Self::Real),
            "pseudo" => Some(Self::Pseudo),
            _ => None,
        }
    }

    /// 中文显示名（用于 prompt / 日志）
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Real => "真钩子（sidecar 自动归档）",
            Self::Pseudo => "伪钩子（LLM 自感知归档）",
        }
    }
}

impl Default for HookMode {
    /// 默认为伪钩子（闭源 Agent 更常见，降级安全）
    fn default() -> Self {
        Self::Pseudo
    }
}

impl std::fmt::Display for HookMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// 钩子模式解析器（v2.40 新增）
///
/// 根据 AgentFamily 自动决定 HookMode，并提供 session_id 反解能力。
pub struct HookModeResolver;

impl HookModeResolver {
    /// 根据 Agent family 解析钩子模式
    ///
    /// - `OpenCode` / `ClaudeCode` → [`HookMode::Real`]（开源，sidecar 可适配）
    /// - 其他 → [`HookMode::Pseudo`]（闭源，LLM 自感知）
    pub fn resolve(family: &crate::AgentFamily) -> HookMode {
        if family.supports_real_hook() {
            HookMode::Real
        } else {
            HookMode::Pseudo
        }
    }

    /// 从 session_id 前缀反解 Agent family
    ///
    /// 用于读取旧 session（无 agent_family 字段）时补全 Agent 信息。
    ///
    /// ## 匹配规则
    ///
    /// session_id 约定为 `{prefix}-{project}-{date}`，其中 prefix 可能含连字符
    /// （如 `claude-code`）。遍历所有 family 的 `default_session_prefix()`，
    /// 检查 session_id 是否以 `{prefix}-` 开头。
    ///
    /// ## 示例
    ///
    /// - `"opencode-myapp-20260709"` → `Some(AgentFamily::OpenCode)`
    /// - `"trae-myapp-20260709"` → `Some(AgentFamily::Trae)`
    /// - `"claude-code-myapp-20260709"` → `Some(AgentFamily::ClaudeCode)`（prefix 含连字符）
    /// - `"custom-xxx"` → `None`（custom 前缀无法确定具体 family）
    /// - `"myapp-session"` → `None`（无前缀）
    pub fn family_from_session_id(session_id: &str) -> Option<crate::AgentFamily> {
        if session_id.is_empty() {
            return None;
        }
        // 遍历所有 builtin family，检查 session_id 是否以 `{prefix}-` 开头
        // 注意：必须检查 prefix 后跟 `-`，避免 `trae` 误匹配 `traexxx-`
        crate::AgentFamily::all_builtin()
            .into_iter()
            .find(|f| {
                let prefix = f.default_session_prefix();
                session_id.starts_with(prefix)
                    && session_id
                        .get(prefix.len()..)
                        .map(|rest| rest.starts_with('-') || rest.is_empty())
                        .unwrap_or(false)
            })
    }

    /// 从 session_id 推导 hook_mode（便捷方法）
    ///
    /// 等价于先调 [`family_from_session_id`](Self::family_from_session_id) 再调
    /// [`resolve`](Self::resolve)，若无法识别 family 则返回 [`HookMode::Pseudo`]。
    pub fn mode_from_session_id(session_id: &str) -> HookMode {
        match Self::family_from_session_id(session_id) {
            Some(family) => Self::resolve(&family),
            None => HookMode::Pseudo,
        }
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentFamily;

    #[test]
    fn test_hook_mode_as_str_roundtrip() {
        for mode in [HookMode::Real, HookMode::Pseudo] {
            let s = mode.as_str();
            let back = HookMode::from_str(s);
            assert_eq!(back, Some(mode), "{} 往返失败", s);
        }
    }

    #[test]
    fn test_hook_mode_from_unknown() {
        assert!(HookMode::from_str("unknown").is_none());
        assert!(HookMode::from_str("").is_none());
    }

    #[test]
    fn test_hook_mode_default_is_pseudo() {
        assert_eq!(HookMode::default(), HookMode::Pseudo);
    }

    #[test]
    fn test_resolve_opensource_to_real() {
        assert_eq!(HookModeResolver::resolve(&AgentFamily::OpenCode), HookMode::Real);
        assert_eq!(HookModeResolver::resolve(&AgentFamily::ClaudeCode), HookMode::Real);
    }

    #[test]
    fn test_resolve_closedsource_to_pseudo() {
        assert_eq!(HookModeResolver::resolve(&AgentFamily::Trae), HookMode::Pseudo);
        assert_eq!(HookModeResolver::resolve(&AgentFamily::Cursor), HookMode::Pseudo);
        assert_eq!(HookModeResolver::resolve(&AgentFamily::Codex), HookMode::Pseudo);
        assert_eq!(
            HookModeResolver::resolve(&AgentFamily::Custom("x".into())),
            HookMode::Pseudo
        );
    }

    #[test]
    fn test_family_from_session_id_known_prefixes() {
        assert_eq!(
            HookModeResolver::family_from_session_id("opencode-myapp-20260709"),
            Some(AgentFamily::OpenCode)
        );
        assert_eq!(
            HookModeResolver::family_from_session_id("trae-myapp-20260709"),
            Some(AgentFamily::Trae)
        );
        assert_eq!(
            HookModeResolver::family_from_session_id("cursor-myapp-20260709"),
            Some(AgentFamily::Cursor)
        );
        assert_eq!(
            HookModeResolver::family_from_session_id("claude-code-myapp-20260709"),
            Some(AgentFamily::ClaudeCode)
        );
        assert_eq!(
            HookModeResolver::family_from_session_id("codex-myapp-20260709"),
            Some(AgentFamily::Codex)
        );
    }

    #[test]
    fn test_family_from_session_id_unknown() {
        assert!(HookModeResolver::family_from_session_id("custom-xxx").is_none());
        assert!(HookModeResolver::family_from_session_id("myapp-session").is_none());
        assert!(HookModeResolver::family_from_session_id("").is_none());
        assert!(HookModeResolver::family_from_session_id("unknown-20260709").is_none());
    }

    #[test]
    fn test_mode_from_session_id() {
        // 开源 Agent 前缀 → Real
        assert_eq!(
            HookModeResolver::mode_from_session_id("opencode-myapp-20260709"),
            HookMode::Real
        );
        assert_eq!(
            HookModeResolver::mode_from_session_id("claude-code-myapp-20260709"),
            HookMode::Real
        );

        // 闭源 Agent 前缀 → Pseudo
        assert_eq!(
            HookModeResolver::mode_from_session_id("trae-myapp-20260709"),
            HookMode::Pseudo
        );

        // 未识别前缀 → Pseudo（降级安全）
        assert_eq!(
            HookModeResolver::mode_from_session_id("unknown-20260709"),
            HookMode::Pseudo
        );
    }

    #[test]
    fn test_serde_roundtrip() {
        let mode = HookMode::Real;
        let json = serde_json::to_string(&mode).unwrap();
        assert_eq!(json, "\"real\"");
        let back: HookMode = serde_json::from_str(&json).unwrap();
        assert_eq!(back, mode);

        let mode = HookMode::Pseudo;
        let json = serde_json::to_string(&mode).unwrap();
        assert_eq!(json, "\"pseudo\"");
    }

    #[test]
    fn test_display() {
        assert_eq!(format!("{}", HookMode::Real), "real");
        assert_eq!(format!("{}", HookMode::Pseudo), "pseudo");
    }
}
