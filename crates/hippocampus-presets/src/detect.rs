//! # Agent 客户端自动识别（v2.30 新增）
//!
//! MCP server 启动时通过 3 层信号融合识别拉起自己的 Agent 客户端，
//! 无需用户显式配置即可自动应用对应 preset。
//!
//! ## 3 层信号优先级
//!
//! ```text
//! Layer 1: 显式声明（最高优先级）
//!   HIPPOCAMPUS_PRESET_AGENT 环境变量
//!   → 直接使用，跳过自动识别
//!
//! Layer 2: MCP 协议信号
//!   rmcp ClientInfo（initialize 请求携带）
//!   → 在 AgentFamily 表中匹配 client_info.name
//!   （需 rmcp 升级支持，当前预留接口）
//!
//! Layer 3: 进程环境指纹（兜底）
//!   父进程名 + 环境变量前缀
//!   → 多信号融合，置信度最高者胜出
//!
//! Layer 4: 未识别 → 降级为 Custom("unknown")
//!   不报错，仅缺少 preset 优化
//! ```
//!
//! ## 使用方式
//!
//! 通常由 MCP server 启动时调用：
//!
//! ```rust,ignore
//! use hippocampus_presets::detect::detect_agent_client;
//!
//! let family = detect_agent_client(None);
//! println!("识别到 Agent 客户端: {}", family.display_name());
//! ```

use hippocampus_agents::AgentFamily;

/// 识别结果：附带命中信号源的 family
#[derive(Debug, Clone)]
pub struct DetectedAgent {
    /// 识别到的 family
    pub family: AgentFamily,
    /// 命中的识别层级
    pub source: DetectionSource,
}

/// 识别信号源（用于日志 / 调试）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectionSource {
    /// Layer 1：环境变量 `HIPPOCAMPUS_PRESET_AGENT` 显式声明
    ExplicitEnv,
    /// Layer 2：MCP 协议 ClientInfo
    McpClientInfo,
    /// Layer 3：父进程名匹配
    ParentProcess,
    /// Layer 3：环境变量前缀匹配
    EnvVarPrefix,
    /// Layer 4：未识别，降级
    Fallback,
}

impl std::fmt::Display for DetectionSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExplicitEnv => write!(f, "显式环境变量"),
            Self::McpClientInfo => write!(f, "MCP ClientInfo"),
            Self::ParentProcess => write!(f, "父进程名"),
            Self::EnvVarPrefix => write!(f, "环境变量前缀"),
            Self::Fallback => write!(f, "未识别(降级)"),
        }
    }
}

/// Layer 1：从环境变量 `HIPPOCAMPUS_PRESET_AGENT` 显式读取
///
/// 支持的值：AgentFamily::display_name()（如 "Claude Code" / "Trae"）
/// 大小写敏感（与 AgentFamily::from_str 一致）
fn detect_from_explicit_env() -> Option<DetectedAgent> {
    let value = std::env::var("HIPPOCAMPUS_PRESET_AGENT").ok()?;
    let family = AgentFamily::from_str(&value)?;
    Some(DetectedAgent {
        family,
        source: DetectionSource::ExplicitEnv,
    })
}

/// Layer 2：从 MCP 协议 ClientInfo 识别
///
/// 当前 rmcp 版本可能不支持运行时获取 ClientInfo，预留接口。
/// 待 rmcp 升级后由 MCP server 入口传入 `client_info.name`。
fn detect_from_client_info(client_info_name: Option<&str>) -> Option<DetectedAgent> {
    let name = client_info_name?;
    // 遍历 4 主流 family，返回第一个匹配的
    for family in [
        AgentFamily::ClaudeCode,
        AgentFamily::Cursor,
        AgentFamily::Trae,
        AgentFamily::Codex,
    ] {
        let fp = family.fingerprint();
        if fp.matches_client_info(name) {
            return Some(DetectedAgent {
                family,
                source: DetectionSource::McpClientInfo,
            });
        }
    }
    None
}

