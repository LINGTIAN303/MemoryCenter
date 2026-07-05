//! # Agent 家族枚举（family / variant 分离设计）
//!
//! family 稳定（本模块维护），variant 高频迭代（字符串保存）。
//!
//! ## 11 个主流 Agent family
//!
//! 4 主流（有完整预设）：ClaudeCode / Cursor / Trae / Codex
//! 7 待补（generic 预设）：Zcode / OpenCode / Qoder / WorkBuddy / CatPaw / OpenClaw / Marvis
//! 1 兜底：Custom(String)

use serde::{Deserialize, Serialize};

/// Agent 代理工具家族（稳定枚举）
///
/// 11 个主流 Agent + Custom 兜底。variant（型号）由 [`crate::AgentProfile`]
/// 单独保存为字符串，避免 family 枚举频繁变动。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
pub enum AgentFamily {
    /// Anthropic Claude Code CLI（有 /compact 命令）
    ClaudeCode,
    /// Cursor IDE（chat 压缩机制）
    Cursor,
    /// ByteDance Trae IDE（conversation 压缩）
    Trae,
    /// OpenAI Codex CLI（rolling 压缩，无摘要）
    Codex,
    /// Zcode
    Zcode,
    /// OpenCode
    OpenCode,
    /// Qoder
    Qoder,
    /// WorkBuddy
    WorkBuddy,
    /// CatPaw
    CatPaw,
    /// OpenClaw
    OpenClaw,
    /// Marvis
    Marvis,
    /// 用户自定义兜底（支持未来扩展）
    Custom(String),
}

impl AgentFamily {
    /// 返回所有内置 family（11 个，不含 Custom）
    pub fn all_builtin() -> Vec<Self> {
        vec![
            Self::ClaudeCode,
            Self::Cursor,
            Self::Trae,
            Self::Codex,
            Self::Zcode,
            Self::OpenCode,
            Self::Qoder,
            Self::WorkBuddy,
            Self::CatPaw,
            Self::OpenClaw,
            Self::Marvis,
        ]
    }

    /// 是否为 4 主流之一（有完整预设）
    pub fn is_mainstream(&self) -> bool {
        matches!(
            self,
            Self::ClaudeCode | Self::Cursor | Self::Trae | Self::Codex
        )
    }

    /// 是否为内置 family（非 Custom）
    pub fn is_builtin(&self) -> bool {
        !matches!(self, Self::Custom(_))
    }

    /// 中文显示名（用于 UI 展示 / 日志）
    pub fn display_name(&self) -> &str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Cursor => "Cursor",
            Self::Trae => "Trae",
            Self::Codex => "Codex",
            Self::Zcode => "Zcode",
            Self::OpenCode => "OpenCode",
            Self::Qoder => "Qoder",
            Self::WorkBuddy => "WorkBuddy",
            Self::CatPaw => "CatPaw",
            Self::OpenClaw => "OpenClaw",
            Self::Marvis => "Marvis",
            Self::Custom(s) => s,
        }
    }

    /// 从字符串解析（与 [`display_name`](Self::display_name) 互逆）
    ///
    /// 大小写敏感，Custom 不参与解析（调用方自行构造 `Custom(String)`）
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "Claude Code" => Some(Self::ClaudeCode),
            "Cursor" => Some(Self::Cursor),
            "Trae" => Some(Self::Trae),
            "Codex" => Some(Self::Codex),
            "Zcode" => Some(Self::Zcode),
            "OpenCode" => Some(Self::OpenCode),
            "Qoder" => Some(Self::Qoder),
            "WorkBuddy" => Some(Self::WorkBuddy),
            "CatPaw" => Some(Self::CatPaw),
            "OpenClaw" => Some(Self::OpenClaw),
            "Marvis" => Some(Self::Marvis),
            _ => None,
        }
    }

    /// 默认 session ID 前缀（用于按 Agent 隔离记忆）
    ///
    /// 4 主流有专用前缀，其他返回 family 小写名
    pub fn default_session_prefix(&self) -> &str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Cursor => "cursor",
            Self::Trae => "trae",
            Self::Codex => "codex",
            Self::Zcode => "zcode",
            Self::OpenCode => "opencode",
            Self::Qoder => "qoder",
            Self::WorkBuddy => "workbuddy",
            Self::CatPaw => "catpaw",
            Self::OpenClaw => "openclaw",
            Self::Marvis => "marvis",
            Self::Custom(_) => "custom",
        }
    }

    /// 返回该 family 的识别指纹（v2.30 新增）
    ///
    /// 用于 MCP server 启动时自动识别 Agent 客户端：
    /// - `client_info_keywords`：MCP 协议 `initialize` 请求中 `client_info.name` 的匹配关键词（小写）
    /// - `parent_process_keywords`：父进程名匹配关键词（小写）
    /// - `env_var_prefixes`：环境变量名前缀指纹
    ///
    /// 4 主流 family 有专属指纹，其他返回空数组（generic）。
    /// 详见 `hippocampus_presets::detect::detect_agent_client`。
    pub fn fingerprint(&self) -> AgentFingerprint {
        match self {
            Self::ClaudeCode => AgentFingerprint {
                client_info_keywords: &["claude-code", "claude_code", "claudecode"],
                parent_process_keywords: &["claude", "claude-code"],
                env_var_prefixes: &["CLAUDE_CODE_", "CLAUDE_"],
            },
            Self::Cursor => AgentFingerprint {
                client_info_keywords: &["cursor"],
                parent_process_keywords: &["cursor"],
                env_var_prefixes: &["CURSOR_"],
            },
            Self::Trae => AgentFingerprint {
                client_info_keywords: &["trae"],
                parent_process_keywords: &["trae"],
                env_var_prefixes: &["TRAE_"],
            },
            Self::Codex => AgentFingerprint {
                client_info_keywords: &["codex", "openai-codex"],
                parent_process_keywords: &["codex"],
                env_var_prefixes: &["CODEX_"],
            },
            _ => AgentFingerprint::generic(),
        }
    }
}

