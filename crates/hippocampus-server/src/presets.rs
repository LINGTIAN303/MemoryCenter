//! # 预设端点（v2.29）
//!
//! 即时构建预设配置 + 查询支持的 Agent / Scenario / Model。
//!
//! ## 端点
//!
//! | 方法 | 路径 | 作用 |
//! |------|------|------|
//! | GET  | `/api/v1/presets/agents` | 列出 11 个内置 Agent |
//! | GET  | `/api/v1/presets/scenarios` | 列出 7 个内置 Scenario |
//! | GET  | `/api/v1/presets/models` | 列出所有 ModelVariant |
//! | POST | `/api/v1/presets/build` | 即时构建预设，返回 CombinedProfile |
//!
//! ## 设计
//!
//! - **无状态**：每次请求独立构建，不持久化预设
//! - **即时计算**：build 端点接收字符串参数，服务端组装 PresetBuilder 后返回最终配置
//! - **向后兼容**：archive 端点新增可选 preset 字段，未传时保持原行为

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::AppState;
use hippocampus_agents::AgentFamily;
use hippocampus_models::ModelRegistry;
use hippocampus_presets::PresetBuilder;
use hippocampus_scenarios::Scenario;

// ============================================================================
// 响应结构
// ============================================================================

/// Agent 信息（GET /presets/agents 返回）
#[derive(Serialize)]
pub struct AgentInfo {
    /// display_name（如 "Claude Code"）
    pub name: String,
    /// 默认 session 前缀（如 "claude-code"）
    pub session_prefix: String,
    /// 是否为 4 主流（有完整预设）
    pub is_mainstream: bool,
}

/// Scenario 信息（GET /presets/scenarios 返回）
#[derive(Serialize)]
pub struct ScenarioInfo {
    /// 枚举变体名（如 "Coding"）
    pub variant: String,
    /// 中文显示名（如 "编码场景"）
    pub display_name: String,
    /// 默认归档阈值
    pub archive_threshold: usize,
}

/// Model 信息（GET /presets/models 返回）
#[derive(Serialize)]
pub struct ModelInfo {
    /// 型号名称（如 "claude-opus-4.8"）
    pub name: String,
    /// 家族显示名（如 "Anthropic Claude"）
    pub family: String,
    /// 上下文窗口大小
    pub context_window: usize,
    /// 是否为家族默认型号
    pub is_default: bool,
}

/// 预设构建请求（POST /presets/build）
///
/// 所有字段可选，未提供的字段使用默认值或联动推导。
#[derive(Deserialize, Default)]
pub struct BuildPresetRequest {
    /// Agent display_name（如 "Claude Code"），大小写敏感
    pub agent: Option<String>,
    /// Scenario 名称（大小写不敏感，如 "coding" / "Coding"）
    pub scenario: Option<String>,
    /// ModelVariant 名称（如 "claude-opus-4.8"）
    pub model: Option<String>,
    /// 用户覆盖：归档阈值（最高优先级）
    pub archive_threshold: Option<usize>,
    /// 用户覆盖：摘要模板（最高优先级，需含 {conversation}）
    pub summary_template: Option<String>,
}

// ============================================================================
// 辅助函数
// ============================================================================

/// 字符串解析为 Scenario（大小写不敏感）
fn scenario_from_str(s: &str) -> Scenario {
    let lower = s.to_lowercase();
    match lower.as_str() {
        "coding" => Scenario::Coding,
        "writing" => Scenario::Writing,
        "research" => Scenario::Research,
        "daily" => Scenario::Daily,
        "finance" => Scenario::Finance,
        "design" => Scenario::Design,
        "officework" | "office" | "work" => Scenario::OfficeWork,
        _ => Scenario::Custom(s.to_string()),
    }
}