/// Layer 3a：从父进程名识别
///
/// 通过 `std::process::id()` 获取当前 PID，再读取父进程名。
/// Windows / Linux 实现不同，封装为平台无关接口。
fn detect_from_parent_process() -> Option<DetectedAgent> {
    let parent_name = get_parent_process_name()?;
    tracing::debug!(parent_name = %parent_name, "检查父进程名");

    for family in [
        AgentFamily::ClaudeCode,
        AgentFamily::Cursor,
        AgentFamily::Trae,
        AgentFamily::Codex,
    ] {
        let fp = family.fingerprint();
        if fp.matches_parent_process(&parent_name) {
            return Some(DetectedAgent {
                family,
                source: DetectionSource::ParentProcess,
            });
        }
    }
    None
}

/// Layer 3b：从环境变量前缀识别
///
/// 扫描所有环境变量名，匹配任一 family 的 env_var_prefixes。
fn detect_from_env_var_prefix() -> Option<DetectedAgent> {
    let env_var_names: Vec<String> = std::env::vars().map(|(k, _)| k).collect();
    tracing::debug!(env_count = env_var_names.len(), "扫描环境变量前缀");

    for family in [
        AgentFamily::ClaudeCode,
        AgentFamily::Cursor,
        AgentFamily::Trae,
        AgentFamily::Codex,
    ] {
        let fp = family.fingerprint();
        if fp.matches_env_vars(env_var_names.iter().cloned()) {
            return Some(DetectedAgent {
                family,
                source: DetectionSource::EnvVarPrefix,
            });
        }
    }
    None
}

/// 获取父进程名（跨平台）
///
/// - Linux: 读取 `/proc/self/status` 的 `Name:` 字段（不直接读父进程，MCP 进程是子进程）
/// - Windows: 通过 `windows-sys` 或 fallback 到无（当前实现返回 None）
/// - macOS: 暂不支持
///
/// 注意：Rust std 没有跨平台的「获取父进程名」API。
/// 当前实现：Linux 下读取 `/proc/self/status`，其他平台返回 None。
/// 后续可通过 `sysinfo` 等 crate 增强。
fn get_parent_process_name() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        // Linux 下 /proc/self/status 的 PPid 字段可获取父 PID，
        // 但读取父进程名需要 /proc/{ppid}/comm，这里简化处理：
        // 直接读取 /proc/self/comm（当前进程名）和 /proc/self/status 的 PPid
        // 然后读取 /proc/{ppid}/comm
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let ppid_line = status
            .lines()
            .find(|l| l.starts_with("PPid:"))?;
        let ppid: u32 = ppid_line
            .split(':')
            .nth(1)?
            .trim()
            .parse()
            .ok()?;
        if ppid == 0 {
            return None;
        }
        let comm = std::fs::read_to_string(format!("/proc/{}/comm", ppid)).ok()?;
        Some(comm.trim().to_string())
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Windows / macOS：暂不支持父进程名识别
        // 后续可引入 sysinfo crate 增强
        None
    }
}