/// Agent 客户端识别指纹（v2.30 新增）
///
/// 用于 MCP server 启动时自动识别拉起自己的 Agent 客户端。
/// 3 层信号融合：MCP 协议 client_info → 父进程名 → 环境变量前缀。
///
/// ## 字段说明
///
/// - `client_info_keywords`：MCP `initialize` 请求 `client_info.name` 的小写匹配关键词
/// - `parent_process_keywords`：父进程名的小写匹配关键词
/// - `env_var_prefixes`：环境变量名前缀（如 `CLAUDE_CODE_`）
///
/// ## 使用方式
///
/// 通常不直接使用，通过 [`AgentFamily::fingerprint`] 获取，
/// 再由 `hippocampus_presets::detect::detect_agent_client` 进行多信号融合识别。
#[derive(Debug, Clone, Copy, Default)]
pub struct AgentFingerprint {
    /// MCP 协议 `client_info.name` 匹配关键词（小写）
    pub client_info_keywords: &'static [&'static str],
    /// 父进程名匹配关键词（小写）
    pub parent_process_keywords: &'static [&'static str],
    /// 环境变量指纹（变量名前缀）
    pub env_var_prefixes: &'static [&'static str],
}

impl AgentFingerprint {
    /// 通用空指纹（未识别的 family 使用）
    pub const fn generic() -> Self {
        Self {
            client_info_keywords: &[],
            parent_process_keywords: &[],
            env_var_prefixes: &[],
        }
    }

    /// 是否为空指纹（无任何识别信号）
    pub fn is_empty(&self) -> bool {
        self.client_info_keywords.is_empty()
            && self.parent_process_keywords.is_empty()
            && self.env_var_prefixes.is_empty()
    }

    /// 检查 MCP client_info.name 是否匹配（大小写不敏感）
    pub fn matches_client_info(&self, client_info_name: &str) -> bool {
        if self.client_info_keywords.is_empty() {
            return false;
        }
        let lower = client_info_name.to_lowercase();
        self.client_info_keywords
            .iter()
            .any(|kw| lower.contains(kw))
    }

    /// 检查父进程名是否匹配（大小写不敏感）
    pub fn matches_parent_process(&self, parent_process_name: &str) -> bool {
        if self.parent_process_keywords.is_empty() {
            return false;
        }
        let lower = parent_process_name.to_lowercase();
        self.parent_process_keywords
            .iter()
            .any(|kw| lower.contains(kw))
    }

    /// 检查环境变量集合是否包含任一前缀（大小写敏感，环境变量名通常大写）
    pub fn matches_env_vars(&self, mut env_vars: impl Iterator<Item = String>) -> bool {
        if self.env_var_prefixes.is_empty() {
            return false;
        }
        env_vars.any(|name| {
            self.env_var_prefixes
                .iter()
                .any(|prefix| name.starts_with(prefix))
        })
    }
}