/// 从 PresetRequest 构建 CombinedProfile（v2.29）
///
/// 公共函数，供 `archive` handler 和 `build_preset` 端点共用。
/// 返回 `Result<CombinedProfile, String>`，错误消息直接返回给调用方。
pub fn build_combined_from_request(
    req: &crate::handlers::PresetRequest,
) -> Result<hippocampus_presets::CombinedProfile, String> {
    let mut builder = PresetBuilder::new();

    // 1. Agent
    if let Some(agent_str) = &req.agent {
        let family = AgentFamily::from_str(agent_str).unwrap_or_else(|| {
            AgentFamily::Custom(agent_str.clone())
        });
        let profile = hippocampus_agents::AgentProfile::from_family(family);
        builder = builder.with_agent(profile);
    }

    // 2. Scenario
    if let Some(scenario_str) = &req.scenario {
        let sc = scenario_from_str(scenario_str);
        let profile = hippocampus_scenarios::ScenarioProfile::from_scenario(sc);
        builder = builder.with_scenario(profile);
    }

    // 3. Model
    if let Some(model_str) = &req.model {
        match ModelRegistry::find(model_str) {
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
    if let Some(threshold) = req.archive_threshold {
        builder = builder.with_user_archive_threshold(threshold);
    }
    if let Some(template) = &req.summary_template {
        if !template.contains("{conversation}") {
            return Err("summary_template 必须包含 {conversation} 占位符".to_string());
        }
        builder = builder.with_user_summary_template(template);
    }

    builder.build().map_err(|e| format!("预设构建失败: {}", e))
}

// ============================================================================
// 4 个端点 handler
// ============================================================================

/// GET /api/v1/presets/agents
///
/// 列出所有内置 Agent（11 个）。
pub async fn list_agents(State(_state): State<AppState>) -> Json<Vec<AgentInfo>> {
    let agents: Vec<AgentInfo> = AgentFamily::all_builtin()
        .into_iter()
        .map(|family| AgentInfo {
            name: family.display_name().to_string(),
            session_prefix: family.default_session_prefix().to_string(),
            is_mainstream: family.is_mainstream(),
        })
        .collect();

    Json(agents)
}

/// GET /api/v1/presets/scenarios
///
/// 列出所有内置 Scenario（7 个）。
pub async fn list_scenarios(State(_state): State<AppState>) -> Json<Vec<ScenarioInfo>> {
    let scenarios: Vec<ScenarioInfo> = Scenario::all_builtin()
        .iter()
        .map(|s| {
            let profile = hippocampus_scenarios::ScenarioProfile::from_scenario(s.clone());
            ScenarioInfo {
                variant: format!("{:?}", s),
                display_name: s.display_name(),
                archive_threshold: profile.archive_threshold,
            }
        })
        .collect();

    Json(scenarios)
}

/// GET /api/v1/presets/models
///
/// 列出所有 ModelVariant。
pub async fn list_models(State(_state): State<AppState>) -> Json<Vec<ModelInfo>> {
    let models: Vec<ModelInfo> = ModelRegistry::all_variants()
        .map(|(name, variant)| {
            // 判断是否为家族默认型号
            let default = ModelRegistry::default_variant(variant.family);
            ModelInfo {
                name: name.clone(),
                family: variant.family.display_name().to_string(),
                context_window: variant.context_window,
                is_default: default.name == *name,
            }
        })
        .collect();

    Json(models)
}

/// POST /api/v1/presets/build
///
/// 即时构建预设，返回 CombinedProfile。
///
/// 所有字段可选，未提供的字段使用默认值或联动推导。
pub async fn build_preset(
    State(_state): State<AppState>,
    Json(req): Json<BuildPresetRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    // 转换为 PresetRequest 复用公共构建逻辑
    let preset_req = crate::handlers::PresetRequest {
        agent: req.agent,
        scenario: req.scenario,
        model: req.model,
        archive_threshold: req.archive_threshold,
        summary_template: req.summary_template,
    };

    let combined = build_combined_from_request(&preset_req)
        .map_err(AppError::BadRequest)?;

    let response = serde_json::json!({
        // 解析后的最终生效值
        "archive_threshold": combined.archive_threshold(),
        "summary_template": combined.summary_template(),
        "session_prefix": combined.session_prefix(),
        "archive_to_hippocampus": combined.archive_to_hippocampus(),
        // 标志位
        "has_agent": combined.agent.is_some(),
        "has_scenario": combined.scenario.is_some(),
        "has_window": combined.window.is_some(),
        "has_model": combined.model.is_some(),
        "skills_count": combined.skills.len(),
    });

    Ok(Json(response))
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppState;
    use std::path::PathBuf;

    fn test_state() -> AppState {
        AppState {
            storage_root: PathBuf::from("./test-data"),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn test_list_agents_returns_11_builtin() {
        let state = test_state();
        let Json(agents) = list_agents(State(state)).await;
        assert_eq!(agents.len(), 11);
        // Claude Code 应在列表中
        assert!(agents.iter().any(|a| a.name == "Claude Code"));
        assert!(agents.iter().any(|a| a.is_mainstream));
    }

    #[tokio::test]
    async fn test_list_scenarios_returns_7_builtin() {
        let state = test_state();
        let Json(scenarios) = list_scenarios(State(state)).await;
        assert_eq!(scenarios.len(), 7);
        // Coding 场景应在列表中
        let coding = scenarios.iter().find(|s| s.variant == "Coding");
        assert!(coding.is_some());
        assert_eq!(coding.unwrap().archive_threshold, 500_000);
    }

    #[tokio::test]
    async fn test_list_models_returns_all() {
        let state = test_state();
        let Json(models) = list_models(State(state)).await;
        // 总型号数 15 个（v2.24 型号库更新后）
        assert!(models.len() >= 15);
        // 至少有一个是家族默认
        assert!(models.iter().any(|m| m.is_default));
    }

    #[tokio::test]
    async fn test_build_preset_empty_uses_defaults() {
        let state = test_state();
        let req = BuildPresetRequest::default();
        let Json(result) = build_preset(State(state), Json(req)).await.unwrap();
        // 默认归档阈值
        assert_eq!(result["archive_threshold"], 400_000);
        assert_eq!(result["has_agent"], false);
    }

    #[tokio::test]
    async fn test_build_preset_with_agent_triggers_window_linkage() {
        let state = test_state();
        let req = BuildPresetRequest {
            agent: Some("Claude Code".into()),
            ..Default::default()
        };
        let Json(result) = build_preset(State(state), Json(req)).await.unwrap();
        // 联动推导 Window
        assert_eq!(result["has_agent"], true);
        assert_eq!(result["has_window"], true);
        assert_eq!(result["session_prefix"], "claude-code");
    }

    #[tokio::test]
    async fn test_build_preset_with_scenario_overrides_threshold() {
        let state = test_state();
        let req = BuildPresetRequest {
            scenario: Some("coding".into()),
            ..Default::default()
        };
        let Json(result) = build_preset(State(state), Json(req)).await.unwrap();
        // Coding 场景默认 500K
        assert_eq!(result["archive_threshold"], 500_000);
        assert_eq!(result["has_scenario"], true);
    }

    #[tokio::test]
    async fn test_build_preset_user_threshold_overrides_scenario() {
        let state = test_state();
        let req = BuildPresetRequest {
            scenario: Some("coding".into()),
            archive_threshold: Some(450_000),
            ..Default::default()
        };
        let Json(result) = build_preset(State(state), Json(req)).await.unwrap();
        // 用户覆盖优先
        assert_eq!(result["archive_threshold"], 450_000);
    }

    #[tokio::test]
    async fn test_build_preset_invalid_model_returns_error() {
        let state = test_state();
        let req = BuildPresetRequest {
            model: Some("nonexistent-model".into()),
            ..Default::default()
        };
        let result = build_preset(State(state), Json(req)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_build_preset_invalid_template_returns_error() {
        let state = test_state();
        let req = BuildPresetRequest {
            summary_template: Some("missing placeholder".into()),
            ..Default::default()
        };
        let result = build_preset(State(state), Json(req)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_build_preset_full_combination() {
        let state = test_state();
        let req = BuildPresetRequest {
            agent: Some("Claude Code".into()),
            scenario: Some("coding".into()),
            model: Some("claude-opus-4.8".into()),
            archive_threshold: Some(300_000),
            summary_template: Some("custom {conversation}".into()),
        };
        let Json(result) = build_preset(State(state), Json(req)).await.unwrap();
        assert_eq!(result["archive_threshold"], 300_000);
        assert_eq!(result["summary_template"], "custom {conversation}");
        assert_eq!(result["has_agent"], true);
        assert_eq!(result["has_scenario"], true);
        assert_eq!(result["has_model"], true);
    }
}