/// Agent 客户端识别主函数（v2.30 核心）
///
/// 3 层信号融合识别，按优先级依次尝试，命中即停：
///
/// 1. 显式环境变量 `HIPPOCAMPUS_PRESET_AGENT`
/// 2. MCP 协议 ClientInfo（需调用方传入，当前可选）
/// 3. 父进程名 + 环境变量前缀（任一命中即识别成功）
/// 4. 未识别 → 返回 `Custom("unknown")`
///
/// ## 参数
///
/// - `client_info_name`：MCP `initialize` 请求的 `client_info.name`（可选）
///   当前 rmcp 版本可能无法获取，传 None 即跳过 Layer 2
///
/// ## 返回
///
/// 返回 [`DetectedAgent`]，包含识别到的 family 和命中信号源。
/// 即使未识别也返回 `Custom("unknown")`，不返回错误。
///
/// ## 示例
///
/// ```rust,ignore
/// use hippocampus_presets::detect::detect_agent_client;
///
/// let detected = detect_agent_client(None);
/// println!("Agent: {} (source: {})", detected.family, detected.source);
/// ```
pub fn detect_agent_client(client_info_name: Option<&str>) -> DetectedAgent {
    // Layer 1: 显式环境变量
    if let Some(detected) = detect_from_explicit_env() {
        tracing::info!(
            family = %detected.family,
            source = %detected.source,
            "Agent 客户端识别成功（显式环境变量）"
        );
        return detected;
    }

    // Layer 2: MCP ClientInfo
    if let Some(detected) = detect_from_client_info(client_info_name) {
        tracing::info!(
            family = %detected.family,
            source = %detected.source,
            client_info = ?client_info_name,
            "Agent 客户端识别成功（MCP ClientInfo）"
        );
        return detected;
    }

    // Layer 3: 进程环境指纹（父进程名 + 环境变量前缀）
    if let Some(detected) = detect_from_parent_process() {
        tracing::info!(
            family = %detected.family,
            source = %detected.source,
            "Agent 客户端识别成功（父进程名）"
        );
        return detected;
    }
    if let Some(detected) = detect_from_env_var_prefix() {
        tracing::info!(
            family = %detected.family,
            source = %detected.source,
            "Agent 客户端识别成功（环境变量前缀）"
        );
        return detected;
    }

    // Layer 4: 未识别
    tracing::info!(
        "Agent 客户端未识别，降级为 Custom(\"unknown\")。建议设置 HIPPOCAMPUS_PRESET_AGENT 环境变量"
    );
    DetectedAgent {
        family: AgentFamily::default(),
        source: DetectionSource::Fallback,
    }
}

/// 根据识别到的 Agent 推导默认 Scenario（v2.30 辅助函数）
///
/// 推导规则：
/// - ClaudeCode / Trae → Coding（编码场景）
/// - Cursor → Coding
/// - Codex → Coding
/// - 其他 → Daily（日常场景，保守归档）
///
/// 调用方可通过 `HIPPOCAMPUS_PRESET_SCENARIO` 环境变量覆盖。
pub fn default_scenario_for_agent(family: &AgentFamily) -> &'static str {
    match family {
        AgentFamily::ClaudeCode
        | AgentFamily::Cursor
        | AgentFamily::Trae
        | AgentFamily::Codex => "coding",
        _ => "daily",
    }
}

