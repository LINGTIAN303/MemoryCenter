//! # PresetBuilder（预设构造器 + 叠加引擎）
//!
//! 链式收集 5 个可选 Profile + 用户覆盖参数，调用 [`build`](PresetBuilder::build)
//! 时应用联动机制 + 优先级链，生成 [`CombinedProfile`]。

use crate::combined::{CombinedProfile, DEFAULT_ARCHIVE_THRESHOLD, DEFAULT_SUMMARY_TEMPLATE, TriggerRule, UsageProtocol};
use crate::linkage::{derive_window_from_agent, should_derive_window};
use hippocampus_agents::AgentProfile;
use hippocampus_models::ModelVariant;
use hippocampus_scenarios::{Scenario, ScenarioProfile, SummaryFocus};
use hippocampus_skills::SkillProfile;
use hippocampus_windows::WindowProfile;

/// 预设构造器错误
#[derive(Debug, thiserror::Error)]
pub enum PresetError {
    /// Profile 校验失败
    #[error("{0} 校验失败: {1}")]
    ValidateFailed(&'static str, String),
}

/// 预设构造器
///
/// ## 使用示例
///
/// ```rust,ignore
/// use hippocampus_presets::PresetBuilder;
/// use hippocampus_agents::AgentProfile;
/// use hippocampus_scenarios::{ScenarioProfile, Scenario};
///
/// let combined = PresetBuilder::new()
///     .with_agent(AgentProfile::claude_code())
///     .with_scenario(ScenarioProfile::from_scenario(Scenario::Coding))
///     .with_user_archive_threshold(450_000)  // 用户覆盖
///     .build()
///     .unwrap();
/// ```
#[derive(Debug, Clone, Default)]
pub struct PresetBuilder {
    /// 模型型号（可选）
    model: Option<ModelVariant>,
    /// 场景配置（可选）
    scenario: Option<ScenarioProfile>,
    /// 窗口配置（可选，未设置时由 Agent 联动推导）
    window: Option<WindowProfile>,
    /// Agent 配置（可选）
    agent: Option<AgentProfile>,
    /// 技能列表（可为空）
    skills: Vec<SkillProfile>,
    /// 用户覆盖：归档阈值
    user_archive_threshold: Option<usize>,
    /// 用户覆盖：摘要模板
    user_summary_template: Option<String>,
}

impl PresetBuilder {
    /// 创建空的构造器
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置模型型号
    pub fn with_model(mut self, model: ModelVariant) -> Self {
        self.model = Some(model);
        self
    }

    /// 设置场景配置
    pub fn with_scenario(mut self, scenario: ScenarioProfile) -> Self {
        self.scenario = Some(scenario);
        self
    }

    /// 设置窗口配置（显式设置后不触发 Agent → Window 联动）
    pub fn with_window(mut self, window: WindowProfile) -> Self {
        self.window = Some(window);
        self
    }

    /// 设置 Agent 配置
    pub fn with_agent(mut self, agent: AgentProfile) -> Self {
        self.agent = Some(agent);
        self
    }

    /// 添加技能（可链式调用多次）
    pub fn with_skill(mut self, skill: SkillProfile) -> Self {
        self.skills.push(skill);
        self
    }

    /// 批量设置技能（覆盖原有列表）
    pub fn with_skills(mut self, skills: Vec<SkillProfile>) -> Self {
        self.skills = skills;
        self
    }

    /// 用户覆盖：归档阈值（最高优先级）
    pub fn with_user_archive_threshold(mut self, threshold: usize) -> Self {
        self.user_archive_threshold = Some(threshold);
        self
    }

    /// 用户覆盖：摘要模板（最高优先级）
    ///
    /// 模板需包含 `{conversation}` 占位符
    pub fn with_user_summary_template(mut self, template: impl Into<String>) -> Self {
        self.user_summary_template = Some(template.into());
        self
    }