impl Default for AgentFamily {
    /// 默认为 Custom("unknown")，强制调用方显式指定
    fn default() -> Self {
        Self::Custom("unknown".to_string())
    }
}

impl std::fmt::Display for AgentFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
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
        assert_eq!(AgentFamily::all_builtin().len(), 11);
    }

    #[test]
    fn test_is_mainstream() {
        assert!(AgentFamily::ClaudeCode.is_mainstream());
        assert!(AgentFamily::Cursor.is_mainstream());
        assert!(AgentFamily::Trae.is_mainstream());
        assert!(AgentFamily::Codex.is_mainstream());
        assert!(!AgentFamily::Zcode.is_mainstream());
        assert!(!AgentFamily::Custom("x".into()).is_mainstream());
    }

    #[test]
    fn test_is_builtin() {
        assert!(AgentFamily::ClaudeCode.is_builtin());
        assert!(AgentFamily::Marvis.is_builtin());
        assert!(!AgentFamily::Custom("x".into()).is_builtin());
    }

    #[test]
    fn test_display_name() {
        assert_eq!(AgentFamily::ClaudeCode.display_name(), "Claude Code");
        assert_eq!(AgentFamily::Cursor.display_name(), "Cursor");
        assert_eq!(AgentFamily::Custom("MyAgent".into()).display_name(), "MyAgent");
    }

    #[test]
    fn test_from_str_roundtrip() {
        for family in AgentFamily::all_builtin() {
            let name = family.display_name();
            let parsed = AgentFamily::from_str(name);
            assert_eq!(parsed, Some(family.clone()), "{} 往返失败", name);
        }
    }

    #[test]
    fn test_from_str_unknown_returns_none() {
        assert!(AgentFamily::from_str("UnknownAgent").is_none());
        assert!(AgentFamily::from_str("").is_none());
    }

    #[test]
    fn test_default_session_prefix() {
        assert_eq!(AgentFamily::ClaudeCode.default_session_prefix(), "claude-code");
        assert_eq!(AgentFamily::Cursor.default_session_prefix(), "cursor");
        assert_eq!(AgentFamily::Trae.default_session_prefix(), "trae");
        assert_eq!(AgentFamily::Codex.default_session_prefix(), "codex");
        assert_eq!(AgentFamily::Zcode.default_session_prefix(), "zcode");
        assert_eq!(
            AgentFamily::Custom("x".into()).default_session_prefix(),
            "custom"
        );
    }

    #[test]
    fn test_default_is_custom_unknown() {
        let f = AgentFamily::default();
        assert!(matches!(f, AgentFamily::Custom(s) if s == "unknown"));
    }

    #[test]
    fn test_serialize_deserialize() {
        let f = AgentFamily::ClaudeCode;
        let json = serde_json::to_string(&f).unwrap();
        let back: AgentFamily = serde_json::from_str(&json).unwrap();
        assert_eq!(f, back);

        let custom = AgentFamily::Custom("MyAgent".into());
        let json = serde_json::to_string(&custom).unwrap();
        let back: AgentFamily = serde_json::from_str(&json).unwrap();
        assert_eq!(custom, back);
    }

    #[test]
    fn test_display_trait() {
        assert_eq!(format!("{}", AgentFamily::ClaudeCode), "Claude Code");
        assert_eq!(format!("{}", AgentFamily::Custom("Foo".into())), "Foo");
    }

    #[test]
    fn test_hash_set_usage() {
        // 验证 AgentFamily 可放入 HashSet（派生了 Hash）
        use std::collections::HashSet;
        let mut set: HashSet<AgentFamily> = HashSet::new();
        set.insert(AgentFamily::ClaudeCode);
        set.insert(AgentFamily::Cursor);
        set.insert(AgentFamily::ClaudeCode); // 重复，不会增加
        assert_eq!(set.len(), 2);
    }

    // =========================================================================
    // v2.30 新增：AgentFingerprint 识别指纹测试
    // =========================================================================

    #[test]
    fn test_fingerprint_mainstream_non_empty() {
        // 4 主流 family 必须有专属指纹
        for family in [AgentFamily::ClaudeCode, AgentFamily::Cursor, AgentFamily::Trae, AgentFamily::Codex] {
            let fp = family.fingerprint();
            assert!(!fp.is_empty(), "{} 指纹为空", family.display_name());
            assert!(!fp.client_info_keywords.is_empty(), "{} client_info_keywords 为空", family.display_name());
            assert!(!fp.parent_process_keywords.is_empty(), "{} parent_process_keywords 为空", family.display_name());
            assert!(!fp.env_var_prefixes.is_empty(), "{} env_var_prefixes 为空", family.display_name());
        }
    }

    #[test]
    fn test_fingerprint_generic_for_non_mainstream() {
        // 7 待补 family 返回空指纹
        for family in [
            AgentFamily::Zcode,
            AgentFamily::OpenCode,
            AgentFamily::Qoder,
            AgentFamily::WorkBuddy,
            AgentFamily::CatPaw,
            AgentFamily::OpenClaw,
            AgentFamily::Marvis,
        ] {
            let fp = family.fingerprint();
            assert!(fp.is_empty(), "{} 不应有指纹", family.display_name());
        }
    }

    #[test]
    fn test_fingerprint_generic_for_custom() {
        let custom = AgentFamily::Custom("MyAgent".into());
        assert!(custom.fingerprint().is_empty());
    }

    #[test]
    fn test_fingerprint_matches_client_info() {
        // Claude Code：多种写法都应匹配
        let fp = AgentFamily::ClaudeCode.fingerprint();
        assert!(fp.matches_client_info("claude-code"));
        assert!(fp.matches_client_info("Claude-Code"));
        assert!(fp.matches_client_info("claudecode"));
        assert!(fp.matches_client_info("claude-code-cli"));
        assert!(!fp.matches_client_info("cursor"));

        // Cursor
        let fp = AgentFamily::Cursor.fingerprint();
        assert!(fp.matches_client_info("cursor"));
        assert!(fp.matches_client_info("Cursor"));
        assert!(!fp.matches_client_info("trae"));

        // Trae
        let fp = AgentFamily::Trae.fingerprint();
        assert!(fp.matches_client_info("trae"));
        assert!(fp.matches_client_info("Trae IDE"));

        // Codex
        let fp = AgentFamily::Codex.fingerprint();
        assert!(fp.matches_client_info("codex"));
        assert!(fp.matches_client_info("openai-codex"));
    }

    #[test]
    fn test_fingerprint_matches_parent_process() {
        let fp = AgentFamily::ClaudeCode.fingerprint();
        assert!(fp.matches_parent_process("claude"));
        assert!(fp.matches_parent_process("claude-code"));
        assert!(fp.matches_parent_process("claude-code.exe"));
        assert!(!fp.matches_parent_process("node"));

        let fp = AgentFamily::Trae.fingerprint();
        assert!(fp.matches_parent_process("trae"));
        assert!(fp.matches_parent_process("Trae.exe"));
    }

    #[test]
    fn test_fingerprint_matches_env_vars() {
        let fp = AgentFamily::ClaudeCode.fingerprint();
        let env_vars = vec!["CLAUDE_CODE_VERSION".to_string(), "PATH".to_string()];
        assert!(fp.matches_env_vars(env_vars.into_iter()));

        let env_vars = vec!["CURSOR_DEBUG_DIR".to_string()];
        assert!(!fp.matches_env_vars(env_vars.into_iter()));

        let fp = AgentFamily::Cursor.fingerprint();
        let env_vars = vec!["CURSOR_DEBUG_DIR".to_string()];
        assert!(fp.matches_env_vars(env_vars.into_iter()));
    }

    #[test]
    fn test_fingerprint_generic_is_empty() {
        let fp = AgentFingerprint::generic();
        assert!(fp.is_empty());
        assert!(!fp.matches_client_info("anything"));
        assert!(!fp.matches_parent_process("anything"));
        assert!(!fp.matches_env_vars(std::iter::empty()));
    }

    #[test]
    fn test_fingerprint_all_mainstream_distinct() {
        // 4 主流的 client_info_keywords 不应有重叠
        let cc = AgentFamily::ClaudeCode.fingerprint();
        let cursor = AgentFamily::Cursor.fingerprint();
        let trae = AgentFamily::Trae.fingerprint();
        let codex = AgentFamily::Codex.fingerprint();

        // Claude Code 的关键词不应匹配其他 family
        for kw in cc.client_info_keywords {
            assert!(!cursor.matches_client_info(kw), "Cursor 误匹配 {}", kw);
            assert!(!trae.matches_client_info(kw), "Trae 误匹配 {}", kw);
            assert!(!codex.matches_client_info(kw), "Codex 误匹配 {}", kw);
        }
        // 反向验证
        for kw in cursor.client_info_keywords {
            assert!(!cc.matches_client_info(kw), "ClaudeCode 误匹配 {}", kw);
        }
    }
}