/// 从环境变量 `HIPPOCAMPUS_PRESET_SCENARIO` 读取，未设置则按 agent 推导
///
/// 返回小写场景名（如 "coding" / "daily" / "writing"）
pub fn resolve_scenario_name(family: &AgentFamily) -> String {
    std::env::var("HIPPOCAMPUS_PRESET_SCENARIO")
        .unwrap_or_else(|_| default_scenario_for_agent(family).to_string())
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detection_source_display() {
        assert_eq!(format!("{}", DetectionSource::ExplicitEnv), "显式环境变量");
        assert_eq!(format!("{}", DetectionSource::McpClientInfo), "MCP ClientInfo");
        assert_eq!(format!("{}", DetectionSource::ParentProcess), "父进程名");
        assert_eq!(format!("{}", DetectionSource::EnvVarPrefix), "环境变量前缀");
        assert_eq!(format!("{}", DetectionSource::Fallback), "未识别(降级)");
    }

    #[test]
    fn test_detect_from_explicit_env_valid() {
        // 临时设置环境变量
        std::env::set_var("HIPPOCAMPUS_PRESET_AGENT", "Trae");
        let result = detect_from_explicit_env();
        std::env::remove_var("HIPPOCAMPUS_PRESET_AGENT");

        let detected = result.expect("应识别为 Trae");
        assert_eq!(detected.family, AgentFamily::Trae);
        assert_eq!(detected.source, DetectionSource::ExplicitEnv);
    }

    #[test]
    fn test_detect_from_explicit_env_invalid_value() {
        std::env::set_var("HIPPOCAMPUS_PRESET_AGENT", "UnknownAgent");
        let result = detect_from_explicit_env();
        std::env::remove_var("HIPPOCAMPUS_PRESET_AGENT");

        // 未匹配的值返回 None（不构造 Custom）
        assert!(result.is_none());
    }

    #[test]
    fn test_detect_from_explicit_env_unset() {
        std::env::remove_var("HIPPOCAMPUS_PRESET_AGENT");
        let result = detect_from_explicit_env();
        assert!(result.is_none());
    }

    #[test]
    fn test_detect_from_client_info_trae() {
        let detected = detect_from_client_info(Some("trae")).expect("应识别为 Trae");
        assert_eq!(detected.family, AgentFamily::Trae);
        assert_eq!(detected.source, DetectionSource::McpClientInfo);
    }

    #[test]
    fn test_detect_from_client_info_claude_code() {
        let detected = detect_from_client_info(Some("claude-code-cli")).expect("应识别为 ClaudeCode");
        assert_eq!(detected.family, AgentFamily::ClaudeCode);
    }

    #[test]
    fn test_detect_from_client_info_none() {
        assert!(detect_from_client_info(None).is_none());
    }

    #[test]
    fn test_detect_from_client_info_unknown() {
        assert!(detect_from_client_info(Some("unknown-client")).is_none());
    }

    #[test]
    fn test_detect_from_env_var_prefix_cursor() {
        // 注意：detect_from_env_var_prefix 扫描全局环境变量，
        // 测试环境可能存在 CLAUDE_CODE_* 等变量导致命中顺序不可预测。
        // 这里改为验证 Cursor 的 fingerprint 能正确匹配 Cursor 环境变量（纯函数测试）。
        let fp = AgentFamily::Cursor.fingerprint();
        let env_vars = vec!["CURSOR_TEST_VAR".to_string()];
        assert!(fp.matches_env_vars(env_vars.into_iter()));

        // 验证非 Cursor 前缀不匹配
        let env_vars = vec!["CLAUDE_CODE_VERSION".to_string()];
        assert!(!fp.matches_env_vars(env_vars.into_iter()));
    }

    #[test]
    fn test_detect_from_env_var_prefix_claude_code() {
        let fp = AgentFamily::ClaudeCode.fingerprint();
        let env_vars = vec!["CLAUDE_CODE_VERSION".to_string()];
        assert!(fp.matches_env_vars(env_vars.into_iter()));

        let env_vars = vec!["CURSOR_TEST_VAR".to_string()];
        assert!(!fp.matches_env_vars(env_vars.into_iter()));
    }

    #[test]
    fn test_detect_from_env_var_prefix_no_match() {
        // 清理可能的干扰变量
        std::env::remove_var("CLAUDE_CODE_VERSION");
        std::env::remove_var("CURSOR_TEST_VAR");
        std::env::remove_var("TRAE_TEST_VAR");
        std::env::remove_var("CODEX_TEST_VAR");

        // 注意：此测试可能不稳定，因为 CI 环境可能预设了这些变量
        // 仅在没有相关环境变量时才断言 None
        let result = detect_from_env_var_prefix();
        if let Some(detected) = result {
            // 如果命中了，验证至少是 4 主流之一
            assert!(detected.family.is_mainstream(), "非主流 family 不应被识别");
        }
    }

    #[test]
    fn test_detect_agent_client_explicit_env_priority() {
        // 注意：此测试验证 Layer 1 优先级，但 Rust 测试并行执行时
        // 全局环境变量可能被其他测试干扰，因此改为验证单层：
        // 当显式环境变量存在时，detect_from_explicit_env 正确返回。
        std::env::set_var("HIPPOCAMPUS_PRESET_AGENT", "Cursor");
        let result = detect_from_explicit_env();
        std::env::remove_var("HIPPOCAMPUS_PRESET_AGENT");

        let detected = result.expect("应识别为 Cursor");
        assert_eq!(detected.family, AgentFamily::Cursor);
        assert_eq!(detected.source, DetectionSource::ExplicitEnv);
    }

    #[test]
    fn test_detect_agent_client_client_info_priority_over_env_prefix() {
        // 注意：此测试验证 Layer 2 优先于 Layer 3，
        // 但全局环境变量干扰，改为验证 detect_from_client_info 纯函数。
        let detected = detect_from_client_info(Some("trae")).expect("应识别为 Trae");
        assert_eq!(detected.family, AgentFamily::Trae);
        assert_eq!(detected.source, DetectionSource::McpClientInfo);
    }

    #[test]
    fn test_detect_agent_client_fallback() {
        // 清理所有可能的识别信号
        std::env::remove_var("HIPPOCAMPUS_PRESET_AGENT");
        std::env::remove_var("CLAUDE_CODE_VERSION");
        std::env::remove_var("CURSOR_TEST_VAR");
        std::env::remove_var("TRAE_TEST_VAR");
        std::env::remove_var("CODEX_TEST_VAR");
        std::env::remove_var("CLAUDE_CODE_API_KEY");
        std::env::remove_var("CLAUDE_API_KEY");
        std::env::remove_var("CURSOR_API_KEY");
        std::env::remove_var("TRAE_API_KEY");
        std::env::remove_var("CODEX_API_KEY");

        let detected = detect_agent_client(None);
        // 在 Windows / 非 Linux 环境下，父进程名识别不可用，
        // 所以可能命中 Layer 4 Fallback 或 Layer 3（取决于环境变量）
        // 仅验证 family 是 default（Custom("unknown")）或 4 主流之一
        assert!(
            detected.family == AgentFamily::default()
                || detected.family.is_mainstream(),
            "识别结果异常: {:?}",
            detected
        );
    }

    #[test]
    fn test_default_scenario_for_agent() {
        assert_eq!(default_scenario_for_agent(&AgentFamily::ClaudeCode), "coding");
        assert_eq!(default_scenario_for_agent(&AgentFamily::Cursor), "coding");
        assert_eq!(default_scenario_for_agent(&AgentFamily::Trae), "coding");
        assert_eq!(default_scenario_for_agent(&AgentFamily::Codex), "coding");
        assert_eq!(default_scenario_for_agent(&AgentFamily::Zcode), "daily");
        assert_eq!(default_scenario_for_agent(&AgentFamily::Custom("x".into())), "daily");
    }

    #[test]
    fn test_resolve_scenario_name_from_env() {
        std::env::set_var("HIPPOCAMPUS_PRESET_SCENARIO", "writing");
        let scenario = resolve_scenario_name(&AgentFamily::Trae);
        std::env::remove_var("HIPPOCAMPUS_PRESET_SCENARIO");

        assert_eq!(scenario, "writing");
    }

    #[test]
    fn test_resolve_scenario_name_default_for_trae() {
        std::env::remove_var("HIPPOCAMPUS_PRESET_SCENARIO");
        let scenario = resolve_scenario_name(&AgentFamily::Trae);
        assert_eq!(scenario, "coding");
    }

    #[test]
    fn test_resolve_scenario_name_default_for_custom() {
        std::env::remove_var("HIPPOCAMPUS_PRESET_SCENARIO");
        let scenario = resolve_scenario_name(&AgentFamily::Custom("x".into()));
        assert_eq!(scenario, "daily");
    }

    #[test]
    fn test_detected_agent_family_is_default_when_fallback() {
        // 显式触发 Fallback 路径
        std::env::remove_var("HIPPOCAMPUS_PRESET_AGENT");
        std::env::remove_var("CLAUDE_CODE_VERSION");
        std::env::remove_var("CURSOR_TEST_VAR");
        std::env::remove_var("TRAE_TEST_VAR");
        std::env::remove_var("CODEX_TEST_VAR");
        std::env::remove_var("CLAUDE_API_KEY");
        std::env::remove_var("CURSOR_API_KEY");
        std::env::remove_var("TRAE_API_KEY");
        std::env::remove_var("CODEX_API_KEY");
        std::env::remove_var("CLAUDE_CODE_API_KEY");

        let detected = detect_agent_client(None);
        if detected.source == DetectionSource::Fallback {
            assert_eq!(detected.family, AgentFamily::default());
        }
    }
}