    /// 构建最终配置
    ///
    /// ## 执行顺序
    ///
    /// 1. 校验所有已设置的 Profile
    /// 2. 联动推导：若 Agent 已设置但 Window 未设置，从 Agent 推导 Window
    /// 3. 解析归档阈值（优先级：用户 > scenario > model > 默认）
    /// 4. 解析摘要模板（优先级：用户 > scenario.custom > scenario 预设 > 默认）
    /// 5. 解析 session_prefix（来自 Agent）
    /// 6. 解析 archive_to_hippocampus（Agent 和 Window 任一禁用则不归档）
    pub fn build(self) -> Result<CombinedProfile, PresetError> {
        // 1. 校验所有 Profile
        if let Some(s) = &self.scenario {
            s.validate()
                .map_err(|e| PresetError::ValidateFailed("scenario", e))?;
        }
        if let Some(w) = &self.window {
            w.validate()
                .map_err(|e| PresetError::ValidateFailed("window", e))?;
        }
        if let Some(a) = &self.agent {
            a.validate()
                .map_err(|e| PresetError::ValidateFailed("agent", e))?;
        }
        for s in &self.skills {
            s.validate()
                .map_err(|e| PresetError::ValidateFailed("skill", e))?;
        }

        // 2. 联动推导：Agent → Window
        let resolved_window = if should_derive_window(self.agent.as_ref(), self.window.as_ref()) {
            let agent = self.agent.as_ref().expect("agent 已确认存在");
            let derived = derive_window_from_agent(agent);
            tracing::debug!(
                agent = %agent.family,
                derived_scheme = ?derived_window_scheme(&derived),
                "联动推导 Window"
            );
            Some(derived)
        } else {
            self.window.clone()
        };

        // 3. 解析归档阈值（用户 > scenario > model > 默认）
        let archive_threshold = self
            .user_archive_threshold
            .or_else(|| self.scenario.as_ref().map(|s| s.archive_threshold))
            .or_else(|| self.model.as_ref().map(|m| m.archive_strategy.threshold()))
            .unwrap_or(DEFAULT_ARCHIVE_THRESHOLD);

        // 4. 解析摘要模板（用户 > scenario > 默认）
        let summary_template = self
            .user_summary_template
            .clone()
            .or_else(|| {
                self.scenario
                    .as_ref()
                    .map(|s| s.summary_template())
            })
            .unwrap_or_else(|| DEFAULT_SUMMARY_TEMPLATE.to_string());

        // 5. 解析 session_prefix（来自 Agent）
        let session_prefix = self
            .agent
            .as_ref()
            .map(|a| a.session_prefix.clone());

        // 6. 解析 archive_to_hippocampus（Agent 和 Window 任一禁用则不归档）
        let archive_to_hippocampus = {
            let agent_flag = self
                .agent
                .as_ref()
                .map(|a| a.archive_to_hippocampus)
                .unwrap_or(true);
            let window_flag = resolved_window
                .as_ref()
                .map(|w| w.archive_to_hippocampus)
                .unwrap_or(true);
            agent_flag && window_flag
        };

        // 7. 生成 usage_protocol（v2.30 新增）
        let usage_protocol = generate_usage_protocol(
            self.agent.as_ref(),
            self.scenario.as_ref().map(|s| &s.scenario),
            resolved_window.as_ref(),
            archive_threshold,
        );

        Ok(CombinedProfile::new(
            self.model,
            self.scenario,
            resolved_window,
            self.agent,
            self.skills,
            archive_threshold,
            summary_template,
            session_prefix,
            archive_to_hippocampus,
            usage_protocol,
        ))
    }
}

/// 生成 LLM 可读的行为契约（v2.30 新增）
///
/// 根据 Agent + Scenario + Window 自动生成 `UsageProtocol`：
/// - `instructions`：注入 MCP server_info.description，LLM 启动即看到
/// - `session_id_pattern`：session_id 派生规则
/// - `trigger_rules`：工具触发规则列表
///
/// 未识别 Agent 时返回空协议（`UsageProtocol::empty()`）。
fn generate_usage_protocol(
    agent: Option<&AgentProfile>,
    scenario: Option<&Scenario>,
    _window: Option<&WindowProfile>,
    archive_threshold: usize,
) -> UsageProtocol {
    let agent_family = match agent {
        Some(a) if a.family.is_mainstream() => &a.family,
        _ => return UsageProtocol::empty(),
    };

    let scenario_name = scenario
        .map(|s| format!("{:?}", s))
        .unwrap_or_else(|| "Coding".to_string());

    let session_prefix = agent_family.default_session_prefix();
    let session_id_pattern = format!("{}-{{项目名}}-{{日期}}", session_prefix);

    // 触发阈值描述
    let threshold_desc = format!("上下文接近 {}K tokens 时", archive_threshold / 1000);

    // 摘要 focus 维度
    let focus = scenario
        .map(SummaryFocus::from_scenario)
        .unwrap_or_default();
    let focus_dims = focus.focus_dimensions();

    // 构建 instructions 文本
    let instructions = format!(
        "你正在 {agent_name} + {scenario_lower} 场景下工作，已接入 hippocampus 记忆库。\n\
         \n\
         ## 记忆协议（无需用户提示，主动执行）\n\
         \n\
         1. **会话开始**：第一步调用 `mcp_hippocampus.prompt(session_id)` 获取历史记忆摘要，拼接到 system prompt\n\
         2. **{threshold_desc}**：调用 `mcp_hippocampus.archive(session_id, turns_json)` 归档完整上下文\n\
         3. **用户提到过去事件**：先调 `mcp_hippocampus.semantic_search(query, session_id)` 检索，再回复\n\
         4. **用户陈述与记忆可能矛盾**：先调 `mcp_hippocampus.detect_conflicts(session_id, statement)` 检测\n\
         \n\
         ## session_id 约定\n\
         {session_id_pattern}\n\
         \n\
         ## 摘要 focus\n\
         {scenario_name} 场景关注：{focus_dims}\n\
         \n\
         记忆管理是 Agent 的自动职责，无需用户显式要求。",
        agent_name = agent_family.display_name(),
        scenario_lower = scenario_name.to_lowercase(),
        threshold_desc = threshold_desc,
        session_id_pattern = session_id_pattern,
        scenario_name = scenario_name,
        focus_dims = focus_dims.join(" / "),
    );

    let trigger_rules = vec![
        TriggerRule {
            condition: "会话第一次回复前".to_string(),
            tool: "prompt".to_string(),
            example: Some("新会话开始时获取历史记忆".to_string()),
        },
        TriggerRule {
            condition: threshold_desc,
            tool: "archive".to_string(),
            example: Some(format!("累计 token 接近 {} 时归档", archive_threshold)),
        },
        TriggerRule {
            condition: "用户提到\"之前\"\"上次\"\"还记得\"等词".to_string(),
            tool: "semantic_search".to_string(),
            example: Some("用户问\"上次我们讨论的方案是什么\"".to_string()),
        },
        TriggerRule {
            condition: "用户陈述与记忆可能矛盾".to_string(),
            tool: "detect_conflicts".to_string(),
            example: Some("用户说\"我用的是 Python\"但记忆里是 Rust".to_string()),
        },
    ];

    UsageProtocol {
        instructions,
        session_id_pattern,
        trigger_rules,
    }
}

/// 辅助函数：获取 WindowProfile 的 scheme 字符串（用于日志）
fn derived_window_scheme(w: &WindowProfile) -> String {
    format!("{:?}", w.scheme)
}

// ============================================================================
// 字符串参数构建（v2.29 公共函数，供 server / mcp / python 复用）
// ============================================================================

/// 字符串解析为 Scenario（大小写不敏感）
///
/// 支持的别名：coding / writing / research / daily / finance / design / officework|office|work
/// 未匹配则返回 `Scenario::Custom(s)`。
pub fn scenario_from_str(s: &str) -> hippocampus_scenarios::Scenario {
    let lower = s.to_lowercase();
    match lower.as_str() {
        "coding" => hippocampus_scenarios::Scenario::Coding,
        "writing" => hippocampus_scenarios::Scenario::Writing,
        "research" => hippocampus_scenarios::Scenario::Research,
        "daily" => hippocampus_scenarios::Scenario::Daily,
        "finance" => hippocampus_scenarios::Scenario::Finance,
        "design" => hippocampus_scenarios::Scenario::Design,
        "officework" | "office" | "work" => hippocampus_scenarios::Scenario::OfficeWork,
        _ => hippocampus_scenarios::Scenario::Custom(s.to_string()),
    }
}

/// 从字符串参数构建 CombinedProfile（v2.29）
///
/// 公共函数，供 `hippocampus-server` 的 archive handler / build_preset 端点、
/// `hippocampus-mcp` 的 archive tool / preset_build tool 共用。
///
/// ## 参数
///
/// 所有参数可选，未提供的字段使用默认值或联动推导：
/// - `agent`：Agent display_name（如 "Claude Code"），未匹配则视为 Custom Agent
/// - `scenario`：Scenario 名称（大小写不敏感，如 "coding" / "Coding"）
/// - `model`：ModelVariant 名称（如 "claude-opus-4.8"），未找到则返回错误
/// - `archive_threshold`：用户覆盖归档阈值（最高优先级）
/// - `summary_template`：用户覆盖摘要模板（最高优先级，需含 `{conversation}`）
///
/// ## 错误
///
/// - `model` 未找到：`"未找到型号: {name}"`
/// - `summary_template` 缺少 `{conversation}`：`"summary_template 必须包含 {conversation} 占位符"`
/// - Profile 校验失败：`"预设构建失败: {err}"`
pub fn build_from_strings(
    agent: Option<&str>,
    scenario: Option<&str>,
    model: Option<&str>,
    archive_threshold: Option<usize>,
    summary_template: Option<&str>,
) -> Result<CombinedProfile, String> {
    let mut builder = PresetBuilder::new();

    // 1. Agent
    if let Some(agent_str) = agent {
        let family = hippocampus_agents::AgentFamily::from_str(agent_str)
            .unwrap_or_else(|| hippocampus_agents::AgentFamily::Custom(agent_str.to_string()));
        let profile = hippocampus_agents::AgentProfile::from_family(family);
        builder = builder.with_agent(profile);
    }

    // 2. Scenario
    if let Some(scenario_str) = scenario {
        let sc = scenario_from_str(scenario_str);
        let profile = hippocampus_scenarios::ScenarioProfile::from_scenario(sc);
        builder = builder.with_scenario(profile);
    }

    // 3. Model
    if let Some(model_str) = model {
        match hippocampus_models::ModelRegistry::find(model_str) {
            Some(variant) => {
                builder = builder.with_model(variant);
            }
            None => {
                return Err(format!(
                    "未找到型号: {}（GET /api/v1/presets/models 查询支持的型号）",
                    model_str
                ));
            }
        }
    }

    // 4. 用户覆盖
    if let Some(threshold) = archive_threshold {
        builder = builder.with_user_archive_threshold(threshold);
    }
    if let Some(template) = summary_template {
        if !template.contains("{conversation}") {
            return Err("summary_template 必须包含 {conversation} 占位符".to_string());
        }
        builder = builder.with_user_summary_template(template);
    }

    builder.build().map_err(|e| format!("预设构建失败: {}", e))
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use hippocampus_agents::{AgentFamily, AgentProfile};
    use hippocampus_scenarios::{Scenario, ScenarioProfile};
    use hippocampus_skills::{BuiltinSkill, SkillProfile};
    #[test]
    fn test_empty_builder_uses_defaults() {
        let combined = PresetBuilder::new().build().unwrap();
        assert_eq!(combined.archive_threshold(), DEFAULT_ARCHIVE_THRESHOLD);
        assert_eq!(combined.summary_template(), DEFAULT_SUMMARY_TEMPLATE);
        assert!(combined.session_prefix().is_none());
        assert!(combined.archive_to_hippocampus());
    }

    #[test]
    fn test_agent_only_triggers_window_linkage() {
        let combined = PresetBuilder::new()
            .with_agent(AgentProfile::claude_code())
            .build()
            .unwrap();

        // 联动推导：ClaudeCode → claude_code() window（180K）
        assert!(combined.window.is_some());
        assert_eq!(combined.window.as_ref().unwrap().trigger_threshold, 180_000);
        // session_prefix 来自 Agent
        assert_eq!(combined.session_prefix(), Some("claude-code"));
    }

    #[test]
    fn test_explicit_window_overrides_linkage() {
        let custom_window = WindowProfile::default(); // GenericSliding, 100K
        let combined = PresetBuilder::new()
            .with_agent(AgentProfile::claude_code())
            .with_window(custom_window)
            .build()
            .unwrap();

        // 显式设置的 Window 优先，不触发联动（100K 而非 ClaudeCode 的 180K）
        assert_eq!(combined.window.as_ref().unwrap().trigger_threshold, 100_000);
    }

    #[test]
    fn test_scenario_archive_threshold() {
        let combined = PresetBuilder::new()
            .with_scenario(ScenarioProfile::from_scenario(Scenario::Coding))
            .build()
            .unwrap();

        // Coding 场景默认 500K
        assert_eq!(combined.archive_threshold(), 500_000);
    }

    #[test]
    fn test_user_archive_threshold_overrides_scenario() {
        let combined = PresetBuilder::new()
            .with_scenario(ScenarioProfile::from_scenario(Scenario::Coding))
            .with_user_archive_threshold(450_000)
            .build()
            .unwrap();

        // 用户覆盖优先
        assert_eq!(combined.archive_threshold(), 450_000);
    }

    #[test]
    fn test_model_archive_threshold() {
        use hippocampus_models::variant::ArchiveStrategy;

        let mut model = ModelVariant::claude_opus_4_6();
        model.archive_strategy = ArchiveStrategy::LargeWindow { threshold: 600_000 };

        let combined = PresetBuilder::new()
            .with_model(model)
            .build()
            .unwrap();

        // 来自 model.archive_strategy.threshold()
        assert_eq!(combined.archive_threshold(), 600_000);
    }

    #[test]
    fn test_priority_user_over_scenario_over_model() {
        use hippocampus_models::variant::ArchiveStrategy;

        let mut model = ModelVariant::claude_opus_4_6();
        model.archive_strategy = ArchiveStrategy::LargeWindow { threshold: 600_000 };

        let combined = PresetBuilder::new()
            .with_model(model)
            .with_scenario(ScenarioProfile::from_scenario(Scenario::Coding)) // 500K
            .with_user_archive_threshold(300_000) // 用户最高优先
            .build()
            .unwrap();

        assert_eq!(combined.archive_threshold(), 300_000);
    }

    #[test]
    fn test_scenario_over_model() {
        use hippocampus_models::variant::ArchiveStrategy;

        let mut model = ModelVariant::claude_opus_4_6();
        model.archive_strategy = ArchiveStrategy::LargeWindow { threshold: 600_000 };

        let combined = PresetBuilder::new()
            .with_model(model) // 600K
            .with_scenario(ScenarioProfile::from_scenario(Scenario::Coding)) // 500K
            .build()
            .unwrap();

        // scenario 优先于 model
        assert_eq!(combined.archive_threshold(), 500_000);
    }

    #[test]
    fn test_summary_template_user_override() {
        let combined = PresetBuilder::new()
            .with_scenario(ScenarioProfile::from_scenario(Scenario::Coding))
            .with_user_summary_template("custom {conversation}")
            .build()
            .unwrap();

        assert_eq!(combined.summary_template(), "custom {conversation}");
    }

    #[test]
    fn test_summary_template_from_scenario() {
        let combined = PresetBuilder::new()
            .with_scenario(ScenarioProfile::from_scenario(Scenario::Coding))
            .build()
            .unwrap();

        // 来自 scenario 的 SummaryFocus 预设
        let template = combined.summary_template();
        assert!(template.contains("{conversation}"));
        assert!(template.contains("title"));
    }

    #[test]
    fn test_summary_template_default_when_no_scenario() {
        let combined = PresetBuilder::new().build().unwrap();
        assert_eq!(combined.summary_template(), DEFAULT_SUMMARY_TEMPLATE);
    }

    #[test]
    fn test_archive_disabled_by_agent() {
        let combined = PresetBuilder::new()
            .with_agent(AgentProfile::claude_code().with_archive_disabled())
            .build()
            .unwrap();

        assert!(!combined.archive_to_hippocampus());
    }

    #[test]
    fn test_archive_disabled_by_window() {
        let window = WindowProfile::default().with_archive_to_hippocampus(false);
        let combined = PresetBuilder::new()
            .with_window(window)
            .build()
            .unwrap();

        assert!(!combined.archive_to_hippocampus());
    }

    #[test]
    fn test_archive_enabled_when_both_default() {
        let combined = PresetBuilder::new()
            .with_agent(AgentProfile::cursor())
            .build()
            .unwrap();

        assert!(combined.archive_to_hippocampus());
    }

    #[test]
    fn test_skills_collected() {
        let combined = PresetBuilder::new()
            .with_skill(SkillProfile::new(BuiltinSkill::Read))
            .with_skill(SkillProfile::new(BuiltinSkill::Bash).with_disabled())
            .build()
            .unwrap();

        assert_eq!(combined.skills.len(), 2);
        assert!(combined.is_skill_enabled("读取文件"));
        assert!(!combined.is_skill_enabled("执行命令"));
    }

    #[test]
    fn test_all_builtin_agents_build_success() {
        for family in AgentFamily::all_builtin() {
            let agent = AgentProfile::from_family(family);
            let combined = PresetBuilder::new()
                .with_agent(agent)
                .build()
                .unwrap();
            assert!(combined.window.is_some(), "Agent 未触发 Window 联动");
        }
    }

    #[test]
    fn test_full_preset() {
        let combined = PresetBuilder::new()
            .with_model(ModelVariant::claude_opus_4_6())
            .with_scenario(ScenarioProfile::from_scenario(Scenario::Coding))
            .with_agent(AgentProfile::claude_code())
            .with_skill(SkillProfile::new(BuiltinSkill::Read))
            .with_user_archive_threshold(450_000)
            .build()
            .unwrap();

        assert_eq!(combined.archive_threshold(), 450_000);
        assert!(combined.window.is_some());
        assert!(combined.agent.is_some());
        assert!(combined.model.is_some());
        assert!(combined.scenario.is_some());
        assert_eq!(combined.skills.len(), 1);
        assert_eq!(combined.session_prefix(), Some("claude-code"));
    }

    #[test]
    fn test_invalid_scenario_returns_error() {
        use hippocampus_scenarios::{RetrievalStrategy, ScoreWeights};

        let mut bad_scenario = ScenarioProfile::from_scenario(Scenario::Coding);
        // 构造非法权重（和不为 1.0）
        bad_scenario.score_weights = ScoreWeights {
            recency: 0.1,
            access_frequency: 0.1,
            topic_relevance: 0.1,
            user_marked: 0.1,
        };
        bad_scenario.retrieval_strategy = RetrievalStrategy::Hybrid {
            bm25_weight: 0.0,
            semantic_weight: 0.0,
        };

        let result = PresetBuilder::new()
            .with_scenario(bad_scenario)
            .build();
        assert!(result.is_err());
    }

    #[test]
    fn test_builder_chain() {
        let combined = PresetBuilder::new()
            .with_agent(AgentProfile::cursor().with_variant("1.45"))
            .with_scenario(ScenarioProfile::from_scenario(Scenario::Writing))
            .with_user_summary_template("writing template {conversation}")
            .build()
            .unwrap();

        assert_eq!(combined.session_prefix(), Some("cursor"));
        assert_eq!(combined.summary_template(), "writing template {conversation}");
    }

    // =========================================================================
    // v2.30 新增：usage_protocol 行为契约测试
    // =========================================================================

    #[test]
    fn test_usage_protocol_empty_when_no_agent() {
        // 未设置 agent 时，usage_protocol 为空
        let combined = PresetBuilder::new().build().unwrap();
        assert!(combined.usage_protocol().is_empty());
    }

    #[test]
    fn test_usage_protocol_empty_when_non_mainstream_agent() {
        // 7 待补 family 不是主流，usage_protocol 为空
        let combined = PresetBuilder::new()
            .with_agent(AgentProfile::from_family(AgentFamily::Zcode))
            .build()
            .unwrap();
        assert!(combined.usage_protocol().is_empty());
    }

    #[test]
    fn test_usage_protocol_generated_for_claude_code() {
        let combined = PresetBuilder::new()
            .with_agent(AgentProfile::claude_code())
            .with_scenario(ScenarioProfile::from_scenario(Scenario::Coding))
            .build()
            .unwrap();

        let protocol = combined.usage_protocol();
        assert!(!protocol.is_empty());
        assert!(protocol.instructions.contains("Claude Code"));
        assert!(protocol.instructions.contains("coding"));
        assert!(protocol.instructions.contains("mcp_hippocampus.prompt"));
        assert!(protocol.instructions.contains("mcp_hippocampus.archive"));
        assert!(protocol.session_id_pattern.contains("claude-code"));
        assert_eq!(protocol.trigger_rules.len(), 4);

        // 验证触发规则
        let tools: Vec<&str> = protocol.trigger_rules.iter().map(|r| r.tool.as_str()).collect();
        assert!(tools.contains(&"prompt"));
        assert!(tools.contains(&"archive"));
        assert!(tools.contains(&"semantic_search"));
        assert!(tools.contains(&"detect_conflicts"));
    }

    #[test]
    fn test_usage_protocol_generated_for_trae() {
        let combined = PresetBuilder::new()
            .with_agent(AgentProfile::trae())
            .build()
            .unwrap();

        let protocol = combined.usage_protocol();
        assert!(!protocol.is_empty());
        assert!(protocol.instructions.contains("Trae"));
        assert!(protocol.session_id_pattern.starts_with("trae-"));
    }

    #[test]
    fn test_usage_protocol_threshold_in_instructions() {
        // Coding 场景默认 500K，instructions 应包含 500K
        let combined = PresetBuilder::new()
            .with_agent(AgentProfile::claude_code())
            .with_scenario(ScenarioProfile::from_scenario(Scenario::Coding))
            .build()
            .unwrap();

        let protocol = combined.usage_protocol();
        // Coding 默认 500K，但 ClaudeCode 联动推导的 Window 是 180K，
        // archive_threshold 优先级：scenario(500K) > window，所以是 500K
        assert!(protocol.instructions.contains("500K") || protocol.instructions.contains("500k"));
    }

    #[test]
    fn test_usage_protocol_user_threshold_override() {
        // 用户覆盖阈值，instructions 应反映新阈值
        let combined = PresetBuilder::new()
            .with_agent(AgentProfile::cursor())
            .with_user_archive_threshold(300_000)
            .build()
            .unwrap();

        let protocol = combined.usage_protocol();
        assert!(protocol.instructions.contains("300K") || protocol.instructions.contains("300k"));
    }

    #[test]
    fn test_usage_protocol_focus_dims_in_instructions() {
        // Writing 场景的 focus 维度应出现在 instructions
        let combined = PresetBuilder::new()
            .with_agent(AgentProfile::claude_code())
            .with_scenario(ScenarioProfile::from_scenario(Scenario::Writing))
            .build()
            .unwrap();

        let protocol = combined.usage_protocol();
        assert!(protocol.instructions.contains("核心观点") || protocol.instructions.contains("论据"));
    }

    #[test]
    fn test_usage_protocol_serialize_deserialize() {
        let combined = PresetBuilder::new()
            .with_agent(AgentProfile::claude_code())
            .with_scenario(ScenarioProfile::from_scenario(Scenario::Coding))
            .build()
            .unwrap();

        let json = serde_json::to_string(&combined).unwrap();
        let back: CombinedProfile = serde_json::from_str(&json).unwrap();

        assert!(!back.usage_protocol().is_empty());
        assert_eq!(
            back.usage_protocol().session_id_pattern,
            combined.usage_protocol().session_id_pattern
        );
        assert_eq!(
            back.usage_protocol().trigger_rules.len(),
            combined.usage_protocol().trigger_rules.len()
        );
    }
}
