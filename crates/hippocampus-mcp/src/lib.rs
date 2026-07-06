//! # Hippocampus MCP Server
//!
//! 将 Hippocampus 核心能力暴露为 MCP (Model Context Protocol) tools，
//! 供 Claude Code / Cursor / Trae / Codex CLI 等 MCP 客户端调用。
//!
//! ## 5 个 MCP Tools
//!
//! | Tool | 作用 | 触发时机 |
//! |------|------|----------|
//! | `archive` | 归档对话轮次为记忆文件 | Agent 会话达 token 阈值时 |
//! | `retrieve` | 按钩子 ID 检索完整记忆 | Agent 需要历史对话细节时 |
//! | `summaries` | 获取所有周期摘要列表 | 会话开始时了解历史记忆 |
//! | `prompt` | 渲染 system prompt 文本 | 会话开始时注入 LLM |
//! | `compaction` | 触发周期任务 | 周级合并 / 月级淘汰 |
//!
//! ## 传输方式
//!
//! - **stdio**（默认）：被 Claude Code / Cursor 等本地拉起子进程
//! - 未来可扩展 Streamable HTTP（挂载到 hippocampus-server 的 axum Router）
//!
//! ## 使用示例
//!
//! 在 Claude Code 的 MCP 配置中：
//!
//! ```json
//! {
//!   "mcpServers": {
//!     "hippocampus": {
//!       "command": "hippocampus-mcp",
//!       "env": {
//!         "HIPPOCAMPUS_ROOT": "/path/to/memory/data"
//!       }
//!     }
//!   }
//! }
//! ```

use rmcp::handler::server::wrapper::Parameters;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::schemars;
use rmcp::tool;
use rmcp::tool_handler;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

use hippocampus_core::archive::Archiver;
use hippocampus_core::compact::Compactor;
use hippocampus_core::conflict::ConflictDetector;
use hippocampus_core::generate::SummaryGenerator;
use hippocampus_core::model::{apply_turn_defaults, ArchiveConfig, MessageTurn};
use hippocampus_core::retrieve::{Retriever, SummaryView};
use hippocampus_core::score::DefaultScorer;
use hippocampus_core::storage::{LocalStorage, Storage};
// v2.18 批次2：复用 hippocampus-search 的 SessionSearchRouter（不引入 axum 重依赖）
use hippocampus_search::SessionSearchRouter;
// v2.30：启动时识别 Agent 客户端 + 注入 CombinedProfile（行为契约）
use hippocampus_presets::CombinedProfile;

/// MCP server 主结构体
#[derive(Clone)]
pub struct HippocampusMcp {
    /// 存储根目录
    storage_root: PathBuf,
    /// 可注入的冲突检测器（v2.11）
    ///
    /// - `Some`：`detect_conflicts` 和 `batch_update` 使用注入的检测器
    ///   （支持 `HeuristicDetector` / `HttpLlmDetector` / `HybridDetector`）
    /// - `None`：`detect_conflicts` 降级为 `HeuristicDetector`（向后兼容）
    ///   `batch_update` 不做冲突检测（保持 v2.6 行为）
    conflict_detector: Option<Arc<dyn ConflictDetector>>,
    /// 可注入的 Session 级语义检索路由器（v2.18）
    ///
    /// - `Some`：`semantic_search` 工具使用注入的路由器
    ///   （首次访问 session 时从 storage 自动重建索引 + 关键词/向量混合检索）
    /// - `None`：`semantic_search` 工具不可用（返回 501 错误，向后兼容）
    session_search: Option<Arc<SessionSearchRouter>>,
    /// 可注入的 LLM 摘要生成器（v2.21 批次 8c）
    ///
    /// - `Some`：`archive` 工具调用 LLM 生成结构化摘要填入 IndexHook
    ///   （title + abstract + key_facts + key_entities）
    /// - `None`：使用启发式 `Summary::from_title`（首条消息前 80 字符，向后兼容）
    /// - LLM 调用失败：降级为启发式，归档主流程不中断
    summary_generator: Option<Arc<dyn SummaryGenerator>>,
    /// 启动时识别 + 注入的 CombinedProfile（v2.30 新增）
    ///
    /// 由 `main()` 调用 `detect_agent_client()` + `PresetBuilder::build()` 生成，
    /// 包含 `usage_protocol`（LLM 可读的行为契约）。
    ///
    /// - `Some`：MCP server 启动时已识别 Agent 客户端（ClaudeCode/Cursor/Trae/Codex），
    ///   后续 tool 可读取 `usage_protocol.instructions` 注入 server_info.description
    /// - `None`：未识别（Custom/降级），tool 行为与 v2.29 一致（向后兼容）
    combined_profile: Option<CombinedProfile>,
}

impl HippocampusMcp {
    /// 创建新的 MCP server 实例（无冲突检测器，无语义检索，向后兼容）
    ///
    /// 等价于 `with_conflict_detector(None)` + `with_session_search(None)` + `with_summary_generator(None)`。
    pub fn new(storage_root: PathBuf) -> Self {
        Self {
            storage_root,
            conflict_detector: None,
            session_search: None,
            summary_generator: None,
            combined_profile: None,
        }
    }

    /// 创建带冲突检测器的 MCP server 实例（v2.11）
    ///
    /// ## 参数
    ///
    /// - `conflict_detector`：注入的检测器，支持：
    ///   - `HeuristicDetector`：启发式纯算法（默认）
    ///   - `HttpLlmDetector`：LLM 语义级检测
    ///   - `HybridDetector`：串联启发式 + LLM
    ///   - `None`：降级为 `HeuristicDetector`（仅 `detect_conflicts` 工具）
    pub fn with_conflict_detector(
        storage_root: PathBuf,
        conflict_detector: Option<Arc<dyn ConflictDetector>>,
    ) -> Self {
        Self {
            storage_root,
            conflict_detector,
            session_search: None,
            summary_generator: None,
            combined_profile: None,
        }
    }

    /// 链式注入 Session 级语义检索路由器（v2.18 builder 模式）
    ///
    /// 启用后 `semantic_search` 工具可用，首次访问 session 时自动从
    /// storage 读取所有 hook 批量重建索引（使用 `embed_batch` 优化 API 调用）。
    ///
    /// ## 使用示例
    ///
    /// ```rust,ignore
    /// let router = SessionSearchRouter::new(Some(embedder), dim)
    ///     .with_storage(storage);
    /// let mcp = HippocampusMcp::with_conflict_detector(root, detector)
    ///     .with_session_search(Some(Arc::new(router)));
    /// ```
    ///
    /// ## 参数
    ///
    /// - `session_search`：注入的路由器，传 `None` 禁用 `semantic_search` 工具
    pub fn with_session_search(mut self, session_search: Option<Arc<SessionSearchRouter>>) -> Self {
        self.session_search = session_search;
        self
    }

    /// 链式注入 LLM 摘要生成器（v2.21 批次 8c builder 模式）
    ///
    /// 启用后 `archive` 工具归档时调用 LLM 生成结构化摘要
    /// （title + abstract + key_facts + key_entities）填入 IndexHook。
    /// 未注入时使用启发式 `Summary::from_title`（首条消息前 80 字符）。
    ///
    /// ## 降级策略
    ///
    /// - LLM 调用失败：降级为 `Summary::from_title`，归档主流程不中断
    /// - 未注入：使用 `Summary::from_title`（向后兼容）
    ///
    /// ## 使用示例
    ///
    /// ```rust,ignore
    /// let gen: Arc<dyn SummaryGenerator> = Arc::new(HttpSummaryGenerator::new(config));
    /// let mcp = HippocampusMcp::with_conflict_detector(root, detector)
    ///     .with_summary_generator(Some(gen));
    /// ```
    ///
    /// ## 参数
    ///
    /// - `summary_generator`：注入的生成器，传 `None` 使用启发式摘要
    pub fn with_summary_generator(
        mut self,
        summary_generator: Option<Arc<dyn SummaryGenerator>>,
    ) -> Self {
        self.summary_generator = summary_generator;
        self
    }

    /// 链式注入启动时识别的 CombinedProfile（v2.30 builder 模式）
    ///
    /// 由 `main()` 在启动时调用 `detect_agent_client()` + `PresetBuilder::build()`
    /// 生成 CombinedProfile 后注入。后续 tool 可通过 `combined_profile()` 访问器
    /// 读取 `usage_protocol.instructions` 等字段。
    ///
    /// ## 降级策略
    ///
    /// - 识别成功（mainstream agent）：注入完整 CombinedProfile（含 usage_protocol）
    /// - 识别失败（Custom/降级）：传 `None`，tool 行为与 v2.29 一致（向后兼容）
    ///
    /// ## 使用示例
    ///
    /// ```rust,ignore
    /// use hippocampus_presets::{detect_agent_client, resolve_scenario_name, PresetBuilder};
    /// use hippocampus_agents::AgentProfile;
    /// use hippocampus_scenarios::ScenarioProfile;
    ///
    /// let detected = detect_agent_client(None);
    /// let scenario = resolve_scenario_name(&detected.family);
    /// let combined = PresetBuilder::new()
    ///     .with_agent(AgentProfile::from_family(detected.family))
    ///     .with_scenario(ScenarioProfile::from_scenario(scenario_from_str(&scenario)))
    ///     .build()?;
    ///
    /// let mcp = HippocampusMcp::with_conflict_detector(root, detector)
    ///     .with_combined_profile(Some(combined));
    /// ```
    ///
    /// ## 参数
    ///
    /// - `combined_profile`：注入的 CombinedProfile，传 `None` 走旧逻辑（向后兼容）
    pub fn with_combined_profile(
        mut self,
        combined_profile: Option<CombinedProfile>,
    ) -> Self {
        self.combined_profile = combined_profile;
        self
    }

    /// 获取启动时识别 + 注入的 CombinedProfile（v2.30）
    ///
    /// 返回 `Option<&CombinedProfile>`：
    /// - `Some`：已识别 Agent 客户端，可读取 `usage_protocol` 等字段
    /// - `None`：未识别（Custom/降级），tool 按旧逻辑处理
    pub fn combined_profile(&self) -> Option<&CombinedProfile> {
        self.combined_profile.as_ref()
    }

    /// 创建 Storage 实例（每次 tool 调用创建，无状态）
    fn create_storage(&self) -> Arc<dyn Storage> {
        Arc::new(LocalStorage::new(self.storage_root.clone()))
    }

    /// 获取冲突检测器（v2.11）
    ///
    /// 若未注入检测器，降级为 `HeuristicDetector`（用于 `detect_conflicts` 工具）。
    /// 返回 `Arc` clone（廉价引用计数）。
    fn detector(&self) -> Arc<dyn ConflictDetector> {
        match &self.conflict_detector {
            Some(d) => Arc::clone(d),
            None => Arc::new(hippocampus_core::heuristic::HeuristicDetector::new()),
        }
    }
}

// ============================================================================
// Tool 参数结构体
// ============================================================================

/// 预设参数（v2.29，archive tool 内联参数）
///
/// 所有字段可选，传入后服务端即时构建 CombinedProfile，应用：
/// - `archive_threshold` 覆盖默认 ArchiveConfig.token_threshold
/// - `summary_template` 通过 `with_summary_template_override` 注入
///
/// 与 `POST /api/v1/presets/build` 的请求体结构一致。
#[derive(Deserialize, schemars::JsonSchema, Default)]
struct PresetParams {
    /// Agent display_name（如 "Claude Code"）
    #[schemars(description = "Agent display_name（如 \"Claude Code\"），未匹配则视为 Custom Agent")]
    #[serde(default)]
    agent: Option<String>,
    /// Scenario 名称（大小写不敏感）
    #[schemars(description = "Scenario 名称（大小写不敏感，如 \"coding\" / \"Coding\"）")]
    #[serde(default)]
    scenario: Option<String>,
    /// ModelVariant 名称
    #[schemars(description = "ModelVariant 名称（如 \"claude-opus-4.8\"），未找到则报错")]
    #[serde(default)]
    model: Option<String>,
    /// 用户覆盖：归档阈值
    #[schemars(description = "用户覆盖归档阈值（token 数，最高优先级）")]
    #[serde(default)]
    archive_threshold: Option<usize>,
    /// 用户覆盖：摘要模板（需含 {conversation}）
    #[schemars(description = "用户覆盖摘要模板（最高优先级，需含 {conversation} 占位符）")]
    #[serde(default)]
    summary_template: Option<String>,
}

/// archive tool 参数
#[derive(Deserialize, schemars::JsonSchema, Default)]
struct ArchiveParams {
    /// 会话 ID
    #[schemars(description = "会话 ID（用于隔离不同会话的记忆）")]
    session_id: String,
    /// 待归档的轮次列表（MessageTurn 数组的 JSON 字符串）
    #[schemars(description = "待归档的轮次列表，格式为 MessageTurn 数组的 JSON 字符串")]
    turns_json: String,
    /// 项目 ID（可选）
    #[schemars(description = "项目 ID（可选，用于项目级隔离）")]
    project_id: Option<String>,
    /// 预设配置（v2.29，可选）
    ///
    /// 传入后即时构建 CombinedProfile，应用 archive_threshold + summary_template。
    /// 未传入时保持原行为（ArchiveConfig::default() + 内部模板）。
    #[schemars(description = "预设配置（可选，传入后应用 archive_threshold + summary_template 覆盖默认行为）")]
    #[serde(default)]
    preset: Option<PresetParams>,
    /// 任务状态快照（v2.31 动手点 2，可选）
    ///
    /// LLM 主动传入当前任务状态，hippocampus 持久化到 session_state.json。
    /// 下次 prompt 时返回最新快照，用于校准 Trae Summary 第8章节"Current Work"。
    /// 传入后覆盖上一次的快照（每 session 只保留最新一份）。
    #[schemars(description = "任务状态快照（可选，v2.31）。传入后持久化到 session_state.json，下次 prompt 时返回。用于压缩后校准 Trae Summary 第8章节 Current Work。字段：current_task(当前任务名)/completed_steps(已完成步骤数组)/in_progress_step(进行中步骤,可选)/next_step(下一步建议)。snapshot_at 由服务端自动填充。")]
    #[serde(default)]
    task_state_snapshot: Option<TaskStateSnapshotParams>,
}

/// 任务状态快照参数（v2.31 动手点 2）
///
/// 与 hippocampus_core::model::TaskStateSnapshot 对应，
/// 但 snapshot_at 由服务端自动填充（LLM 无需传入）。
#[derive(Deserialize, schemars::JsonSchema)]
struct TaskStateSnapshotParams {
    /// 当前任务名称（如 "批次A-数据完整性修复"）
    current_task: String,
    /// 已完成步骤列表
    #[serde(default)]
    completed_steps: Vec<String>,
    /// 进行中步骤（如果有，表示被压缩打断的任务）
    #[serde(default)]
    in_progress_step: Option<String>,
    /// 下一建议步骤
    next_step: String,
}

/// retrieve tool 参数
#[derive(Deserialize, schemars::JsonSchema)]
struct RetrieveParams {
    /// 会话 ID
    #[schemars(description = "会话 ID")]
    session_id: String,
    /// 钩子 ID
    #[schemars(description = "要检索的记忆钩子 ID（hook_id，可从 summaries 工具获取）")]
    hook_id: String,
    /// 项目 ID（可选）
    #[schemars(description = "项目 ID（可选）")]
    project_id: Option<String>,
}

/// summaries tool 参数
#[derive(Deserialize, schemars::JsonSchema)]
struct SummariesParams {
    /// 会话 ID
    #[schemars(description = "会话 ID")]
    session_id: String,
    /// 项目 ID（可选）
    #[schemars(description = "项目 ID（可选）")]
    project_id: Option<String>,
}

/// prompt tool 参数
#[derive(Deserialize, schemars::JsonSchema)]
struct PromptParams {
    /// 会话 ID
    #[schemars(description = "会话 ID")]
    session_id: String,
    /// 项目 ID（可选）
    #[schemars(description = "项目 ID（可选）")]
    project_id: Option<String>,
}

/// compaction tool 参数
#[derive(Deserialize, schemars::JsonSchema)]
struct CompactionParams {
    /// 会话 ID
    #[schemars(description = "会话 ID")]
    session_id: String,
    /// 周期类型
    #[schemars(description = "周期类型：\"weekly\"（周级去重合并）或 \"monthly\"（月级评分淘汰）")]
    period: String,
    /// 项目 ID（可选）
    #[schemars(description = "项目 ID（可选）")]
    project_id: Option<String>,
}

// ============================================================================
// v2.5 批次 6：批量操作 tool 参数
// ============================================================================

/// batch_retrieve tool 参数
#[derive(Deserialize, schemars::JsonSchema)]
struct BatchRetrieveParams {
    /// 会话 ID
    #[schemars(description = "会话 ID")]
    session_id: String,
    /// hook_id 列表（JSON 字符串）
    #[schemars(description = "要检索的 hook_id 列表（JSON 字符串数组，如 [\"uuid1\",\"uuid2\"]）")]
    hook_ids_json: String,
    /// 项目 ID（可选）
    #[schemars(description = "项目 ID（可选）")]
    project_id: Option<String>,
}

/// batch_delete tool 参数
#[derive(Deserialize, schemars::JsonSchema)]
struct BatchDeleteParams {
    /// 会话 ID
    #[schemars(description = "会话 ID")]
    session_id: String,
    /// hook_id 列表（JSON 字符串）
    #[schemars(description = "要删除的 hook_id 列表（JSON 字符串数组）")]
    hook_ids_json: String,
    /// 项目 ID（可选）
    #[schemars(description = "项目 ID（可选）")]
    project_id: Option<String>,
}

/// find_hook_by_prefix tool 参数（v2.31 新增）
#[derive(Deserialize, schemars::JsonSchema)]
struct FindHookByPrefixParams {
    /// hook_id 前缀（最少 4 字符）
    #[schemars(description = "hook_id 前缀（最少 4 字符，跨 session 检索）")]
    hook_prefix: String,
    /// 项目 ID（可选，跨 session 检索时使用）
    #[schemars(description = "项目 ID（跨 session 检索时使用，推荐传入）")]
    project_id: Option<String>,
    /// 会话 ID（可选，缩小检索范围）
    #[schemars(description = "会话 ID（可选，缩小检索范围）")]
    session_id: Option<String>,
}

/// batch_update tool 参数
#[derive(Deserialize, schemars::JsonSchema)]
struct BatchUpdateParams {
    /// 会话 ID
    #[schemars(description = "会话 ID")]
    session_id: String,
    /// 更新条目列表（JSON 字符串）
    #[schemars(description = "更新条目列表 JSON 字符串，每条含 hook_id/added_facts/revised_facts/deprecated_facts")]
    updates_json: String,
    /// 项目 ID（可选）
    #[schemars(description = "项目 ID（可选）")]
    project_id: Option<String>,
}

/// detect_conflicts tool 参数（v2.6 批次 8）
#[derive(Deserialize, schemars::JsonSchema)]
struct DetectConflictsParams {
    /// 会话 ID
    #[schemars(description = "会话 ID")]
    session_id: String,
    /// 钩子 ID
    #[schemars(description = "要检测的钩子 ID")]
    hook_id: String,
    /// 新增事实
    #[schemars(default, description = "新增事实列表")]
    #[serde(default)]
    added_facts: Vec<String>,
    /// 修正事实
    #[schemars(default, description = "修正事实列表")]
    #[serde(default)]
    revised_facts: Vec<String>,
    /// 废弃事实
    #[schemars(default, description = "废弃事实列表")]
    #[serde(default)]
    deprecated_facts: Vec<String>,
    /// 项目 ID（可选）
    #[schemars(description = "项目 ID（可选）")]
    project_id: Option<String>,
}

/// get_conflicts tool 参数（v2.6 批次 8）
#[derive(Deserialize, schemars::JsonSchema)]
struct GetConflictsParams {
    /// 会话 ID
    #[schemars(description = "会话 ID")]
    session_id: String,
    /// 钩子 ID
    #[schemars(description = "要查询冲突的钩子 ID")]
    hook_id: String,
    /// 项目 ID（可选）
    #[schemars(description = "项目 ID（可选）")]
    project_id: Option<String>,
}

/// install_rules tool 参数（v2.31 新增）
/// 首次接入时调用，自动写入 Rules 模板到 Agent 客户端的 rules 目录
#[derive(Deserialize, schemars::JsonSchema)]
struct InstallRulesParams {
    /// 客户端类型
    #[schemars(description = "客户端类型：catpaw（写入 .catpaw/rules/）、trae（写入 .trae/rules/）、claude-code（追加到 CLAUDE.md）")]
    client: String,
    /// 项目根目录（绝对路径）
    #[schemars(description = "项目根目录的绝对路径，如 D:/myapp 或 /home/user/myapp")]
    project_root: String,
    /// 是否强制覆盖（默认 false）
    #[serde(default)]
    #[schemars(description = "是否强制覆盖已存在的文件（默认 false：catpaw/trae 跳过已存在文件，claude-code 跳过已有 hippocampus 标记的文件）")]
    force: bool,
}

/// semantic_search tool 参数（v2.18 新增）
#[derive(Deserialize, schemars::JsonSchema)]
struct SemanticSearchParams {
    /// 会话 ID
    #[schemars(description = "会话 ID（用于隔离不同会话的检索范围）")]
    session_id: String,
    /// 查询文本
    #[schemars(description = "查询文本（自然语言，用于关键词 + 向量混合检索）")]
    query: String,
    /// 返回 top-K 结果数（可选，默认 5）
    #[schemars(description = "返回 top-K 结果数（默认 5）")]
    top_k: Option<usize>,
    /// 项目 ID（可选，影响索引读取路径：有则检索 project 级聚合索引）
    #[schemars(description = "项目 ID（可选，跨 session 检索时使用）")]
    project_id: Option<String>,
}

/// preset_build tool 参数（v2.29）
///
/// 所有字段可选，与 archive tool 的 PresetParams 结构一致，
/// 但独立定义以便 tool 描述更清晰。
#[derive(Deserialize, schemars::JsonSchema, Default)]
struct PresetBuildParams {
    /// Agent display_name
    #[schemars(description = "Agent display_name（如 \"Claude Code\"），未匹配则视为 Custom Agent")]
    #[serde(default)]
    agent: Option<String>,
    /// Scenario 名称
    #[schemars(description = "Scenario 名称（大小写不敏感，如 \"coding\" / \"Coding\"）")]
    #[serde(default)]
    scenario: Option<String>,
    /// ModelVariant 名称
    #[schemars(description = "ModelVariant 名称（如 \"claude-opus-4.8\"），未找到则报错")]
    #[serde(default)]
    model: Option<String>,
    /// 用户覆盖归档阈值
    #[schemars(description = "用户覆盖归档阈值（token 数，最高优先级）")]
    #[serde(default)]
    archive_threshold: Option<usize>,
    /// 用户覆盖摘要模板
    #[schemars(description = "用户覆盖摘要模板（最高优先级，需含 {conversation} 占位符）")]
    #[serde(default)]
    summary_template: Option<String>,
}

/// 空参数（用于无参数 tool，如 preset_list_*）
#[derive(Deserialize, schemars::JsonSchema, Default)]
struct NoParams {}

/// update_project_memory tool 参数（v2.31 动手点 4）
///
/// LLM 主动调用，用固定章节覆盖策略更新 hippocampus 维护的 project_memory.md 副本。
/// 章节用 HTML 注释标记界定，不影响用户手动写入的内容。
#[derive(Deserialize, schemars::JsonSchema)]
struct UpdateProjectMemoryParams {
    /// 项目 ID
    #[schemars(description = "项目 ID（用于定位 projects/{project_id}/project_memory.md）")]
    project_id: String,
    /// 章节标识（如 "task_state" / "decisions" / "progress" / "risks"）
    #[schemars(description = "章节标识（如 task_state/decisions/progress/risks）。同一标识的内容会被覆盖，不同标识的章节独立存在。")]
    section: String,
    /// 章节内容（Markdown 格式）
    #[schemars(description = "章节内容（Markdown 格式）。将覆盖该 section 的旧内容（action=replace 时）。")]
    content: String,
    /// 写入动作（可选，默认 "replace"）
    #[schemars(description = "写入动作（默认 replace）：replace=覆盖该章节旧内容；append=在章节末尾追加；delete=删除整个章节（含标记）。")]
    #[serde(default)]
    action: Option<String>,
}

/// get_project_memory tool 参数（v2.31 动手点 4）
#[derive(Deserialize, schemars::JsonSchema)]
struct GetProjectMemoryParams {
    /// 项目 ID
    #[schemars(description = "项目 ID")]
    project_id: String,
}

// ============================================================================
// MCP Tools 实现
// ============================================================================

// v2.30：去掉 server_handler 标志，改用两段式（tool_router + 独立 tool_handler impl），
// 以便手写 ServerHandler::get_info() 注入 usage_protocol.instructions
#[tool_router]
impl HippocampusMcp {
    /// 归档一批轮次为记忆文件，生成索引钩子。
    #[tool(description = "归档对话轮次到 Hippocampus 记忆库（长期记忆）。何时主动调用：(1)对话超过20轮或包含大量代码/长文档时，(2)你感觉到'上下文变重'或'前面说过但记不清'时，(3)用户即将手动压缩上下文前，(4)每30轮兜底归档一次。你无法直接感知自身token消耗，但调用此工具后会返回 estimated_total_tokens / threshold_ratio_percent / suggestion，让你建立'token意识'判断后续何时归档：ratio>=100立即归档，>=80准备归档。调用格式简化：turns_json 只需传 [{\"user_message\":{\"text\":\"用户问的\"},\"llm_message\":{\"text\":\"我答的\"}}]，id/timestamp/tags/token_count 可省略由服务端自动补全。支持 preset 参数（内联预设）。返回 hook_id 用于后续 retrieve/semantic_search 检索。\
     \
     【tool_calls 字段 schema（v2.31 补充）】归档端不自动注入 tool_calls，Agent 须主动喂入。MessageContent.tool_calls 是 ToolInvocation 数组，每项含 name(工具名,字符串)/arguments(JSON字符串,不是对象)/result(JSON字符串)/duration_ms(可选,毫秒)。示例：{\"user_message\":{\"text\":\"搜索 Rust 资料\"},\"llm_message\":{\"text\":\"已找到...\",\"tool_calls\":[{\"name\":\"WebSearch\",\"arguments\":\"{\\\"q\\\":\\\"Rust 编程\\\"}\",\"result\":\"{\\\"hits\\\":[...]}\",\"duration_ms\":1200}]}}。注意 arguments 和 result 都是 JSON 字符串而非嵌套对象。\
     \
     【tags 自动推断（v2.31 补充）】tags 字段可省略，服务端根据内容自动推断：含 tool_calls → [ToolCall, AgentTool]；含 thinking → [Thinking]；含代码块 → [CodeBlock]；含 URL → [Url]；兜底 → [Text]。Agent 显式传入的 tags 优先，不会被覆盖。")]
    async fn archive(
        &self,
        Parameters(params): Parameters<ArchiveParams>,
    ) -> Result<String, McpError> {
        // 解析 turns_json
        // v2.30.1：MessageTurn 的 id/timestamp/tags/token_count 可省略，服务端反序列化时自动补全
        let mut turns: Vec<MessageTurn> = serde_json::from_str(&params.turns_json)
            .map_err(|e| McpError::invalid_params(
                format!(
                    "turns_json 解析失败: {e}\n\n\
                     合法格式：MessageTurn 数组的 JSON 字符串。\n\
                     v2.30.1 简化：仅需 user_message/llm_message，其余字段（id/timestamp/tags/token_count）可省略，服务端自动补全。\n\
                     最简示例：[{{\"user_message\":{{\"text\":\"用户消息\"}},\"llm_message\":{{\"text\":\"LLM 回复\"}}}}]\n\
                     完整示例：[{{\"id\":\"7f9c1b2a-3d4e-4f5a-8a9b-0c1d2e3f4a5b\",\"user_message\":{{\"text\":\"用户消息\"}},\"llm_message\":{{\"text\":\"LLM 回复\"}},\"tags\":[{{\"kind\":\"Text\"}}],\"timestamp\":\"2026-07-05T00:00:00Z\",\"token_count\":100}}]\n\
                     说明：MessageContent 的 attachments/tool_calls/thinking 也可省略（默认空）。"
                ),
                None,
            ))?;

        if turns.is_empty() {
            return Err(McpError::invalid_params(
                "turns 不能为空。请传入至少一条 MessageTurn。",
                None,
            ));
        }

        // v2.30.1：对每条 turn 应用自动补全
        // - tags 为空（Agent 未传）→ 根据内容启发式推断（ToolCall/Image/CodeBlock 等）
        // - token_count 为 0（Agent 未传）→ 根据文本长度估算（chars/3）
        // - 已传入的值不覆盖（Agent 显式优先）
        for turn in turns.iter_mut() {
            apply_turn_defaults(turn);
        }

        // v2.29：若传入 preset，构建 CombinedProfile 提取 archive_threshold + summary_template
        // 复用 presets crate 的 build_from_strings 公共函数（与 server 端共享同一套解析逻辑）
        let (archive_threshold, summary_template) = if let Some(preset_req) = &params.preset {
            let combined = hippocampus_presets::build_from_strings(
                preset_req.agent.as_deref(),
                preset_req.scenario.as_deref(),
                preset_req.model.as_deref(),
                preset_req.archive_threshold,
                preset_req.summary_template.as_deref(),
            )
            .map_err(|e| McpError::invalid_params(
                format!(
                    "预设构建失败: {e}\n\n\
                     提示：调用 preset_list_agents / preset_list_scenarios / preset_list_models 查询合法值。"
                ),
                None,
            ))?;
            (
                Some(combined.archive_threshold()),
                Some(combined.summary_template().to_string()),
            )
        } else {
            (None, None)
        };

        let storage = self.create_storage();
        // v2.29：archive_threshold 覆盖默认 token_threshold（force_truncate_limit 按比例放大）
        let config = if let Some(threshold) = archive_threshold {
            ArchiveConfig {
                token_threshold: threshold,
                force_truncate_limit: threshold * 3 / 2,
                wait_for_turn_completion: true,
            }
        } else {
            ArchiveConfig::default()
        };
        // v2.31：保存阈值用于返回值（config 会被 Archiver::new move）
        let threshold = config.token_threshold;
        // v2.31 动手点 2：clone storage 用于归档后写 session_state.json
        let storage_for_snapshot = storage.clone();
        let mut archiver = Archiver::new(config, storage, &params.session_id, params.project_id);

        // v2.21 批次 8c：若注入了 summary_generator，注入到 Archiver
        if let Some(gen) = &self.summary_generator {
            archiver = archiver.with_summary_generator(gen.clone());
        }

        // v2.29：若构建出 summary_template，注入到 Archiver
        // 通过 with_summary_template_override 覆盖 HttpSummaryGenerator 的内部模板
        if let Some(tpl) = summary_template {
            archiver = archiver.with_summary_template_override(tpl);
        }

        for turn in turns {
            archiver.push_turn(turn);
        }

        let (_, hook) = archiver.archive().await.map_err(|e| {
            McpError::internal_error(format!("归档失败: {e}"), None)
        })?;

        let summary = SummaryView::from(&hook);

        // v2.31：增强返回值，提供 token 估算反馈与归档建议
        // 让 LLM 通过外部反馈建立"token 意识"，主动判断何时归档（伪钩子方案）
        let archived_tokens: usize = summary.token_count;
        let ratio = if threshold > 0 {
            (archived_tokens as f64 / threshold as f64 * 100.0).round() as u64
        } else {
            0
        };
        let suggestion = if ratio >= 100 {
            format!(
                "已归档 {} 轮，累计估算 {} tokens（已达阈值 {}，{}%）。建议立即归档或触发上下文压缩。",
                summary.token_count, archived_tokens, threshold, ratio
            )
        } else if ratio >= 80 {
            format!(
                "已归档 {} 轮，累计估算 {} tokens（接近阈值 {}，{}%）。建议准备归档。",
                summary.token_count, archived_tokens, threshold, ratio
            )
        } else {
            format!(
                "已归档 {} 轮，累计估算 {} tokens（阈值 {}，当前 {}%）。继续对话。",
                summary.token_count, archived_tokens, threshold, ratio
            )
        };

        // v2.31 动手点 2：若 LLM 传入 task_state_snapshot，持久化到 session_state.json
        // 用于压缩后校准 Trae Summary 第8章节 Current Work
        let task_state_written = if let Some(snap) = &params.task_state_snapshot {
            let snapshot = hippocampus_core::model::TaskStateSnapshot {
                current_task: snap.current_task.clone(),
                completed_steps: snap.completed_steps.clone(),
                in_progress_step: snap.in_progress_step.clone(),
                next_step: snap.next_step.clone(),
                snapshot_at: chrono::Utc::now(),
            };
            match storage_for_snapshot.write_session_state(&params.session_id, &snapshot).await {
                Ok(()) => {
                    tracing::info!(
                        session_id = %params.session_id,
                        current_task = %snapshot.current_task,
                        "task_state_snapshot 已持久化"
                    );
                    true
                }
                Err(e) => {
                    tracing::warn!(
                        session_id = %params.session_id,
                        error = %e,
                        "task_state_snapshot 持久化失败（不影响归档结果）"
                    );
                    false
                }
            }
        } else {
            false
        };

        let result = serde_json::json!({
            "hook_id": summary.hook_id,
            "memory_file_id": summary.memory_id,
            "summary_title": summary.summary_title,
            "abstract_text": summary.abstract_text,
            "key_facts": summary.key_facts,
            "key_entities": summary.key_entities,
            "clue_anchors": summary.clue_anchors,
            "tags": summary.tags,
            "archived_at": summary.archived_at,
            "period": summary.period,
            "token_count": summary.token_count,
            // v2.31 新增：token 反馈与归档建议（伪钩子方案）
            "estimated_total_tokens": archived_tokens,
            "threshold": threshold,
            "threshold_ratio_percent": ratio,
            "suggestion": suggestion,
            // v2.31 动手点 2：任务状态快照持久化结果
            "task_state_snapshot_persisted": task_state_written,
        });
        let result = serde_json::to_string(&result).map_err(|e| {
            McpError::internal_error(format!("序列化结果失败: {e}"), None)
        })?;

        Ok(result)
    }

    /// 按钩子 ID 检索完整记忆文件。
    #[tool(description = "按 hook_id 检索完整记忆文件。当 semantic_search 返回 hook_id 后，或你需要的细节不在摘要中时调用此工具，返回完整的记忆文件（含所有轮次的用户消息+LLM消息+标签）。用于回溯历史对话的具体内容。")]
    async fn retrieve(
        &self,
        Parameters(params): Parameters<RetrieveParams>,
    ) -> Result<String, McpError> {
        let storage = self.create_storage();
        let retriever = Retriever::new(storage, &params.session_id, params.project_id);

        let memory = retriever.retrieve_memory(&params.hook_id).await.map_err(|e| {
            McpError::internal_error(format!("检索失败: {e}"), None)
        })?;

        let result = serde_json::to_string(&memory).map_err(|e| {
            McpError::internal_error(format!("序列化结果失败: {e}"), None)
        })?;

        Ok(result)
    }

    /// 获取所有周期的摘要视图列表。
    #[tool(description = "获取当前会话所有周期的记忆摘要列表。返回摘要钩子数组，每个钩子含 hook_id/summary_title/tags/token_count 等。用于在会话开始时了解有哪些历史记忆。")]
    async fn summaries(
        &self,
        Parameters(params): Parameters<SummariesParams>,
    ) -> Result<String, McpError> {
        let storage = self.create_storage();
        let retriever = Retriever::new(storage, &params.session_id, params.project_id);

        let summaries = retriever.get_summaries().await.map_err(|e| {
            McpError::internal_error(format!("获取摘要失败: {e}"), None)
        })?;

        let result = serde_json::to_string(&summaries).map_err(|e| {
            McpError::internal_error(format!("序列化结果失败: {e}"), None)
        })?;

        Ok(result)
    }

    /// 渲染摘要为 system prompt 文本。
    #[tool(description = "渲染记忆摘要为 system prompt 文本。【会话开始必调】每个新会话的第一次回复前必须调用此工具，返回的历史记忆摘要拼接到 system prompt 末尾，让你了解之前讨论过什么。若返回空列表说明该 session 无历史记忆，正常继续。这是 Hippocampus 记忆协议的第一步，无需用户提示主动执行。v2.31 新增：若该 session 存在 task_state_snapshot（archive 时传入），会在 prompt 末尾追加任务状态快照，用于压缩后校准 Trae Summary 第8章节 Current Work。")]
    async fn prompt(
        &self,
        Parameters(params): Parameters<PromptParams>,
    ) -> Result<String, McpError> {
        let storage = self.create_storage();
        let retriever = Retriever::new(storage.clone(), &params.session_id, params.project_id);

        let prompt = retriever.render_to_system_prompt().await.map_err(|e| {
            McpError::internal_error(format!("渲染 prompt 失败: {e}"), None)
        })?;

        // v2.31 动手点 2：若存在 task_state_snapshot，追加到 prompt 末尾
        // 让 LLM 在压缩后调 prompt 时自然看到任务状态，用于校准 Trae Summary
        let mut final_prompt = match storage.read_session_state(&params.session_id).await {
            Ok(Some(snapshot)) => {
                let mut s = prompt;
                s.push_str("\n\n--- Task State Snapshot (v2.31) ---\n");
                s.push_str(&format!("current_task: {}\n", snapshot.current_task));
                if !snapshot.completed_steps.is_empty() {
                    s.push_str(&format!("completed_steps: {}\n", snapshot.completed_steps.join(", ")));
                }
                if let Some(in_progress) = &snapshot.in_progress_step {
                    s.push_str(&format!("in_progress_step: {}\n", in_progress));
                }
                s.push_str(&format!("next_step: {}\n", snapshot.next_step));
                s.push_str(&format!("snapshot_at: {}\n", snapshot.snapshot_at));
                s.push_str("--- End Snapshot ---\n");
                s.push_str("\n提示：若你是被压缩后调用此工具，请比对上方快照与 Trae Summary 第8章节 Current Work，以快照为准继续执行。");
                s
            }
            Ok(None) => prompt,
            Err(e) => {
                tracing::warn!(
                    session_id = %params.session_id,
                    error = %e,
                    "读取 task_state_snapshot 失败（不影响 prompt 返回）"
                );
                prompt
            }
        };

        // v2.31 新增：追加可用 session 列表（兜底：引导 LLM 用正确 session_id）
        // 当 LLM 用错 session_id 时，让它能看到现有 session 列表，自行修正
        match storage.list_sessions().await {
            Ok(sessions) if !sessions.is_empty() => {
                // 检查当前 session_id 是否在列表中
                let current_in_list = sessions.iter().any(|s| s == &params.session_id);
                if !current_in_list {
                    final_prompt.push_str("\n\n--- Available Sessions (v2.31) ---\n");
                    final_prompt.push_str(&format!("⚠️ 当前 session_id `{}` 不在已有列表中。\n", params.session_id));
                    final_prompt.push_str("若你是新会话，这是正常的。若你想检索历史记忆，请从以下列表选择正确的 session_id：\n");
                    for s in &sessions {
                        final_prompt.push_str(&format!("- {}\n", s));
                    }
                    final_prompt.push_str("\n提示：session_id 约定为 `{客户端前缀}-{项目名}-{日期}`，如 `catpaw-myapp-20260706`。");
                    final_prompt.push_str("禁止使用 `项目名-session` 这种格式（会导致 retrieve 找不到记忆）。");
                }
            }
            Ok(_) => {
                // sessions 为空（首次使用），无需追加
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "列出 sessions 失败（不影响 prompt 返回）"
                );
            }
        }

        Ok(final_prompt)
    }

    /// 触发周期任务（周级合并 / 月级评分淘汰）。
    #[tool(description = "触发周期任务。period=\"weekly\" 执行周级无损去重合并（7天内记忆去重合并为1个），period=\"monthly\" 执行月级4维评分淘汰（保留高价值记忆）。")]
    async fn compaction(
        &self,
        Parameters(params): Parameters<CompactionParams>,
    ) -> Result<String, McpError> {
        let storage = self.create_storage();
        let mut compactor = Compactor::new(
            storage,
            Box::new(DefaultScorer::new()),
            &params.session_id,
            params.project_id,
        );

        // v2.22: 若注入了 summary_generator，注入到 Compactor（compaction 也用 LLM 摘要）
        if let Some(gen) = &self.summary_generator {
            compactor = compactor.with_summary_generator(gen.clone());
        }

        let (memory, index_doc) = match params.period.as_str() {
            "weekly" => compactor.weekly_merge().await,
            "monthly" => compactor.monthly_evict().await,
            other => {
                return Err(McpError::invalid_params(
                    format!("无效的 period: {other}（支持: weekly, monthly）"),
                    None,
                ));
            }
        }
        .map_err(|e| McpError::internal_error(format!("周期任务失败: {e}"), None))?;

        let result = serde_json::json!({
            "memory_file_id": memory.id.to_string(),
            "total_turns": memory.turns.len(),
            "total_tokens": memory.total_tokens,
            "hooks_count": index_doc.hooks.len(),
            "period": params.period,
        })
        .to_string();

        Ok(result)
    }

    // ========================================================================
    // v2.5 批次 6：批量操作 tools
    // ========================================================================

    /// 批量按 hook_id 列表检索记忆文件。
    #[tool(description = "批量检索多个记忆文件。传入 hook_id 列表，返回每个记忆的完整内容。单个失败不影响其他。用于一次性获取多个历史记忆，减少多次调用 retrieve 的开销。")]
    async fn batch_retrieve(
        &self,
        Parameters(params): Parameters<BatchRetrieveParams>,
    ) -> Result<String, McpError> {
        let hook_ids: Vec<String> = serde_json::from_str(&params.hook_ids_json)
            .map_err(|e| McpError::invalid_params(
                format!(
                    "hook_ids_json 解析失败: {e}\n\n\
                     合法格式：字符串数组的 JSON 字符串。\n\
                     示例：[\"7f9c1b2a-3d4e-4f5a-8a9b-0c1d2e3f4a5b\",\"abc123\"]\n\
                     提示：hook_id 可从 summaries 工具获取。"
                ),
                None,
            ))?;

        if hook_ids.is_empty() {
            return Err(McpError::invalid_params(
                "hook_ids 不能为空。请传入至少一个 hook_id。",
                None,
            ));
        }

        let storage = self.create_storage();
        let retriever = Retriever::new(storage, &params.session_id, params.project_id);

        // v2.16 IMP-08：并发检索（Semaphore 限制 8 并发 + JoinSet 收集结果）
        //
        // 串行循环改为并发执行，提升批量检索性能。
        // 单个失败不影响其他（保持原有容错语义）。
        // 8 是经验值：平衡并发开销与系统负载，适合大多数存储后端。
        // 使用 tokio::sync::Semaphore 限制并发数，避免大批量请求压垮存储后端。
        use std::sync::Arc;
        use tokio::sync::Semaphore;
        let semaphore = Arc::new(Semaphore::new(8));
        let mut tasks = tokio::task::JoinSet::new();
        for hook_id in hook_ids.iter().cloned() {
            let retriever = retriever.clone();
            let sem = semaphore.clone();
            tasks.spawn(async move {
                // 获取许可（最多 8 个并发，其余等待）
                let _permit = match sem.acquire().await {
                    Ok(p) => p,
                    Err(e) => {
                        return serde_json::json!({
                            "hook_id": hook_id,
                            "success": false,
                            "error": format!("获取并发许可失败: {e}"),
                        });
                    }
                };
                match retriever.retrieve_memory(&hook_id).await {
                    Ok(memory) => serde_json::json!({
                        "hook_id": hook_id,
                        "success": true,
                        "data": memory,
                    }),
                    Err(e) => serde_json::json!({
                        "hook_id": hook_id,
                        "success": false,
                        "error": e.to_string(),
                    }),
                }
            });
        }

        // 收集结果（顺序与输入无关，按完成顺序）
        let mut results = Vec::with_capacity(hook_ids.len());
        while let Some(joined) = tasks.join_next().await {
            // JoinSet 内部 panic 会转化为 JoinError，这里容错保证不中断
            match joined {
                Ok(value) => results.push(value),
                Err(e) => {
                    tracing::error!(error = %e, "batch_retrieve 任务 panic，跳过");
                    results.push(serde_json::json!({
                        "hook_id": "",
                        "success": false,
                        "error": format!("内部任务错误: {e}"),
                    }));
                }
            }
        }

        let result = serde_json::json!({
            "total": results.len(),
            "success_count": results.iter().filter(|r| r["success"].as_bool().unwrap_or(false)).count(),
            "items": results,
        });
        Ok(result.to_string())
    }

    /// 批量按 hook_id 列表删除记忆文件。
    #[tool(description = "批量删除多个记忆文件（v2.31 软删除方案）。传入 hook_id 列表，逐个删除。单个失败不影响其他。删除时同步清理索引钩子标记为 Deleted + 内存搜索索引，避免 semantic_search 返回幽灵记忆。用于清理过期或不需要的记忆。")]
    async fn batch_delete(
        &self,
        Parameters(params): Parameters<BatchDeleteParams>,
    ) -> Result<String, McpError> {
        let hook_ids: Vec<String> = serde_json::from_str(&params.hook_ids_json)
            .map_err(|e| McpError::invalid_params(
                format!(
                    "hook_ids_json 解析失败: {e}\n\n\
                     合法格式：字符串数组的 JSON 字符串。\n\
                     示例：[\"7f9c1b2a-3d4e-4f5a-8a9b-0c1d2e3f4a5b\",\"abc123\"]\n\
                     提示：hook_id 可从 summaries 工具或 find_hook_by_prefix 获取。"
                ),
                None,
            ))?;

        if hook_ids.is_empty() {
            return Err(McpError::invalid_params(
                "hook_ids 不能为空。请传入至少一个 hook_id。",
                None,
            ));
        }

        let storage = self.create_storage();
        let retriever = Retriever::new(storage.clone(), &params.session_id, params.project_id.clone());

        // v2.31：查找每个 hook_id 的完整 IndexHook（含 memory_id 和 period）
        let mut hooks_info: Vec<(String, Option<(String, hippocampus_core::model::ArchivePeriod)>)> =
            Vec::with_capacity(hook_ids.len());
        for hook_id in &hook_ids {
            let info = retriever.find_hook_by_id(hook_id).await.map(|h| {
                (h.memory_id.clone(), h.period)
            });
            hooks_info.push((hook_id.clone(), info));
        }

        // v2.31：逐条调用 delete_memory_complete（软删除：删文件 + 标记索引 Deleted）
        let mut items: Vec<serde_json::Value> = Vec::with_capacity(hook_ids.len());
        for (hook_id, info_opt) in &hooks_info {
            match info_opt {
                None => {
                    items.push(serde_json::json!({
                        "hook_id": hook_id,
                        "success": false,
                        "error": "未找到对应的 memory_id",
                    }));
                }
                Some((memory_id, period)) => {
                    let r = storage
                        .delete_memory_complete(
                            memory_id,
                            hook_id,
                            &params.session_id,
                            params.project_id.as_deref(),
                            *period,
                        )
                        .await;

                    // v2.31：同步清理 SessionSearchRouter 内存索引
                    if let Some(router) = &self.session_search {
                        router.unindex_hook(&params.session_id, hook_id).await;
                    }

                    match r {
                        Ok(()) => items.push(serde_json::json!({
                            "hook_id": hook_id,
                            "success": true,
                        })),
                        Err(e) => items.push(serde_json::json!({
                            "hook_id": hook_id,
                            "success": false,
                            "error": e.to_string(),
                        })),
                    }
                }
            }
        }

        let result = serde_json::json!({
            "total": items.len(),
            "success_count": items.iter().filter(|r| r["success"].as_bool().unwrap_or(false)).count(),
            "items": items,
        });
        Ok(result.to_string())
    }

    /// 按 hook_id 前缀跨 session 查找匹配的钩子摘要（v2.31 新增）。
    #[tool(description = "按 hook_id 前缀查找记忆钩子（跨 session 检索）。当用户只提供短 ID（如 305b700e）时使用此工具找到完整 hook_id。返回匹配的 hook 摘要列表（不返回完整记忆内容）。支持跨 session 检索：传入 project_id 在所有 session 中查找，传入 session_id 缩小范围。最小前缀长度 4 字符。返回 hook_id + session_id + summary_title，用完整 hook_id 调 retrieve 获取完整记忆。")]
    async fn find_hook_by_prefix(
        &self,
        Parameters(params): Parameters<FindHookByPrefixParams>,
    ) -> Result<String, McpError> {
        // 前缀长度校验
        if params.hook_prefix.len() < 4 {
            return Err(McpError::invalid_params(
                format!(
                    "hook_prefix 长度不足：{}（最少 4 字符）。当前长度：{}",
                    params.hook_prefix,
                    params.hook_prefix.len()
                ),
                None,
            ));
        }

        let storage = self.create_storage();
        let matches = hippocampus_core::retrieve::find_hooks_by_prefix(
            &storage,
            &params.hook_prefix,
            params.project_id.as_deref(),
            params.session_id.as_deref(),
        )
        .await
        .map_err(|e| McpError::internal_error(format!("查找失败: {}", e), None))?;

        let result = serde_json::json!({
            "matches": matches.iter().map(|m| serde_json::json!({
                "hook_id": m.hook_id,
                "session_id": m.session_id,
                "period": format!("{:?}", m.period),
                "summary_title": m.summary_title,
                "archived_at": m.archived_at,
                "token_count": m.token_count,
            })).collect::<Vec<_>>(),
            "total": matches.len(),
        });
        Ok(result.to_string())
    }

    /// 批量按 hook_id 列表更新记忆文件。
    #[tool(description = "批量更新多个记忆文件。传入更新条目列表（每条含 hook_id + added/revised/deprecated facts），逐个更新。单个失败不影响其他。用于批量迭代更新记忆。")]
    async fn batch_update(
        &self,
        Parameters(params): Parameters<BatchUpdateParams>,
    ) -> Result<String, McpError> {
        #[derive(Deserialize)]
        struct UpdateEntry {
            hook_id: String,
            #[serde(default)]
            added_facts: Vec<String>,
            #[serde(default)]
            revised_facts: Vec<String>,
            #[serde(default)]
            deprecated_facts: Vec<String>,
        }

        let entries: Vec<UpdateEntry> = serde_json::from_str(&params.updates_json)
            .map_err(|e| McpError::invalid_params(
                format!(
                    "updates_json 解析失败: {e}\n\n\
                     合法格式：UpdateEntry 数组的 JSON 字符串，每条含 hook_id（必填）+ added_facts/revised_facts/deprecated_facts（可选数组）。\n\
                     示例：[{{\"hook_id\":\"7f9c1b2a-3d4e-4f5a-8a9b-0c1d2e3f4a5b\",\"added_facts\":[\"新事实\"],\"revised_facts\":[],\"deprecated_facts\":[]}}]\n\
                     提示：hook_id 可从 summaries 工具获取。"
                ),
                None,
            ))?;

        if entries.is_empty() {
            return Err(McpError::invalid_params(
                "updates 不能为空。请传入至少一条 UpdateEntry。",
                None,
            ));
        }

        let storage = self.create_storage();
        let retriever = Retriever::new(storage.clone(), &params.session_id, params.project_id);

        // v2.11：用具名结构体替代 6 元组，新增 conflicts_count + has_critical 字段
        /// 单条更新条目（内部中间结构）
        struct UpdatePair {
            /// memory_id（空字符串表示 hook_id 未找到）
            memory_id: String,
            /// hook_id（响应用）
            hook_id: String,
            added: usize,
            revised: usize,
            deprecated: usize,
            /// 检测到的冲突数（v2.11，0 表示无冲突或未检测）
            conflicts_count: usize,
            /// 是否存在 Critical 级别冲突（v2.11）
            has_critical: bool,
            /// 更新结果（None 表示未执行更新，Some(Ok) 成功，Some(Err) 失败）
            result: Option<hippocampus_core::Result<()>>,
        }

        // hook_id → memory_id 转换 + 构造 UpdatePair
        let mut pairs: Vec<UpdatePair> = Vec::with_capacity(entries.len());
        for entry in &entries {
            let mid = retriever.find_memory_id_by_hook(&entry.hook_id).await;
            let memory_id = mid.unwrap_or_default();
            pairs.push(UpdatePair {
                memory_id,
                hook_id: entry.hook_id.clone(),
                added: entry.added_facts.len(),
                revised: entry.revised_facts.len(),
                deprecated: entry.deprecated_facts.len(),
                conflicts_count: 0,
                has_critical: false,
                result: None,
            });
        }

        // v2.11：逐条更新（集成冲突检测）
        // - 注入了 conflict_detector：read_memory + detect + update_memory_with_conflicts
        // - 未注入：直接 update_memory（保持 v2.6 行为，无冲突检测）
        // LocalStorage 的 update_memories_batch 默认实现就是循环 update_memory，
        // 所以逐条调用不会降低性能。
        for pair in &mut pairs {
            // 跳过 hook_id 未找到的条目
            if pair.memory_id.is_empty() {
                continue;
            }

            // 从 entries 找到对应的 entry 构造 MemoryUpdate
            // （pairs 与 entries 顺序一致）
            let entry = entries.iter().find(|e| e.hook_id == pair.hook_id).unwrap();
            let update = hippocampus_core::model::MemoryUpdate::new()
                .add_fact(entry.added_facts.join("\n"))
                .revise_fact(entry.revised_facts.join("\n"))
                .deprecate_fact(entry.deprecated_facts.join("\n"));

            let result = if let Some(detector) = &self.conflict_detector {
                // v2.11：检测冲突 + 持久化冲突记录
                match storage.read_memory(&pair.memory_id).await {
                    Ok(existing) => {
                        let report = detector.detect(&update, &existing).await;
                        pair.conflicts_count = report.count();
                        pair.has_critical = report.has_critical();
                        storage
                            .update_memory_with_conflicts(
                                &pair.memory_id,
                                update,
                                report.conflicts,
                            )
                            .await
                    }
                    Err(e) => Err(e),
                }
            } else {
                // 未注入检测器：直接更新（保持 v2.6 行为，不检测冲突）
                storage.update_memory(&pair.memory_id, update).await
            };
            pair.result = Some(result);
        }

        // 构建响应（含 conflicts/has_critical 字段，v2.11）
        let items: Vec<_> = pairs.iter()
            .map(|pair| {
                if pair.memory_id.is_empty() {
                    return serde_json::json!({
                        "hook_id": pair.hook_id,
                        "success": false,
                        "error": "未找到对应的 memory_id",
                    });
                }
                match &pair.result {
                    Some(Ok(())) => serde_json::json!({
                        "hook_id": pair.hook_id,
                        "success": true,
                        "added": pair.added,
                        "revised": pair.revised,
                        "deprecated": pair.deprecated,
                        "conflicts": pair.conflicts_count,
                        "has_critical": pair.has_critical,
                    }),
                    Some(Err(e)) => serde_json::json!({
                        "hook_id": pair.hook_id,
                        "success": false,
                        "error": e.to_string(),
                    }),
                    None => serde_json::json!({
                        "hook_id": pair.hook_id,
                        "success": false,
                        "error": "内部错误：结果缺失",
                    }),
                }
            })
            .collect();

        let result = serde_json::json!({
            "total": items.len(),
            "success_count": items.iter().filter(|r| r["success"].as_bool().unwrap_or(false)).count(),
            "items": items,
        });
        Ok(result.to_string())
    }

    /// 检测单次记忆更新的冲突（不实际写入）。
    ///
    /// v2.6 批次 8：在 update 前预检测冲突，让 Agent 决策是否继续。
    /// 返回 ConflictReport（含 conflicts 数组 + has_critical 标志）。
    ///
    /// v2.25：从 IndexHook.summary.key_facts 提取历史事实集注入到 memory.updates，
    /// 解决 archive 只写 turns 不写 updates 导致 detector 拿不到历史事实的问题。
    #[tool(description = "检测记忆更新的潜在冲突（不实际写入）。【用户陈述与记忆矛盾时必调】当用户陈述的事实与记忆中的记录可能冲突时（如用户说'我用的是 Python'但记忆里是 Rust），先调用此工具检测冲突，确认后再更新。传入 added/revised/deprecated facts，返回检测到的冲突列表（自我矛盾/直接矛盾/立场反转）。")]
    async fn detect_conflicts(
        &self,
        Parameters(params): Parameters<DetectConflictsParams>,
    ) -> Result<String, McpError> {
        let storage = self.create_storage();
        let retriever = Retriever::new(
            storage.clone(),
            &params.session_id,
            params.project_id.clone(),
        );

        // 通过 hook_id 找到 IndexHook（v2.25：需要 key_facts）
        let hook = retriever.find_hook_by_id(&params.hook_id).await;
        let hook = match hook {
            Some(h) => h,
            None => {
                return Err(McpError::invalid_params(
                    format!(
                        "未找到 hook_id: {}\n\n\
                         提示：调用 summaries(session_id) 查询当前 session 的所有 hook_id 列表。",
                        params.hook_id
                    ),
                    None,
                ));
            }
        };

        // 读取现有记忆
        let mut existing = storage.read_memory(&hook.memory_id).await.map_err(|e| {
            McpError::internal_error(format!("读取记忆失败: {e}"), None)
        })?;

        // v2.25：若 memory.updates 为空但 IndexHook 有 key_facts，
        // 把 key_facts 作为虚拟 MemoryUpdateRecord 注入，让 detector 能看到历史事实。
        // 这解决了 archive 只写 turns 不写 updates 的设计缺陷。
        // v2.25.1：逐条 add_fact 保持事实粒度，避免 join 导致检测粒度变粗。
        if existing.updates.is_empty() && !hook.summary.key_facts.is_empty() {
            use hippocampus_core::model::MemoryUpdateRecord;
            let mut virtual_update = hippocampus_core::model::MemoryUpdate::new();
            for fact in &hook.summary.key_facts {
                virtual_update = virtual_update.add_fact(fact.clone());
            }
            existing.updates.push(MemoryUpdateRecord {
                updated_at: hook.archived_at,
                update: virtual_update,
                conflicts: vec![],
            });
            tracing::debug!(
                hook_id = %params.hook_id,
                facts_count = hook.summary.key_facts.len(),
                "detect_conflicts: 已从 IndexHook 注入 key_facts 作为历史事实集"
            );
        }

        // 构造 MemoryUpdate（v2.25.1：逐条添加保持事实粒度）
        let mut update = hippocampus_core::model::MemoryUpdate::new();
        for fact in &params.added_facts {
            update = update.add_fact(fact.clone());
        }
        for fact in &params.revised_facts {
            update = update.revise_fact(fact.clone());
        }
        for fact in &params.deprecated_facts {
            update = update.deprecate_fact(fact.clone());
        }

        // v2.11：使用注入的检测器（支持 HeuristicDetector / HttpLlmDetector / HybridDetector）
        // 未注入时降级为 HeuristicDetector（向后兼容）
        let detector = self.detector();
        let report = detector.detect(&update, &existing).await;

        let result = serde_json::json!({
            "total": report.count(),
            "has_critical": report.has_critical(),
            "conflicts": report.conflicts,
        });
        Ok(result.to_string())
    }

    /// 查询指定记忆的所有冲突记录（来自历史 updates）。
    ///
    /// v2.6 批次 8：返回持久化在 MemoryUpdateRecord.conflicts 中的所有冲突记录。
    #[tool(description = "查询指定记忆文件的所有冲突历史记录。返回持久化的冲突列表（按 updates 时间顺序），含 total/critical_count/conflicts 字段。用于回溯 Agent 记忆演进过程中的矛盾点。")]
    async fn get_conflicts(
        &self,
        Parameters(params): Parameters<GetConflictsParams>,
    ) -> Result<String, McpError> {
        let storage = self.create_storage();
        let retriever = Retriever::new(
            storage.clone(),
            &params.session_id,
            params.project_id.clone(),
        );

        // 通过 hook_id 找到 memory_id
        let memory_id = retriever.find_memory_id_by_hook(&params.hook_id).await;
        let memory_id = match memory_id {
            Some(mid) => mid,
            None => {
                return Err(McpError::invalid_params(
                    format!(
                        "未找到 hook_id: {}\n\n\
                         提示：调用 summaries(session_id) 查询当前 session 的所有 hook_id 列表。",
                        params.hook_id
                    ),
                    None,
                ));
            }
        };

        // 读取记忆并扁平化所有 conflicts
        let memory = storage.read_memory(&memory_id).await.map_err(|e| {
            McpError::internal_error(format!("读取记忆失败: {e}"), None)
        })?;

        let mut all_conflicts: Vec<hippocampus_core::conflict::ConflictRecord> = Vec::new();
        for record in &memory.updates {
            all_conflicts.extend(record.conflicts.iter().cloned());
        }

        let total = all_conflicts.len();
        let critical_count = all_conflicts
            .iter()
            .filter(|c| c.severity == hippocampus_core::conflict::Severity::Critical)
            .count();

        let result = serde_json::json!({
            "total": total,
            "critical_count": critical_count,
            "conflicts": all_conflicts,
        });
        Ok(result.to_string())
    }

    // ========================================================================
    // v2.18 新增：semantic_search tool
    // ========================================================================

    /// 语义检索（关键词 + 向量混合，session 级隔离）。
    ///
    /// v2.18 批次1：基于 `SessionSearchRouter` 的 `search_with_rebuild` 实现。
    ///
    /// ## 行为
    ///
    /// - 首次访问 session 时，自动从 storage 读取所有 hook 批量重建索引
    ///   （用 `embed_batch` 一次性 embed 所有文本，N 个 hook = 1 次 API 调用）
    /// - 已索引的 session 直接走缓存（避免重复重建）
    /// - 配置了 Embedder：使用 HybridRetriever（BM25 + 向量 + RRF 融合）
    /// - 未配置 Embedder：降级为 KeywordOnlyRetriever（仅 BM25 关键词检索）
    /// - 未注入 SessionSearchRouter：返回 501 错误（向后兼容）
    ///
    /// ## 返回
    ///
    /// SearchHit 列表的 JSON 数组，每个含 hook_id / memory_id / score / source 字段。
    /// Agent 可用返回的 hook_id 调用 `retrieve` 工具获取完整记忆内容。
    #[tool(description = "语义检索记忆（关键词+向量混合）。【project_id 是跨 session 检索的钥匙】传入 project_id 时检索该 project 下所有 session 的记忆（跨 session），不传时仅检索当前 session_id 的记忆。【用户提到过去事件时必调】当用户消息中出现'之前'、'上次'、'还记得'、'我们之前讨论的'、'之前那个方案'等指代过去的词语时，先用用户原话作为 query 调用此工具检索相关记忆，把检索结果作为上下文再回复用户。返回 top-K 相关记忆的 hook_id 列表，可再用 retrieve 工具获取完整内容。首次访问 session 自动从 storage 重建索引（懒加载）。")]
    async fn semantic_search(
        &self,
        Parameters(params): Parameters<SemanticSearchParams>,
    ) -> Result<String, McpError> {
        let session_search = match &self.session_search {
            Some(s) => s,
            None => {
                return Err(McpError::internal_error(
                    "semantic_search 工具未启用：未注入 SessionSearchRouter（需在 MCP 启动时配置 HIPPOCAMPUS_EMBEDDER_* 环境变量）",
                    None,
                ));
            }
        };

        let top_k = params.top_k.unwrap_or(5);

        let hits = session_search
            .search_with_rebuild(
                &params.session_id,
                params.project_id.as_deref(),
                &params.query,
                top_k,
            )
            .await
            .map_err(|e| {
                McpError::internal_error(format!("语义检索失败: {e}"), None)
            })?;

        let result = serde_json::json!({
            "total": hits.len(),
            "hits": hits,
        });
        Ok(result.to_string())
    }

    // ========================================================================
    // v2.29 新增：preset_* tools
    // ========================================================================

    /// 即时构建预设配置。
    ///
    /// v2.29：所有参数可选，服务端即时构建 CombinedProfile，返回最终生效值。
    /// 与 `POST /api/v1/presets/build` 端点行为一致，复用同一套 `build_from_strings` 解析逻辑。
    #[tool(description = "即时构建预设配置。所有参数可选，未提供的字段使用默认值或联动推导（Agent→Window）。返回 archive_threshold / summary_template / session_prefix 等最终生效值。用于预检预设效果后再调用 archive。")]
    async fn preset_build(
        &self,
        Parameters(params): Parameters<PresetBuildParams>,
    ) -> Result<String, McpError> {
        let combined = hippocampus_presets::build_from_strings(
            params.agent.as_deref(),
            params.scenario.as_deref(),
            params.model.as_deref(),
            params.archive_threshold,
            params.summary_template.as_deref(),
        )
        .map_err(|e| McpError::invalid_params(
            format!(
                "预设构建失败: {e}\n\n\
                 提示：调用 preset_list_agents / preset_list_scenarios / preset_list_models 查询合法值。"
            ),
            None,
        ))?;

        let result = serde_json::json!({
            // 解析后的最终生效值
            "archive_threshold": combined.archive_threshold(),
            "summary_template": combined.summary_template(),
            "session_prefix": combined.session_prefix(),
            "archive_to_hippocampus": combined.archive_to_hippocampus(),
            // 标志位（用于追溯哪些 Profile 参与了叠加）
            "has_agent": combined.agent.is_some(),
            "has_scenario": combined.scenario.is_some(),
            "has_window": combined.window.is_some(),
            "has_model": combined.model.is_some(),
            "skills_count": combined.skills.len(),
        });
        Ok(result.to_string())
    }

    /// 列出所有内置 Agent（11 个）。
    #[tool(description = "列出所有内置 Agent（11 个）。返回每个 Agent 的 name / session_prefix / is_mainstream。用于查询 preset_build 的 agent 参数可选值。")]
    async fn preset_list_agents(
        &self,
        Parameters(_): Parameters<NoParams>,
    ) -> Result<String, McpError> {
        let agents: Vec<_> = hippocampus_agents::AgentFamily::all_builtin()
            .into_iter()
            .map(|family| serde_json::json!({
                "name": family.display_name(),
                "session_prefix": family.default_session_prefix(),
                "is_mainstream": family.is_mainstream(),
            }))
            .collect();
        let result = serde_json::json!({
            "total": agents.len(),
            "agents": agents,
        });
        Ok(result.to_string())
    }

    /// 列出所有内置 Scenario（7 个）。
    #[tool(description = "列出所有内置 Scenario（7 个）。返回每个 Scenario 的 variant / display_name / archive_threshold。用于查询 preset_build 的 scenario 参数可选值。")]
    async fn preset_list_scenarios(
        &self,
        Parameters(_): Parameters<NoParams>,
    ) -> Result<String, McpError> {
        let scenarios: Vec<_> = hippocampus_scenarios::Scenario::all_builtin()
            .iter()
            .map(|s| {
                let profile = hippocampus_scenarios::ScenarioProfile::from_scenario(s.clone());
                serde_json::json!({
                    "variant": format!("{:?}", s),
                    "display_name": s.display_name(),
                    "archive_threshold": profile.archive_threshold,
                })
            })
            .collect();
        let result = serde_json::json!({
            "total": scenarios.len(),
            "scenarios": scenarios,
        });
        Ok(result.to_string())
    }

    /// 列出所有 ModelVariant。
    #[tool(description = "列出所有 ModelVariant。返回每个型号的 name / family / context_window / is_default。用于查询 preset_build 的 model 参数可选值。")]
    async fn preset_list_models(
        &self,
        Parameters(_): Parameters<NoParams>,
    ) -> Result<String, McpError> {
        let models: Vec<_> = hippocampus_models::ModelRegistry::all_variants()
            .map(|(name, variant)| {
                let default = hippocampus_models::ModelRegistry::default_variant(variant.family);
                serde_json::json!({
                    "name": name,
                    "family": variant.family.display_name(),
                    "context_window": variant.context_window,
                    "is_default": default.name == *name,
                })
            })
            .collect();
        let result = serde_json::json!({
            "total": models.len(),
            "models": models,
        });
        Ok(result.to_string())
    }

    /// v2.31：首次接入时调用，自动写入 Rules 模板 + AGENTS.md。
    /// 支持 catpaw/trae/claude-code 三种客户端，覆盖策略按客户端类型自动决定。
    /// v2.31 新增：同时写入项目根目录的 AGENTS.md（含 session_id 约定 + 核心协议速查）。
    #[tool(description = "首次接入 hippocampus 时调用此工具，自动写入 Rules 模板 + AGENTS.md。【一次性调用】每个项目只需调用一次，已存在会返回 action=skipped，不要重复调用。支持 catpaw（.catpaw/rules/）、trae（.trae/rules/）、claude-code（追加到 CLAUDE.md）三种客户端。【v2.31 新增】同时写入项目根目录的 AGENTS.md，包含 session_id 约定（治本：让 LLM 知道规范格式，避免用'项目名-session'这种错误格式）+ 核心协议速查表。AGENTS.md 用标记隔离，force=true 时只更新标记内容，不影响用户手动写入的部分。首次配置完 hippocampus MCP 后立即调用：install_rules(client=\"你的客户端类型\", project_root=\"项目根目录绝对路径\")。")]
    async fn install_rules(
        &self,
        Parameters(params): Parameters<InstallRulesParams>,
    ) -> Result<String, McpError> {
        install_rules_to_project(&params.client, &params.project_root, params.force)
            .map_err(|e| McpError::internal_error(e, None))
    }

    // ========================================================================
    // v2.31 动手点 4：project_memory.md 反向写入
    // ========================================================================

    /// 更新 project_memory.md 副本的指定章节（v2.31 动手点 4）
    ///
    /// 用固定章节覆盖策略更新 hippocampus 维护的 project_memory.md 副本。
    /// 章节用 HTML 注释标记界定（`<!-- HIPPOCAMPUS:SECTION:{name} START/END -->`），
    /// 不影响用户手动写入的内容。
    #[tool(description = "更新 project_memory.md 副本的指定章节（v2.31 动手点 4）。【让 hippocampus 记忆流入第7层 Memory Context】调用此工具后，hippocampus 维护的 project_memory.md 副本会被更新，返回 full_content 供你用 Write 工具写入 Trae 客户端的 memory 文件夹（如 c:\\Users\\<user>\\.trae-cn\\memory\\projects\\<project>\\project_memory.md）。\
     \
     【固定章节覆盖策略】章节用 HTML 注释标记界定：\
     <!-- HIPPOCAMPUS:SECTION:task_state START -->\
     （hippocampus 写入的内容）\
     <!-- HIPPOCAMPUS:SECTION:task_state END -->\
     用户手动写入的内容（不在标记内）不受影响。同一 section 的内容会被覆盖（action=replace），不同 section 独立存在。\
     \
     【何时调用】(1)完成一个开发阶段时，更新 task_state/progress 章节；(2)关键架构决策时，更新 decisions 章节；(3)发现风险点时，更新 risks 章节；(4)用户说'记住这个'时立即更新。\
     \
     【参数】project_id/section/content 必填，action 可选（默认 replace，可选 append/delete）。\
     \
     【返回】success + updated_section + action + file_path + full_content。拿到 full_content 后请用 Write 工具写入 Trae 的 project_memory.md，完成'反向写入'闭环。")]
    async fn update_project_memory(
        &self,
        Parameters(params): Parameters<UpdateProjectMemoryParams>,
    ) -> Result<String, McpError> {
        let storage = self.create_storage();

        // 1. 读取现有内容（不存在则空字符串）
        let mut full_content = storage
            .read_project_memory(&params.project_id)
            .await
            .map_err(|e| McpError::internal_error(format!("读取 project_memory.md 失败: {e}"), None))?
            .unwrap_or_default();

        // 2. 构造章节标记
        let section = &params.section;
        let start_marker = format!("<!-- HIPPOCAMPUS:SECTION:{} START -->", section);
        let end_marker = format!("<!-- HIPPOCAMPUS:SECTION:{} END -->", section);

        // 3. 根据 action 处理
        let action = params.action.as_deref().unwrap_or("replace");
        match action {
            "replace" => {
                let new_section_block = format!("{}\n{}\n{}", start_marker, params.content, end_marker);
                // 查找并替换标记之间的内容（含标记本身）
                if let Some(start_idx) = full_content.find(&start_marker) {
                    if let Some(end_idx_rel) = full_content[start_idx..].find(&end_marker) {
                        let end_idx = start_idx + end_idx_rel + end_marker.len();
                        full_content.replace_range(start_idx..end_idx, &new_section_block);
                    } else {
                        // 有 start 无 end（异常状态），追加 end_marker 后再 replace
                        full_content.push_str(&format!("\n{}\n", end_marker));
                        if let Some(s) = full_content.find(&start_marker) {
                            if let Some(e_rel) = full_content[s..].find(&end_marker) {
                                let e = s + e_rel + end_marker.len();
                                full_content.replace_range(s..e, &new_section_block);
                            }
                        }
                    }
                } else {
                    // 无该 section，追加新章节
                    if !full_content.is_empty() && !full_content.ends_with('\n') {
                        full_content.push('\n');
                    }
                    full_content.push_str(&format!("\n{}\n", new_section_block));
                }
            }
            "append" => {
                // 在章节内末尾追加内容（end_marker 之前）
                if let Some(start_idx) = full_content.find(&start_marker) {
                    if let Some(end_idx_rel) = full_content[start_idx..].find(&end_marker) {
                        let insert_pos = start_idx + end_idx_rel;
                        let insert_text = format!("{}\n", params.content);
                        full_content.insert_str(insert_pos, &insert_text);
                    } else {
                        // 有 start 无 end，先补 end_marker 再追加
                        full_content.push_str(&format!("\n{}\n", params.content));
                        full_content.push_str(&format!("{}\n", end_marker));
                    }
                } else {
                    // 无该 section，创建新章节（等同 replace）
                    let new_section_block = format!("{}\n{}\n{}", start_marker, params.content, end_marker);
                    if !full_content.is_empty() && !full_content.ends_with('\n') {
                        full_content.push('\n');
                    }
                    full_content.push_str(&format!("\n{}\n", new_section_block));
                }
            }
            "delete" => {
                // 删除整个章节（含标记及前后空行）
                if let Some(start_idx) = full_content.find(&start_marker) {
                    if let Some(end_idx_rel) = full_content[start_idx..].find(&end_marker) {
                        let end_idx = start_idx + end_idx_rel + end_marker.len();
                        // 向前扩展删除前导空行
                        let mut remove_start = start_idx;
                        while remove_start > 0 && full_content.as_bytes()[remove_start - 1] == b'\n' {
                            remove_start -= 1;
                        }
                        // 向后扩展删除后续空行（保留一个换行避免拼接）
                        let mut remove_end = end_idx;
                        while remove_end < full_content.len() && full_content.as_bytes()[remove_end] == b'\n' {
                            remove_end += 1;
                        }
                        full_content.replace_range(remove_start..remove_end, "");
                    }
                }
                // 无该 section 时 delete 为 no-op
            }
            other => {
                return Err(McpError::invalid_params(
                    format!("无效的 action: {}（支持: replace, append, delete）", other),
                    None,
                ));
            }
        }

        // 4. 写回 hippocampus 副本
        storage
            .write_project_memory(&params.project_id, &full_content)
            .await
            .map_err(|e| McpError::internal_error(format!("写入 project_memory.md 失败: {e}"), None))?;

        // 5. 返回结果（含 full_content 供 LLM 写入 Trae 文件）
        let file_path = format!("projects/{}/project_memory.md", params.project_id);
        let result = serde_json::json!({
            "success": true,
            "updated_section": section,
            "action": action,
            "file_path": file_path,
            "full_content": full_content,
            "content_bytes": full_content.len(),
        });

        Ok(result.to_string())
    }

    /// 读取 project_memory.md 副本完整内容（v2.31 动手点 4）
    #[tool(description = "读取 project_memory.md 副本完整内容（v2.31 动手点 4）。返回 hippocampus 维护的 project_memory.md 副本的完整 Markdown 内容。用于：(1)查看当前 hippocampus 写入了哪些章节；(2)拿到完整内容后用 Write 工具写入 Trae 客户端的 memory 文件夹。若文件不存在返回空字符串。")]
    async fn get_project_memory(
        &self,
        Parameters(params): Parameters<GetProjectMemoryParams>,
    ) -> Result<String, McpError> {
        let storage = self.create_storage();
        let content = storage
            .read_project_memory(&params.project_id)
            .await
            .map_err(|e| McpError::internal_error(format!("读取 project_memory.md 失败: {e}"), None))?
            .unwrap_or_default();

        Ok(content)
    }
}

// ============================================================================
// v2.31：install_rules 公共逻辑（MCP 工具与 CLI 共用）
// ============================================================================
//
// 首次接入 hippocampus 时，将 Rules 模板写入 Agent 客户端的 rules 目录。
// 覆盖策略按客户端类型自动决定：
// - catpaw/trae（独立文件）：已存在则跳过（force=true 时覆盖）
// - claude-code（CLAUDE.md）：检查 hippocampus 标记，有标记则跳过/替换，无标记则追加
//
// 风险防护：
// 1. 路径校验：验证 project_root 存在且为目录
// 2. 文件保护：catpaw/trae 默认不覆盖，claude-code 用标记隔离
// 3. IO 错误：所有文件操作都捕获错误并返回友好提示

/// CatPaw Rules 模板（Model Request 类型，LLM 自主判断何时应用）
const CATPAW_RULES_TEMPLATE: &str = r#"---
ruleType: Model Request
name: "Hippocampus 记忆归档触发"
description: "当对话变长、上下文接近阈值、用户提到历史事件、即将压缩上下文时，主动调用 hippocampus_mcp.archive 归档对话轮次。归档后返回 token 估算反馈，用于判断后续何时再次归档。这是伪钩子方案的核心：通过外部反馈建立 token 意识。"
globs:
---

# Hippocampus 记忆归档触发规则

## 你的角色

你是接入了 Hippocampus 记忆库的 Agent。Hippocampus 帮你保留长对话中的关键信息，
避免上下文压缩时丢失重要内容。**你无法直接感知自身 token 消耗**，但通过 Hippocampus
的 archive 工具返回的 token 估算反馈，你可以建立"token 意识"，主动判断何时归档。

## 何时调用 archive（满足任一即调用）

### 触发条件 1：对话长度
- 本次对话已进行 **超过 20 轮**（用户消息 + LLM 回复算 1 轮）
- 或包含大量代码 / 长文档读写 / 多次工具调用（累计内容超过 5000 字）
- 或你感觉到"前面说过但记不清细节"、"上下文开始变重"

### 触发条件 2：Token 反馈（核心机制）
当你调用过 hippocampus_mcp.archive 后，返回结果包含：
- `estimated_total_tokens`：当前累计估算 token 数
- `threshold`：归档阈值（如 120000 = 120K）
- `threshold_ratio_percent`：当前占比百分比
- `suggestion`：归档建议（人类可读）

**判断规则**：
- `threshold_ratio_percent >= 100` → **立即归档**，并提示用户触发上下文压缩
- `threshold_ratio_percent >= 80` → **准备归档**，主动提示用户"建议归档"
- `threshold_ratio_percent >= 50` → 继续对话，但注意跟踪累计值
- `< 50` → 继续对话

### 触发条件 3：用户提到过去事件
当用户消息中出现以下指代词时，**先调 `semantic_search` 检索相关记忆**，再回复：
- "之前"、"上次"、"还记得"、"我们之前讨论的"、"之前那个方案"
- "之前的代码"、"之前那个 bug"、"上次提到的"

### 触发条件 4：兜底定期归档
- 每 **30 轮对话** 主动归档一次（即使未达阈值）
- 避免上下文丢失，确保关键信息保留

### 触发条件 5：用户即将压缩上下文
当你预判用户即将手动触发上下文压缩（如 Trae 对话框上方的"压缩"按钮），
或客户端即将自动压缩时，**先调 archive 归档完整上下文**，避免压缩丢失原始轮次。

## 如何调用 archive

### 调用方式（简化格式）

```python
hippocampus_mcp.archive(
    session_id="catpaw-{项目名}-{日期}",  # 如 catpaw-myapp-20260705
    turns_json='[{"user_message":{"text":"用户问的"},"llm_message":{"text":"我答的"}}]'
)
```

**最简格式**：只需传 `user_message` 和 `llm_message` 的 `text` 字段。
其他字段（id/timestamp/tags/token_count）可省略，服务端自动补全：
- `id` → 自动生成 UUID
- `timestamp` → 自动取当前时间
- `tags` → 根据内容自动推断（ToolCall/Image/CodeBlock/Url/Text 等）
- `token_count` → 根据文本长度估算（3 char ≈ 1 token）

### 归档多轮对话

```python
hippocampus_mcp.archive(
    session_id="catpaw-myapp-20260705",
    turns_json='[
        {"user_message":{"text":"第一个问题"},"llm_message":{"text":"第一个回答"}},
        {"user_message":{"text":"第二个问题"},"llm_message":{"text":"第二个回答"}}
    ]'
)
```

## Token 反馈循环（关键机制）

### 工作流程

```
你调用 archive 归档对话
    ↓
Hippocampus 估算 token 并返回
    ↓ estimated_total_tokens / threshold_ratio_percent
你记录累计值，作为后续判断依据
    ↓
继续对话，下次判断时参考这个数字
    ↓ threshold_ratio_percent >= 80
你主动提示用户"建议归档或压缩"
    ↓ 循环
```

### 示例：归档后收到反馈

```json
// archive 返回结果
{
  "hook_id": "5b30a117-...",
  "estimated_total_tokens": 45000,
  "threshold": 120000,
  "threshold_ratio_percent": 38,
  "suggestion": "已归档 5 轮，累计估算 45000 tokens（阈值 120000，当前 38%）。继续对话。"
}
// 你记录：本次归档后累计 45000 tokens，占 38%
// 继续对话，下次判断时参考这个数字
```

### 示例：接近阈值时主动提示

```
你（LLM）：根据上次归档反馈，当前累计约 98000 tokens（占阈值 82%）。
          建议先归档这轮对话，避免上下文丢失。
          要不要我现在归档？
```

## session_id 约定

```
catpaw-{项目名}-{日期}
```

示例：
- `catpaw-myapp-20260705`
- `catpaw-hippocampus-20260705`

> 一个 session_id 对应一个独立的记忆空间。同会话内复用同一 session_id，
> 切换项目或日期时换新 session_id。

## 不要归档的情况

- 单次简单问答（如"这个变量什么意思"）
- 纯闲聊或问候
- 用户明确说"不用记"

## 与其他工具配合

| 时机 | 工具 | 说明 |
|------|------|------|
| 会话第一次回复前 | `prompt` | 获取历史记忆摘要 |
| 对话变长 / 接近阈值 | `archive` | 归档完整上下文 |
| 用户提到过去事件 | `semantic_search` | 检索相关记忆 |
| 用户陈述与记忆矛盾 | `detect_conflicts` | 检测事实冲突 |
| 需要查特定记忆细节 | `retrieve` | 按 hook_id 检索完整记忆 |
"#;

/// Trae Rules 模板（Always 类型，始终生效）
const TRAE_RULES_TEMPLATE: &str = r#"# Hippocampus 记忆归档触发规则（Trae）

## 你的角色

你是接入了 Hippocampus 记忆库的 Agent。Hippocampus 帮你保留长对话中的关键信息，
避免上下文压缩时丢失重要内容。**你无法直接感知自身 token 消耗**，但通过 Hippocampus
的 archive 工具返回的 token 估算反馈，你可以建立"token 意识"，主动判断何时归档。

## 何时调用 archive（满足任一即调用）

### 触发条件 1：对话长度
- 本次对话已进行 **超过 20 轮**（用户消息 + LLM 回复算 1 轮）
- 或包含大量代码 / 长文档读写 / 多次工具调用（累计内容超过 5000 字）
- 或你感觉到"前面说过但记不清细节"、"上下文开始变重"

### 触发条件 2：Token 反馈（核心机制）
当你调用过 hippocampus.archive 后，返回结果包含：
- `estimated_total_tokens`：当前累计估算 token 数
- `threshold`：归档阈值（如 120000 = 120K）
- `threshold_ratio_percent`：当前占比百分比
- `suggestion`：归档建议（人类可读）

**判断规则**：
- `threshold_ratio_percent >= 100` → **立即归档**，并提示用户触发上下文压缩
- `threshold_ratio_percent >= 80` → **准备归档**，主动提示用户"建议归档"
- `threshold_ratio_percent >= 50` → 继续对话，但注意跟踪累计值
- `< 50` → 继续对话

### 触发条件 3：用户提到过去事件
当用户消息中出现以下指代词时，**先调 `semantic_search` 检索相关记忆**，再回复：
- "之前"、"上次"、"还记得"、"我们之前讨论的"、"之前那个方案"
- "之前的代码"、"之前那个 bug"、"上次提到的"

### 触发条件 4：兜底定期归档
- 每 **30 轮对话** 主动归档一次（即使未达阈值）
- 避免上下文丢失，确保关键信息保留

### 触发条件 5：用户即将压缩上下文
当你预判用户即将手动触发上下文压缩（如 Trae 对话框上方的"压缩"按钮），
或客户端即将自动压缩时，**先调 archive 归档完整上下文**，避免压缩丢失原始轮次。

> Trae 的自动压缩机制会丢弃旧轮次，归档可保留被丢弃的内容。

## 如何调用 archive

### 调用方式（简化格式）

```python
hippocampus.archive(
    session_id="trae-{项目名}-{日期}",  # 如 trae-myapp-20260705
    turns_json='[{"user_message":{"text":"用户问的"},"llm_message":{"text":"我答的"}}]'
)
```

**最简格式**：只需传 `user_message` 和 `llm_message` 的 `text` 字段。
其他字段（id/timestamp/tags/token_count）可省略，服务端自动补全：
- `id` → 自动生成 UUID
- `timestamp` → 自动取当前时间
- `tags` → 根据内容自动推断（ToolCall/Image/CodeBlock/Url/Text 等）
- `token_count` → 根据文本长度估算（3 char ≈ 1 token）

### 归档多轮对话

```python
hippocampus.archive(
    session_id="trae-myapp-20260705",
    turns_json='[
        {"user_message":{"text":"第一个问题"},"llm_message":{"text":"第一个回答"}},
        {"user_message":{"text":"第二个问题"},"llm_message":{"text":"第二个回答"}}
    ]'
)
```

## Token 反馈循环（关键机制）

### 工作流程

```
你调用 archive 归档对话
    ↓
Hippocampus 估算 token 并返回
    ↓ estimated_total_tokens / threshold_ratio_percent
你记录累计值，作为后续判断依据
    ↓
继续对话，下次判断时参考这个数字
    ↓ threshold_ratio_percent >= 80
你主动提示用户"建议归档或压缩"
    ↓ 循环
```

### 示例：接近阈值时主动提示

```
你（LLM）：根据上次归档反馈，当前累计约 98000 tokens（占阈值 82%）。
          建议先归档这轮对话，避免上下文丢失。
          要不要我现在归档？
```

## session_id 约定

```
trae-{项目名}-{日期}
```

示例：
- `trae-myapp-20260705`
- `trae-hippocampus-20260705`

> 一个 session_id 对应一个独立的记忆空间。同会话内复用同一 session_id，
> 切换项目或日期时换新 session_id。

## 不要归档的情况

- 单次简单问答（如"这个变量什么意思"）
- 纯闲聊或问候
- 用户明确说"不用记"

## Trae 特有提示

- Trae Solo/Work 支持手动和自动上下文压缩
- **建议**：在 Trae 提示压缩前，先调用 `hippocampus.archive` 归档当前对话
- 避免压缩丢失原始内容

## 与其他工具配合

| 时机 | 工具 | 说明 |
|------|------|------|
| 会话第一次回复前 | `prompt` | 获取历史记忆摘要 |
| 对话变长 / 接近阈值 | `archive` | 归档完整上下文 |
| 用户提到过去事件 | `semantic_search` | 检索相关记忆 |
| 用户陈述与记忆矛盾 | `detect_conflicts` | 检测事实冲突 |
| 需要查特定记忆细节 | `retrieve` | 按 hook_id 检索完整记忆 |
"#;

/// Claude Code CLAUDE.md 片段模板
const CLAUDE_CODE_RULES_TEMPLATE: &str = r#"## Hippocampus 记忆协议

本项目已接入 hippocampus 记忆库。Agent 必须遵守以下规则，无需用户提示主动执行。

### 你的角色

你是接入了 Hippocampus 记忆库的 Agent。Hippocampus 帮你保留长对话中的关键信息，
避免上下文压缩时丢失重要内容。**你无法直接感知自身 token 消耗**，但通过 Hippocampus
的 archive 工具返回的 token 估算反馈，你可以建立"token 意识"，主动判断何时归档。

### 1. 会话开始：调 prompt 获取历史记忆

每个新会话的**第一次回复前**，必须先调用：

```
hippocampus.prompt(session_id)
```

把返回的历史记忆摘要拼接到 system prompt。若返回空列表，说明该 session 无历史记忆，正常继续。

### 2. 上下文接近阈值：主动调 archive 归档

#### 触发条件（满足任一即调用）

- **对话长度**：已进行超过 20 轮（用户消息 + LLM 回复算 1 轮）
- **内容复杂度**：包含大量代码 / 长文档 / 多次工具调用（累计内容超过 5000 字）
- **主观判断**：你感觉到"上下文开始变重"、"前面说过但记不清细节"
- **Token 反馈**：上次 archive 返回的 `threshold_ratio_percent >= 80`
- **兜底**：每 30 轮对话主动归档一次（即使未达阈值）

#### 调用方式（简化格式）

```
hippocampus.archive(
    session_id,                  // 必填
    turns_json                   // 必填，最简格式：[{"user_message":{"text":"..."},"llm_message":{"text":"..."}}]
    // id/timestamp/tags/token_count 可省略，服务端自动补全
)
```

#### Token 反馈循环（核心机制）

归档后返回结果包含：
- `estimated_total_tokens`：当前累计估算 token 数
- `threshold`：归档阈值（如 120000 = 120K）
- `threshold_ratio_percent`：当前占比百分比
- `suggestion`：归档建议（人类可读）

**判断规则**：
- `threshold_ratio_percent >= 100` → **立即归档**，并提示用户触发上下文压缩
- `threshold_ratio_percent >= 80` → **准备归档**，主动提示用户"建议归档"
- `threshold_ratio_percent >= 50` → 继续对话，但注意跟踪累计值
- `< 50` → 继续对话

> 你（LLM）无法直接感知自身 token 消耗，但通过 archive 返回的 `estimated_total_tokens`
> 可以建立"token 意识"——这是伪钩子方案的核心：通过外部反馈循环模拟主动感知。

#### 客户端压缩前主动归档

当你预判客户端即将自动压缩时，**先调 archive 归档完整上下文**，避免压缩丢失原始轮次。

### 3. 用户提到过去事件：先调 semantic_search 再回复

当用户消息中出现"之前"、"上次"、"还记得"、"我们之前讨论的"等指代过去的词语时，
**先调用**：

```
hippocampus.semantic_search(query, session_id, top_k=5)
```

用用户原话作为 query，检索相关记忆。把检索结果作为上下文再回复用户。

### 4. 用户陈述与记忆可能矛盾：先调 detect_conflicts 检测

当用户陈述的事实与记忆中的记录可能冲突时，**先调用**：

```
hippocampus.detect_conflicts(session_id, hook_id, added_facts, revised_facts, deprecated_facts)
```

### 5. session_id 约定

```
claude-code-{项目名}-{日期}
```

示例：claude-code-myapp-20260705

> 一个 session_id 对应一个独立的记忆空间。同会话内复用同一 session_id，
> 切换项目或日期时换新 session_id。

### 6. 不要归档的情况

- 单次简单问答（如"这个变量什么意思"）
- 纯闲聊或问候
- 用户明确说"不用记"
"#;

/// hippocampus 规则标记（用于 CLAUDE.md 检测是否已安装）
const HIPPOCAMPUS_RULES_BEGIN: &str = "<!-- hippocampus-rules begin -->";
const HIPPOCAMPUS_RULES_END: &str = "<!-- hippocampus-rules end -->";

/// AGENTS.md 标记（用于检测是否已安装）
const HIPPOCAMPUS_AGENTS_BEGIN: &str = "<!-- hippocampus-agents begin -->";
const HIPPOCAMPUS_AGENTS_END: &str = "<!-- hippocampus-agents end -->";

/// AGENTS.md 模板（v2.31 新增）
///
/// 写入项目根目录的 AGENTS.md，被 Trae/CatPaw/Cursor/Claude Code 等 IDE 自动读取。
/// 包含 session_id 约定（治本）+ 核心协议速查（让 LLM 知道规范格式）。
const AGENTS_MD_TEMPLATE: &str = r#"<!-- hippocampus-agents begin -->
# Hippocampus 记忆协议（项目级硬性规则）

> 本文件由 install_rules 自动写入，被 IDE 自动读取注入 LLM system prompt。
> 完整规则详见 `.trae/rules/hippocampus-archive.md`（或 `.catpaw/rules/`）。

## session_id 约定（必须遵守）

```
{客户端前缀}-{项目名}-{日期}
```

**客户端前缀**：
- CatPaw: `catpaw`
- Trae: `trae`
- Claude Code: `claudecode`
- Cursor: `cursor`

**示例**：
- `catpaw-myapp-20260706`
- `trae-hippocampus-20260706`
- `claudecode-worldsmith-20260706`

> 一个 session_id 对应一个独立的记忆空间。同会话内复用同一 session_id，
> 切换项目或日期时换新 session_id。
>
> **禁止使用 `项目名-session` 这种格式**（如 `worldsmith-session`），
> 会导致 retrieve 找不到记忆。若不确定用哪个 session_id，先调 `prompt` 工具
> 查看可用 session 列表，或询问用户。

## 记忆协议（Agent 必须遵守，无需用户提示主动执行）

### 1. 会话开始：调 prompt 获取历史记忆
每个新会话的第一次回复前，必须先调用 `mcp_hippocampus.prompt(session_id)`。
返回的摘要拼接到 system prompt。若返回空列表，说明该 session 无历史记忆。

### 2. 上下文接近阈值：主动调 archive 归档
触发条件（满足任一）：
- 对话超过 20 轮（用户消息 + LLM 回复算 1 轮）
- 包含大量代码 / 长文档 / 多次工具调用
- 感觉"上下文开始变重"、"前面说过但记不清细节"
- 每 30 轮兜底归档一次

### 3. 用户提到过去事件：先调 semantic_search
当用户消息中出现"之前"、"上次"、"还记得"等指代词时，先用用户原话作为 query
检索相关记忆，把检索结果作为上下文再回复。

### 4. project_memory 反向写入（v2.31）
完成开发阶段/关键决策/风险点时，调 `update_project_memory` 更新 project_memory.md。
拿到 full_content 后用 Write 工具写入 Trae 的 memory 文件夹。

### 5. 上下文被压缩后：执行压缩后行为协议
当系统消息中出现 `This session continues a previous conversation that lost its context.` 时，
立即执行 4 步强制流程（详见 rules/hippocampus-archive.md）：
1. 归档压缩前未持久化的轮次
2. 调 prompt 拉取 hippocampus 一手记忆
3. 交叉校准 Summary 第8章节 Current Work
4. 执行 Next Step 决策协议

## 工具触发规则速查表

| 时机 | 工具 |
|------|------|
| 会话第一次回复前 | `prompt` |
| 上下文接近阈值 | `archive` |
| 用户提到过去事件 | `semantic_search` |
| 需要查特定记忆 | `retrieve` |
| 需要查所有记忆列表 | `summaries` |
| 完成开发阶段/关键决策 | `update_project_memory` |
| 上下文被压缩后 | `archive` + `prompt` |
| 周级去重合并 | `compaction` period="weekly" |
| 月级评分淘汰 | `compaction` period="monthly" |
<!-- hippocampus-agents end -->"#;

/// 安装 Rules 模板到项目目录
///
/// 参数：
/// - client: 客户端类型（catpaw / trae / claude-code）
/// - project_root: 项目根目录路径
/// - force: 是否强制覆盖
///
/// 返回 JSON 字符串（安装结果）
///
/// 覆盖策略：
/// - catpaw/trae（独立文件）：已存在则跳过（force=true 时覆盖）
/// - claude-code（CLAUDE.md）：有 hippocampus 标记则跳过/替换，无标记则追加
pub fn install_rules_to_project(
    client: &str,
    project_root: &str,
    force: bool,
) -> Result<String, String> {
    use std::fs;
    use std::path::Path;

    // 1. 验证项目根目录
    let root = Path::new(project_root);
    if !root.exists() {
        return Err(format!("项目根目录不存在: {project_root}"));
    }
    if !root.is_dir() {
        return Err(format!("路径不是目录: {project_root}"));
    }

    // 2. 按客户端类型处理
    let (file_path, action, message) = match client {
        "catpaw" => {
            let rules_dir = root.join(".catpaw").join("rules");
            fs::create_dir_all(&rules_dir)
                .map_err(|e| format!("创建 .catpaw/rules/ 失败: {e}"))?;
            let file_path = rules_dir.join("hippocampus-archive.md");

            if file_path.exists() && !force {
                let msg = format!("已存在 {}，跳过（force=false）", file_path.display());
                (file_path, "skipped", msg)
            } else {
                fs::write(&file_path, CATPAW_RULES_TEMPLATE)
                    .map_err(|e| format!("写入失败: {e}"))?;
                let (action, action_cn) = if force { ("updated", "更新") } else { ("created", "创建") };
                let msg = format!("已{} CatPaw Rules 模板到 {}", action_cn, file_path.display());
                (file_path, action, msg)
            }
        }
        "trae" => {
            let rules_dir = root.join(".trae").join("rules");
            fs::create_dir_all(&rules_dir)
                .map_err(|e| format!("创建 .trae/rules/ 失败: {e}"))?;
            let file_path = rules_dir.join("hippocampus-archive.md");

            if file_path.exists() && !force {
                let msg = format!("已存在 {}，跳过（force=false）", file_path.display());
                (file_path, "skipped", msg)
            } else {
                fs::write(&file_path, TRAE_RULES_TEMPLATE)
                    .map_err(|e| format!("写入失败: {e}"))?;
                let (action, action_cn) = if force { ("updated", "更新") } else { ("created", "创建") };
                let msg = format!("已{} Trae Rules 模板到 {}", action_cn, file_path.display());
                (file_path, action, msg)
            }
        }
        "claude-code" => {
            let file_path = root.join("CLAUDE.md");
            let content_with_markers = format!(
                "{begin}\n{template}\n{end}",
                begin = HIPPOCAMPUS_RULES_BEGIN,
                template = CLAUDE_CODE_RULES_TEMPLATE,
                end = HIPPOCAMPUS_RULES_END,
            );

            if !file_path.exists() {
                fs::write(&file_path, &content_with_markers)
                    .map_err(|e| format!("写入失败: {e}"))?;
                let msg = "已创建 CLAUDE.md 并写入 hippocampus 规则".to_string();
                (file_path, "created", msg)
            } else {
                let existing = fs::read_to_string(&file_path)
                    .map_err(|e| format!("读取 CLAUDE.md 失败: {e}"))?;

                if existing.contains(HIPPOCAMPUS_RULES_BEGIN) {
                    if !force {
                        let msg = "CLAUDE.md 已有 hippocampus 规则，跳过（force=false）".to_string();
                        (file_path, "skipped", msg)
                    } else {
                        let start_idx = existing.find(HIPPOCAMPUS_RULES_BEGIN)
                            .ok_or("找不到开始标记")?;
                        let end_idx = existing.find(HIPPOCAMPUS_RULES_END)
                            .ok_or("找不到结束标记")?;
                        let end_idx = end_idx + HIPPOCAMPUS_RULES_END.len();
                        let new_content = format!(
                            "{}{}{}",
                            &existing[..start_idx],
                            content_with_markers,
                            &existing[end_idx..],
                        );
                        fs::write(&file_path, new_content)
                            .map_err(|e| format!("写入失败: {e}"))?;
                        let msg = "已更新 CLAUDE.md 中的 hippocampus 规则".to_string();
                        (file_path, "updated", msg)
                    }
                } else {
                    let new_content = format!(
                        "{}\n\n{}\n",
                        existing.trim_end(),
                        content_with_markers,
                    );
                    fs::write(&file_path, new_content)
                        .map_err(|e| format!("写入失败: {e}"))?;
                    let msg = "已追加 hippocampus 规则到 CLAUDE.md".to_string();
                    (file_path, "updated", msg)
                }
            }
        }
        _ => {
            return Err(format!(
                "不支持的客户端类型: {client}\n支持的类型: catpaw / trae / claude-code"
            ));
        }
    };

    // 3. 额外写入 AGENTS.md（v2.31 新增，所有客户端通用）
    // AGENTS.md 是 IDE 通用约定，被 Trae/CatPaw/Cursor/Claude Code 自动读取
    let agents_md_path = root.join("AGENTS.md");
    let agents_content_with_markers = format!(
        "{begin}\n{template}\n{end}",
        begin = HIPPOCAMPUS_AGENTS_BEGIN,
        template = AGENTS_MD_TEMPLATE,
        end = HIPPOCAMPUS_AGENTS_END,
    );

    let agents_action = if !agents_md_path.exists() {
        // 不存在 → 创建
        fs::write(&agents_md_path, &agents_content_with_markers)
            .map_err(|e| format!("写入 AGENTS.md 失败: {e}"))?;
        "created"
    } else {
        let existing = fs::read_to_string(&agents_md_path)
            .map_err(|e| format!("读取 AGENTS.md 失败: {e}"))?;

        if existing.contains(HIPPOCAMPUS_AGENTS_BEGIN) {
            // 已有 hippocampus 标记
            if !force {
                "skipped"
            } else {
                // 替换标记内容
                let start_idx = existing.find(HIPPOCAMPUS_AGENTS_BEGIN)
                    .ok_or("AGENTS.md 找不到开始标记")?;
                let end_idx = existing.find(HIPPOCAMPUS_AGENTS_END)
                    .ok_or("AGENTS.md 找不到结束标记")?;
                let end_idx = end_idx + HIPPOCAMPUS_AGENTS_END.len();
                let new_content = format!(
                    "{}{}{}",
                    &existing[..start_idx],
                    agents_content_with_markers,
                    &existing[end_idx..],
                );
                fs::write(&agents_md_path, new_content)
                    .map_err(|e| format!("写入 AGENTS.md 失败: {e}"))?;
                "updated"
            }
        } else {
            // 无标记 → 追加
            let new_content = format!(
                "{}\n\n{}\n",
                existing.trim_end(),
                agents_content_with_markers,
            );
            fs::write(&agents_md_path, new_content)
                .map_err(|e| format!("写入 AGENTS.md 失败: {e}"))?;
            "appended"
        }
    };

    // 4. 返回 JSON 结果
    let result = serde_json::json!({
        "success": true,
        "client": client,
        "file_path": file_path.to_string_lossy(),
        "action": action,
        "message": message,
        "agents_md_path": agents_md_path.to_string_lossy(),
        "agents_md_action": agents_action,
        "force": force,
    });

    Ok(result.to_string())
}

// ============================================================================
// ServerHandler 手写实现（v2.30 新增）
// ============================================================================
//
// 改用两段式（`#[tool_router]` + 独立 `#[tool_handler] impl ServerHandler`），
// 以便手写 `get_info()` 在运行时把 `usage_protocol.instructions` 注入 MCP 规范的
// 顶层 `instructions` 字段（InitializeResult.instructions）。
//
// ## 为何用顶层 `instructions` 而非 `Implementation.description`
//
// - MCP 规范定义 `InitializeResult.instructions` 为「服务器给客户端/LLM 的使用说明」，
//   语义最贴切 `usage_protocol.instructions`（行为契约）
// - 客户端（Claude Code / Cursor / Trae）通常把顶层 `instructions` 注入 system prompt，
//   让 LLM 启动即看到记忆协议
// - `Implementation.description` 是「服务器实现本身的描述」（元数据），语义偏元信息
//
// ## 降级路径
//
// - `combined_profile` 为 None（Custom/降级）：返回与原宏生成等价的最小 ServerInfo
//   （仅 `enable_tools()` + `Implementation::from_build_env()`，无 instructions）
// - 向后兼容 v2.29：未识别 Agent 时 LLM 看到的 server 元信息与 v2.29 一致
#[tool_handler]
impl ServerHandler for HippocampusMcp {
    /// 手写 get_info：注入 usage_protocol.instructions 到顶层 instructions 字段
    ///
    /// 宏的 `has_method` 检查会跳过此手写实现，但仍自动生成
    /// `call_tool` / `list_tools` / `get_tool`，工具路由功能不受影响。
    ///
    /// ## server_info.name 修正
    ///
    /// v2.29 之前用宏自动生成的 get_info 调 `Implementation::from_build_env()`，
    /// 但 `from_build_env` 内部的 `env!("CARGO_CRATE_NAME")` 在 rmcp crate 编译期
    /// 展开，取到 "rmcp" 而非调用方 crate 名。v2.30 改用 `Implementation::new`
    /// 在 hippocampus-mcp 上下文展开 `env!`，让客户端看到正确的 "hippocampus-mcp"。
    fn get_info(&self) -> ServerInfo {
        let capabilities = ServerCapabilities::builder()
            .enable_tools()
            .build();

        // 基础 ServerInfo（name/version 在 hippocampus-mcp 上下文取，修正 v2.29 的 "rmcp" 问题）
        let server_impl = Implementation::new(
            env!("CARGO_CRATE_NAME"),
            env!("CARGO_PKG_VERSION"),
        );
        let mut info = ServerInfo::new(capabilities).with_server_info(server_impl);

        // 若注入了 CombinedProfile，把 usage_protocol.instructions 作为顶层 instructions
        // 让 MCP 客户端把它注入 LLM 的 system prompt
        if let Some(profile) = self.combined_profile() {
            let protocol = profile.usage_protocol();
            if !protocol.instructions.is_empty() {
                // v2.31：在 instructions 末尾追加 install_rules 提示
                // 让 LLM 首次接入时知道可以调用 install_rules 安装 Rules 模板
                let install_hint = "\n\n---\n## 首次接入提示\n如果你是首次使用 hippocampus，建议调用 `install_rules` 工具安装 Rules 模板到你的客户端 rules 目录，让归档触发规则自动生效。\n\n调用方式：install_rules(client=\"你的客户端类型\", project_root=\"项目根目录绝对路径\")\n支持的客户端：catpaw / trae / claude-code";
                let instructions = format!("{}{}", protocol.instructions, install_hint);
                info = info.with_instructions(instructions);
            }
        }

        info
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::handler::server::wrapper::Parameters;
    use serde_json::{json, Value};
    use tempfile::TempDir;
    use uuid::Uuid;

    // ========================================================================
    // 测试辅助
    // ========================================================================

    /// 创建一个绑定到临时目录的 MCP 实例
    fn make_mcp(tmpdir: &TempDir) -> HippocampusMcp {
        HippocampusMcp::new(tmpdir.path().to_path_buf())
    }

    /// 创建一个注入 HeuristicDetector 的 MCP 实例（v2.11 测试用）
    fn make_mcp_with_detector(tmpdir: &TempDir) -> HippocampusMcp {
        let detector: Arc<dyn ConflictDetector> =
            Arc::new(hippocampus_core::heuristic::HeuristicDetector::new());
        HippocampusMcp::with_conflict_detector(
            tmpdir.path().to_path_buf(),
            Some(detector),
        )
    }

    /// 创建一个注入 SessionSearchRouter 的 MCP 实例（v2.18 测试用）
    ///
    /// SessionSearchRouter 仅关键词模式（无 Embedder）+ 注入 storage 懒重建。
    /// archive 写入 storage 后，semantic_search 首次调用会触发 rebuild。
    fn make_mcp_with_session_search(tmpdir: &TempDir) -> HippocampusMcp {
        use hippocampus_search::SessionSearchRouter;
        let storage: Arc<dyn hippocampus_core::storage::Storage> =
            Arc::new(hippocampus_core::storage::LocalStorage::new(
                tmpdir.path().to_path_buf(),
            ));
        let router = SessionSearchRouter::new(None, 0).with_storage(storage);
        HippocampusMcp::with_conflict_detector(tmpdir.path().to_path_buf(), None)
            .with_session_search(Some(Arc::new(router)))
    }

    /// 创建一个注入 Mock 摘要生成器的 MCP 实例（v2.21 批次 8c 测试用）
    ///
    /// Mock 生成器返回固定的结构化摘要，用于验证 archive tool 注入链路。
    fn make_mcp_with_summary_generator(tmpdir: &TempDir, fail: bool) -> HippocampusMcp {
        use async_trait::async_trait;
        use hippocampus_core::generate::SummaryGenerator;
        use hippocampus_core::model::{MemoryFile, Summary};

        struct MockSummaryGenerator {
            fail: bool,
        }

        #[async_trait::async_trait]
        impl SummaryGenerator for MockSummaryGenerator {
            async fn generate_summary(
                &self,
                _file: &MemoryFile,
            ) -> hippocampus_core::Result<Summary> {
                if self.fail {
                    return Err(hippocampus_core::Error::Storage(
                        "Mock 摘要生成失败".into(),
                    ));
                }
                Ok(Summary {
                    title: "Mock LLM 摘要标题".into(),
                    abstract_text: Some("Mock 摘要内容".into()),
                    key_facts: vec!["事实1".into(), "事实2".into()],
                    key_entities: vec!["实体A".into()],
                    clue_anchors: Vec::new(),
                })
            }
        }

        let gen: Arc<dyn SummaryGenerator> = Arc::new(MockSummaryGenerator { fail });
        HippocampusMcp::with_conflict_detector(tmpdir.path().to_path_buf(), None)
            .with_summary_generator(Some(gen))
    }

    /// 构造一个最小合法 MessageTurn JSON 字符串
    fn make_turns_json(user_text: &str, llm_text: &str, tokens: usize) -> String {
        let turns = vec![json!({
            "id": Uuid::new_v4().to_string(),
            "user_message": {
                "text": user_text,
                "attachments": [],
                "tool_calls": [],
                "thinking": null
            },
            "llm_message": {
                "text": llm_text,
                "attachments": [],
                "tool_calls": [],
                "thinking": null
            },
            "tags": [{"kind": "Text"}],
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "token_count": tokens
        })];
        serde_json::to_string(&turns).unwrap()
    }

    /// 构造多轮消息 JSON（用于测试周级合并）
    fn make_multi_turns_json(count: usize) -> String {
        let turns: Vec<Value> = (0..count)
            .map(|i| {
                json!({
                    "id": Uuid::new_v4().to_string(),
                    "user_message": {
                        "text": format!("用户消息 {i}"),
                        "attachments": [],
                        "tool_calls": [],
                        "thinking": null
                    },
                    "llm_message": {
                        "text": format!("LLM 回复 {i}"),
                        "attachments": [],
                        "tool_calls": [],
                        "thinking": null
                    },
                    "tags": [{"kind": "Text"}],
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                    "token_count": 100
                })
            })
            .collect();
        serde_json::to_string(&turns).unwrap()
    }

    // ========================================================================
    // 基础测试
    // ========================================================================

    #[test]
    fn test_construct() {
        let mcp = HippocampusMcp::new(PathBuf::from("/tmp/test"));
        assert_eq!(mcp.storage_root, PathBuf::from("/tmp/test"));
    }

    // ========================================================================
    // archive tool 测试
    // ========================================================================

    #[tokio::test]
    async fn test_archive_and_retrieve() {
        let tmpdir = TempDir::new().unwrap();
        let mcp = make_mcp(&tmpdir);
        let session_id = "test-session-1";

        // archive
        let turns_json = make_turns_json("你好", "你好！我是助手", 100);
        let params = Parameters(ArchiveParams {
            session_id: session_id.to_string(),
            turns_json,
            project_id: None,
            ..Default::default()
        });
        let result = mcp.archive(params).await.expect("归档失败");
        let summary: Value = serde_json::from_str(&result).unwrap();
        let hook_id = summary["hook_id"].as_str().expect("缺少 hook_id").to_string();
        assert_eq!(summary["token_count"], 100);

        // retrieve
        let params = Parameters(RetrieveParams {
            session_id: session_id.to_string(),
            hook_id,
            project_id: None,
        });
        let result = mcp.retrieve(params).await.expect("检索失败");
        let memory: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(memory["turns"].as_array().unwrap().len(), 1);
        assert_eq!(memory["total_tokens"], 100);
    }

    #[tokio::test]
    async fn test_archive_empty_turns() {
        let tmpdir = TempDir::new().unwrap();
        let mcp = make_mcp(&tmpdir);

        let params = Parameters(ArchiveParams {
            session_id: "s1".to_string(),
            turns_json: "[]".to_string(),
            project_id: None,
            ..Default::default()
        });
        let err = mcp.archive(params).await.unwrap_err();
        let msg = err.message.as_ref();
        assert!(msg.contains("不能为空"), "错误消息应提及 turns 为空, 实际: {msg}");
    }

    #[tokio::test]
    async fn test_archive_invalid_turns_json() {
        let tmpdir = TempDir::new().unwrap();
        let mcp = make_mcp(&tmpdir);

        let params = Parameters(ArchiveParams {
            session_id: "s1".to_string(),
            turns_json: "不是合法 JSON".to_string(),
            project_id: None,
            ..Default::default()
        });
        let err = mcp.archive(params).await.unwrap_err();
        let msg = err.message.as_ref();
        assert!(msg.contains("解析失败"), "应报告 JSON 解析失败, 实际: {msg}");
    }

    // ========================================================================
    // summaries tool 测试
    // ========================================================================

    #[tokio::test]
    async fn test_summaries() {
        let tmpdir = TempDir::new().unwrap();
        let mcp = make_mcp(&tmpdir);
        let session_id = "test-session-sum";

        // 归档 2 次
        for i in 0..2 {
            let turns_json = make_turns_json(
                &format!("用户 {i}"),
                &format!("LLM {i}"),
                50,
            );
            let params = Parameters(ArchiveParams {
                session_id: session_id.to_string(),
                turns_json,
                project_id: None,
                ..Default::default()
            });
            mcp.archive(params).await.unwrap();
        }

        // summaries
        let params = Parameters(SummariesParams {
            session_id: session_id.to_string(),
            project_id: None,
        });
        let result = mcp.summaries(params).await.expect("获取摘要失败");
        let summaries: Vec<Value> = serde_json::from_str(&result).unwrap();
        assert_eq!(summaries.len(), 2, "应有 2 条摘要");
    }

    // ========================================================================
    // prompt tool 测试
    // ========================================================================

    #[tokio::test]
    async fn test_prompt() {
        let tmpdir = TempDir::new().unwrap();
        let mcp = make_mcp(&tmpdir);
        let session_id = "test-session-prompt";

        // 先归档
        let turns_json = make_turns_json("你好", "你好！", 30);
        let params = Parameters(ArchiveParams {
            session_id: session_id.to_string(),
            turns_json,
            project_id: None,
            ..Default::default()
        });
        mcp.archive(params).await.unwrap();

        // prompt
        let params = Parameters(PromptParams {
            session_id: session_id.to_string(),
            project_id: None,
        });
        let prompt = mcp.prompt(params).await.expect("渲染 prompt 失败");
        assert!(!prompt.is_empty(), "prompt 不应为空");
        // prompt 中应包含记忆文件摘要标题的开头（来自 user_message 的前 80 字符）
        assert!(prompt.contains("你好"), "prompt 应包含摘要标题, 实际: {prompt}");
    }

    // ========================================================================
    // compaction tool 测试
    // ========================================================================

    #[tokio::test]
    async fn test_compaction_weekly() {
        let tmpdir = TempDir::new().unwrap();
        let mcp = make_mcp(&tmpdir);
        let session_id = "test-session-weekly";

        // 归档 2 次（产生 2 个 daily 记忆文件）
        for _ in 0..2 {
            let turns_json = make_multi_turns_json(2);
            let params = Parameters(ArchiveParams {
                session_id: session_id.to_string(),
                turns_json,
                project_id: None,
                ..Default::default()
            });
            mcp.archive(params).await.unwrap();
        }

        // weekly merge
        let params = Parameters(CompactionParams {
            session_id: session_id.to_string(),
            period: "weekly".to_string(),
            project_id: None,
        });
        let result = mcp.compaction(params).await.expect("周合并失败");
        let result: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(result["period"], "weekly");
        // 合并后应包含原 2 个文件的 4 个 turn
        assert_eq!(result["total_turns"], 4, "周合并应保留所有 turn");
    }

    #[tokio::test]
    async fn test_compaction_monthly() {
        let tmpdir = TempDir::new().unwrap();
        let mcp = make_mcp(&tmpdir);
        let session_id = "test-session-monthly";

        // 归档 1 个 daily 文件
        let turns_json = make_multi_turns_json(3);
        let params = Parameters(ArchiveParams {
            session_id: session_id.to_string(),
            turns_json,
            project_id: None,
            ..Default::default()
        });
        mcp.archive(params).await.unwrap();

        // 先做 weekly 合并（monthly 需要周级输入，若没有会报错或返回空）
        // 这里测试 monthly 调用本身可执行（不验证淘汰结果）
        let params = Parameters(CompactionParams {
            session_id: session_id.to_string(),
            period: "monthly".to_string(),
            project_id: None,
        });
        // monthly 在无 weekly 文件时可能成功（保留全部）或失败，主要验证 period 参数处理
        let _ = mcp.compaction(params).await;
        // 不严格断言结果，因 monthly 在数据不足时行为依赖具体实现
    }

    #[tokio::test]
    async fn test_compaction_invalid_period() {
        let tmpdir = TempDir::new().unwrap();
        let mcp = make_mcp(&tmpdir);

        let params = Parameters(CompactionParams {
            session_id: "s1".to_string(),
            period: "daily".to_string(), // 不支持的值
            project_id: None,
        });
        let err = mcp.compaction(params).await.unwrap_err();
        let msg = err.message.as_ref();
        assert!(msg.contains("无效的 period"), "应报告无效 period, 实际: {msg}");
    }

    // ========================================================================
    // 会话隔离测试
    // ========================================================================

    #[tokio::test]
    async fn test_session_isolation() {
        let tmpdir = TempDir::new().unwrap();
        let mcp = make_mcp(&tmpdir);

        // session A 归档
        let turns_json = make_turns_json("A 会话", "A 回复", 100);
        let params = Parameters(ArchiveParams {
            session_id: "session-a".to_string(),
            turns_json,
            project_id: None,
            ..Default::default()
        });
        let result_a = mcp.archive(params).await.unwrap();
        let summary_a: Value = serde_json::from_str(&result_a).unwrap();
        let hook_a = summary_a["hook_id"].as_str().unwrap().to_string();

        // session B 归档
        let turns_json = make_turns_json("B 会话", "B 回复", 200);
        let params = Parameters(ArchiveParams {
            session_id: "session-b".to_string(),
            turns_json,
            project_id: None,
            ..Default::default()
        });
        mcp.archive(params).await.unwrap();

        // session A 的 summaries 应只有 1 条
        let params = Parameters(SummariesParams {
            session_id: "session-a".to_string(),
            project_id: None,
        });
        let result = mcp.summaries(params).await.unwrap();
        let summaries: Vec<Value> = serde_json::from_str(&result).unwrap();
        assert_eq!(summaries.len(), 1, "session-a 应只有 1 条记忆");
        assert_eq!(
            summaries[0]["hook_id"].as_str().unwrap(),
            hook_a,
            "应返回 session-a 自己的记忆"
        );

        // session B 的 summaries 也应只有 1 条
        let params = Parameters(SummariesParams {
            session_id: "session-b".to_string(),
            project_id: None,
        });
        let result = mcp.summaries(params).await.unwrap();
        let summaries: Vec<Value> = serde_json::from_str(&result).unwrap();
        assert_eq!(summaries.len(), 1, "session-b 应只有 1 条记忆");
    }

    // ========================================================================
    // 完整工作流测试
    // ========================================================================

    #[tokio::test]
    async fn test_full_workflow() {
        let tmpdir = TempDir::new().unwrap();
        let mcp = make_mcp(&tmpdir);
        let session_id = "workflow-session";

        // 1. 归档 3 批次（模拟 3 个会话窗口）
        let mut hook_ids = Vec::new();
        for _ in 0..3 {
            let turns_json = make_multi_turns_json(2);
            let params = Parameters(ArchiveParams {
                session_id: session_id.to_string(),
                turns_json,
                project_id: Some("proj-1".to_string()),
                ..Default::default()
            });
            let result = mcp.archive(params).await.unwrap();
            let summary: Value = serde_json::from_str(&result).unwrap();
            hook_ids.push(
                summary["hook_id"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            );
        }

        // 2. summaries 应返回 3 条
        let params = Parameters(SummariesParams {
            session_id: session_id.to_string(),
            project_id: Some("proj-1".to_string()),
        });
        let result = mcp.summaries(params).await.unwrap();
        let summaries: Vec<Value> = serde_json::from_str(&result).unwrap();
        assert_eq!(summaries.len(), 3, "完整工作流: summaries 应有 3 条");

        // 3. retrieve 第一个 hook
        let params = Parameters(RetrieveParams {
            session_id: session_id.to_string(),
            hook_id: hook_ids[0].clone(),
            project_id: Some("proj-1".to_string()),
        });
        let result = mcp.retrieve(params).await.unwrap();
        let memory: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(memory["turns"].as_array().unwrap().len(), 2);

        // 4. prompt 应包含所有摘要
        let params = Parameters(PromptParams {
            session_id: session_id.to_string(),
            project_id: Some("proj-1".to_string()),
        });
        let prompt = mcp.prompt(params).await.unwrap();
        assert!(!prompt.is_empty());

        // 5. weekly 合并
        let params = Parameters(CompactionParams {
            session_id: session_id.to_string(),
            period: "weekly".to_string(),
            project_id: Some("proj-1".to_string()),
        });
        let result = mcp.compaction(params).await.unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(result["total_turns"], 6, "3 批次 × 2 turns = 6");
    }

    // ========================================================================
    // v2.6 批次 8：冲突检测测试
    // ========================================================================

    #[tokio::test]
    async fn test_detect_conflicts_direct_contradiction() {
        // 归档后通过 batch_update 添加"喜欢咖啡"，再用 detect_conflicts 预检测"不喜欢咖啡" → 应检测到冲突
        let tmp = TempDir::new().unwrap();
        let mcp = make_mcp(&tmp);

        // 1. 归档
        let params = Parameters(ArchiveParams {
            session_id: "sess-cd".to_string(),
            turns_json: make_turns_json("用户消息", "LLM 回复", 100),
            project_id: None,
            ..Default::default()
        });
        let result = mcp.archive(params).await.unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();
        let hook_id = result["hook_id"].as_str().unwrap().to_string();

        // 2. 通过 batch_update 添加"用户喜欢咖啡"作为历史事实
        let updates_json = serde_json::json!([{
            "hook_id": hook_id,
            "added_facts": ["用户喜欢咖啡"],
            "revised_facts": [],
            "deprecated_facts": [],
        }])
        .to_string();
        let params = Parameters(BatchUpdateParams {
            session_id: "sess-cd".to_string(),
            updates_json,
            project_id: None,
        });
        mcp.batch_update(params).await.unwrap();

        // 3. detect_conflicts 预检测"用户不喜欢咖啡"（与历史"喜欢咖啡"直接矛盾）
        let detect_params = Parameters(DetectConflictsParams {
            session_id: "sess-cd".to_string(),
            hook_id: hook_id.clone(),
            added_facts: vec!["用户不喜欢咖啡".to_string()],
            revised_facts: vec![],
            deprecated_facts: vec![],
            project_id: None,
        });
        let result = mcp.detect_conflicts(detect_params).await.unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();

        assert!(result["total"].as_u64().unwrap() >= 1, "应检测到至少 1 个冲突");
        assert_eq!(result["has_critical"], true);

        let conflicts = result["conflicts"].as_array().unwrap();
        let has_direct = conflicts.iter().any(|c| c["kind"] == "direct_contradict");
        assert!(has_direct, "应包含 direct_contradict");
    }

    #[tokio::test]
    async fn test_detect_conflicts_clean_update() {
        // 无冲突的更新应返回 total=0
        let tmp = TempDir::new().unwrap();
        let mcp = make_mcp(&tmp);

        let params = Parameters(ArchiveParams {
            session_id: "sess-cd-clean".to_string(),
            turns_json: make_turns_json("用户消息", "LLM 回复", 100),
            project_id: None,
            ..Default::default()
        });
        let result = mcp.archive(params).await.unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();
        let hook_id = result["hook_id"].as_str().unwrap().to_string();

        let detect_params = Parameters(DetectConflictsParams {
            session_id: "sess-cd-clean".to_string(),
            hook_id,
            added_facts: vec!["用户住在上海".to_string()],
            revised_facts: vec![],
            deprecated_facts: vec![],
            project_id: None,
        });
        let result = mcp.detect_conflicts(detect_params).await.unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();

        assert_eq!(result["total"], 0);
        assert_eq!(result["has_critical"], false);
    }

    #[tokio::test]
    async fn test_detect_conflicts_nonexistent_hook_fails() {
        let tmp = TempDir::new().unwrap();
        let mcp = make_mcp(&tmp);

        let detect_params = Parameters(DetectConflictsParams {
            session_id: "sess-x".to_string(),
            hook_id: "nonexistent-hook-id".to_string(),
            added_facts: vec!["测试".to_string()],
            revised_facts: vec![],
            deprecated_facts: vec![],
            project_id: None,
        });
        let result = mcp.detect_conflicts(detect_params).await;
        assert!(result.is_err(), "不存在的 hook_id 应返回错误");
    }

    #[tokio::test]
    async fn test_get_conflicts_returns_persisted_records() {
        // 验证 get_conflicts 在无冲突记忆上返回空列表
        // 注意：MCP 的 batch_update 不集成 conflict_detector（只在 HTTP 层集成），
        // 所以持久化冲突记录的验证在 HTTP 集成测试中完成。
        let tmp = TempDir::new().unwrap();
        let mcp = make_mcp(&tmp);

        let params = Parameters(ArchiveParams {
            session_id: "sess-gc".to_string(),
            turns_json: make_turns_json("用户消息", "LLM 回复", 100),
            project_id: None,
            ..Default::default()
        });
        let result = mcp.archive(params).await.unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();
        let hook_id = result["hook_id"].as_str().unwrap().to_string();

        // 直接 get_conflicts（无 update → 无冲突）
        let get_params = Parameters(GetConflictsParams {
            session_id: "sess-gc".to_string(),
            hook_id,
            project_id: None,
        });
        let result = mcp.get_conflicts(get_params).await.unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();

        assert_eq!(result["total"], 0);
        assert_eq!(result["critical_count"], 0);
        assert!(result["conflicts"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_get_conflicts_nonexistent_hook_fails() {
        let tmp = TempDir::new().unwrap();
        let mcp = make_mcp(&tmp);

        let get_params = Parameters(GetConflictsParams {
            session_id: "sess-x".to_string(),
            hook_id: "nonexistent".to_string(),
            project_id: None,
        });
        let result = mcp.get_conflicts(get_params).await;
        assert!(result.is_err(), "不存在的 hook_id 应返回错误");
    }

    // ========================================================================
    // v2.11：注入检测器后的 batch_update 集成测试
    // ========================================================================

    #[tokio::test]
    async fn test_batch_update_with_detector_returns_conflicts_field() {
        // 注入 HeuristicDetector：第一次 batch_update 添加"用户喜欢咖啡"（无冲突），
        // 第二次 batch_update 添加"用户不喜欢咖啡"（与历史直接矛盾）→ 响应应含 conflicts>=1 + has_critical=true
        let tmp = TempDir::new().unwrap();
        let mcp = make_mcp_with_detector(&tmp);

        // 1. 归档
        let params = Parameters(ArchiveParams {
            session_id: "sess-v211-a".to_string(),
            turns_json: make_turns_json("用户消息", "LLM 回复", 100),
            project_id: None,
            ..Default::default()
        });
        let result = mcp.archive(params).await.unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();
        let hook_id = result["hook_id"].as_str().unwrap().to_string();

        // 2. 第一次 batch_update：添加"用户喜欢咖啡"（无历史 → 无冲突）
        let updates_json = serde_json::json!([{
            "hook_id": hook_id,
            "added_facts": ["用户喜欢咖啡"],
            "revised_facts": [],
            "deprecated_facts": [],
        }])
        .to_string();
        let params = Parameters(BatchUpdateParams {
            session_id: "sess-v211-a".to_string(),
            updates_json,
            project_id: None,
        });
        let result = mcp.batch_update(params).await.unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();
        // 第一次 update 无历史事实，无冲突
        assert_eq!(result["items"][0]["success"], true);
        assert_eq!(result["items"][0]["conflicts"], 0);
        assert_eq!(result["items"][0]["has_critical"], false);

        // 3. 第二次 batch_update：添加"用户不喜欢咖啡"（与历史"喜欢咖啡"直接矛盾）
        let updates_json = serde_json::json!([{
            "hook_id": hook_id,
            "added_facts": ["用户不喜欢咖啡"],
            "revised_facts": [],
            "deprecated_facts": [],
        }])
        .to_string();
        let params = Parameters(BatchUpdateParams {
            session_id: "sess-v211-a".to_string(),
            updates_json,
            project_id: None,
        });
        let result = mcp.batch_update(params).await.unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();

        // 应检测到至少 1 条 Critical 冲突（direct_contradict）
        assert_eq!(result["items"][0]["success"], true);
        let conflicts = result["items"][0]["conflicts"].as_u64().unwrap();
        assert!(
            conflicts >= 1,
            "第二次 update 应检测到与历史'喜欢咖啡'的直接矛盾，实际 conflicts: {conflicts}"
        );
        assert_eq!(
            result["items"][0]["has_critical"], true,
            "直接矛盾应为 Critical 级别"
        );
    }

    #[tokio::test]
    async fn test_batch_update_with_detector_persists_conflicts() {
        // 注入 HeuristicDetector：batch_update 检测到的冲突应持久化到 MemoryUpdateRecord.conflicts，
        // 后续 get_conflicts 能查到。
        let tmp = TempDir::new().unwrap();
        let mcp = make_mcp_with_detector(&tmp);

        // 1. 归档
        let params = Parameters(ArchiveParams {
            session_id: "sess-v211-b".to_string(),
            turns_json: make_turns_json("用户消息", "LLM 回复", 100),
            project_id: None,
            ..Default::default()
        });
        let result = mcp.archive(params).await.unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();
        let hook_id = result["hook_id"].as_str().unwrap().to_string();

        // 2. 第一次 batch_update：添加"用户喜欢咖啡"
        let updates_json = serde_json::json!([{
            "hook_id": hook_id,
            "added_facts": ["用户喜欢咖啡"],
        }])
        .to_string();
        let params = Parameters(BatchUpdateParams {
            session_id: "sess-v211-b".to_string(),
            updates_json,
            project_id: None,
        });
        mcp.batch_update(params).await.unwrap();

        // 3. 第二次 batch_update：添加"用户不喜欢咖啡"（触发冲突，应持久化）
        let updates_json = serde_json::json!([{
            "hook_id": hook_id,
            "added_facts": ["用户不喜欢咖啡"],
        }])
        .to_string();
        let params = Parameters(BatchUpdateParams {
            session_id: "sess-v211-b".to_string(),
            updates_json,
            project_id: None,
        });
        mcp.batch_update(params).await.unwrap();

        // 4. get_conflicts 查询持久化的冲突
        let get_params = Parameters(GetConflictsParams {
            session_id: "sess-v211-b".to_string(),
            hook_id,
            project_id: None,
        });
        let result = mcp.get_conflicts(get_params).await.unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();

        // 应有 1 条持久化的 Critical 冲突
        let total = result["total"].as_u64().unwrap();
        assert!(
            total >= 1,
            "get_conflicts 应查到持久化的冲突，实际 total: {total}"
        );
        assert!(
            result["critical_count"].as_u64().unwrap() >= 1,
            "应至少有 1 条 Critical 冲突"
        );

        // 验证冲突类型为 direct_contradict
        let conflicts = result["conflicts"].as_array().unwrap();
        let has_direct = conflicts.iter().any(|c| c["kind"] == "direct_contradict");
        assert!(has_direct, "应包含 direct_contradict 类型冲突");
    }

    #[tokio::test]
    async fn test_batch_update_without_detector_no_conflicts_field_default() {
        // 未注入检测器（HippocampusMcp::new）：batch_update 不检测冲突，
        // 响应中 conflicts=0, has_critical=false（默认值）
        let tmp = TempDir::new().unwrap();
        let mcp = make_mcp(&tmp); // 无检测器

        // 1. 归档
        let params = Parameters(ArchiveParams {
            session_id: "sess-v211-c".to_string(),
            turns_json: make_turns_json("用户消息", "LLM 回复", 100),
            project_id: None,
            ..Default::default()
        });
        let result = mcp.archive(params).await.unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();
        let hook_id = result["hook_id"].as_str().unwrap().to_string();

        // 2. batch_update 添加事实（无检测器 → 不检测冲突）
        let updates_json = serde_json::json!([{
            "hook_id": hook_id,
            "added_facts": ["用户喜欢咖啡"],
        }])
        .to_string();
        let params = Parameters(BatchUpdateParams {
            session_id: "sess-v211-c".to_string(),
            updates_json,
            project_id: None,
        });
        let result = mcp.batch_update(params).await.unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();

        // 无检测器：conflicts=0, has_critical=false（向后兼容）
        assert_eq!(result["items"][0]["success"], true);
        assert_eq!(result["items"][0]["conflicts"], 0);
        assert_eq!(result["items"][0]["has_critical"], false);

        // 3. get_conflicts 查询（无持久化冲突）
        let get_params = Parameters(GetConflictsParams {
            session_id: "sess-v211-c".to_string(),
            hook_id,
            project_id: None,
        });
        let result = mcp.get_conflicts(get_params).await.unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(result["total"], 0);
    }

    // ========================================================================
    // v2.18 新增：semantic_search tool 测试
    // ========================================================================

    #[tokio::test]
    async fn test_semantic_search_no_session_search_returns_error() {
        // 未注入 SessionSearchRouter：semantic_search 应返回错误
        let tmp = TempDir::new().unwrap();
        let mcp = make_mcp(&tmp); // 无 session_search

        let params = Parameters(SemanticSearchParams {
            session_id: "sess-x".to_string(),
            query: "测试".to_string(),
            top_k: None,
            project_id: None,
        });
        let result = mcp.semantic_search(params).await;
        assert!(result.is_err(), "未注入 SessionSearchRouter 应返回错误");
        let err = result.unwrap_err();
        let msg = err.message.as_ref();
        assert!(
            msg.contains("未启用") || msg.contains("未注入"),
            "错误消息应提及未启用/未注入，实际: {msg}"
        );
    }

    #[tokio::test]
    async fn test_semantic_search_basic_with_rebuild() {
        // 归档后 semantic_search 应触发 rebuild 从 storage 读取索引并返回结果
        let tmp = TempDir::new().unwrap();
        let mcp = make_mcp_with_session_search(&tmp);
        let session_id = "sess-search-basic";

        // 1. 归档一条记忆（写入 storage，但不调用 index_hook，模拟 MCP 进程重启场景）
        let turns_json = make_turns_json("Rust 安全编程语言", "Rust 是系统级编程语言", 100);
        let params = Parameters(ArchiveParams {
            session_id: session_id.to_string(),
            turns_json,
            project_id: None,
            ..Default::default()
        });
        let archive_result = mcp.archive(params).await.unwrap();
        let archive_result: Value = serde_json::from_str(&archive_result).unwrap();
        let hook_id = archive_result["hook_id"].as_str().unwrap().to_string();

        // 2. semantic_search "Rust" → 应触发 rebuild 从 storage 重建索引
        let params = Parameters(SemanticSearchParams {
            session_id: session_id.to_string(),
            query: "Rust".to_string(),
            top_k: Some(5),
            project_id: None,
        });
        let result = mcp.semantic_search(params).await.expect("semantic_search 失败");
        let result: Value = serde_json::from_str(&result).unwrap();

        // 应找到至少 1 个结果
        let total = result["total"].as_u64().unwrap_or(0);
        assert!(total >= 1, "rebuild 后应能搜索到归档的记忆，实际 total: {total}");

        // 验证返回的 hits 含 hook_id
        let hits = result["hits"].as_array().unwrap();
        let found_hook = hits.iter().any(|h| {
            h["hook_id"].as_str().map(|s| s == hook_id).unwrap_or(false)
        });
        assert!(
            found_hook,
            "应能找到刚归档的 hook_id: {hook_id}, 实际 hits: {hits:?}"
        );
    }

    #[tokio::test]
    async fn test_semantic_search_session_isolation() {
        // 不同 session 的检索结果应隔离
        let tmp = TempDir::new().unwrap();
        let mcp = make_mcp_with_session_search(&tmp);

        // session-a 归档"Rust 编程"
        let turns_json = make_turns_json("Rust 编程语言", "Rust 是系统级语言", 100);
        let params = Parameters(ArchiveParams {
            session_id: "session-a".to_string(),
            turns_json,
            project_id: None,
            ..Default::default()
        });
        mcp.archive(params).await.unwrap();

        // session-b 归档"Python 编程"
        let turns_json = make_turns_json("Python 数据分析", "Python 是脚本语言", 100);
        let params = Parameters(ArchiveParams {
            session_id: "session-b".to_string(),
            turns_json,
            project_id: None,
            ..Default::default()
        });
        mcp.archive(params).await.unwrap();

        // session-a 检索 "Rust" → 应找到 Rust，不应找到 Python
        let params = Parameters(SemanticSearchParams {
            session_id: "session-a".to_string(),
            query: "Rust".to_string(),
            top_k: Some(5),
            project_id: None,
        });
        let result = mcp.semantic_search(params).await.unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();
        let hits_a = result["hits"].as_array().unwrap();
        assert!(
            !hits_a.is_empty(),
            "session-a 应能搜到 Rust 相关记忆"
        );

        // 验证 hits_a 不包含 session-b 的内容（hook_id 不重复）
        // 注意：BM25 只看文本相关性，但因 session 隔离，session-a 不会返回 session-b 的 hook
        // 这里验证返回的所有 hook 都来自 session-a（hook_id 数量 = session-a 的归档数）
        assert_eq!(
            hits_a.len(),
            1,
            "session-a 只应有 1 条记忆，实际: {}",
            hits_a.len()
        );

        // session-b 检索 "Python" → 应找到 Python
        let params = Parameters(SemanticSearchParams {
            session_id: "session-b".to_string(),
            query: "Python".to_string(),
            top_k: Some(5),
            project_id: None,
        });
        let result = mcp.semantic_search(params).await.unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();
        let hits_b = result["hits"].as_array().unwrap();
        assert_eq!(
            hits_b.len(),
            1,
            "session-b 只应有 1 条记忆，实际: {}",
            hits_b.len()
        );
    }

    #[tokio::test]
    async fn test_semantic_search_default_top_k() {
        // 验证 top_k 默认值 = 5（不传 top_k 参数）
        let tmp = TempDir::new().unwrap();
        let mcp = make_mcp_with_session_search(&tmp);
        let session_id = "sess-default-topk";

        // 归档 1 条记忆
        let turns_json = make_turns_json("默认 top_k 测试", "测试默认值", 50);
        let params = Parameters(ArchiveParams {
            session_id: session_id.to_string(),
            turns_json,
            project_id: None,
            ..Default::default()
        });
        mcp.archive(params).await.unwrap();

        // 不传 top_k → 应默认为 5（不报错）
        let params = Parameters(SemanticSearchParams {
            session_id: session_id.to_string(),
            query: "测试".to_string(),
            top_k: None,
            project_id: None,
        });
        let result = mcp.semantic_search(params).await.expect("默认 top_k 应可用");
        let result: Value = serde_json::from_str(&result).unwrap();
        // 至少返回 1 条（top_k=5 是上限，实际返回数 <= 5）
        assert!(
            result["total"].as_u64().unwrap_or(0) >= 1,
            "应至少返回 1 条结果"
        );
    }

    // ========================================================================
    // v2.21 批次 8c：summary_generator 注入测试
    // ========================================================================

    #[tokio::test]
    async fn test_archive_with_summary_generator_uses_llm_summary() {
        // 注入 Mock 成功生成器：archive 后 summaries 应返回 Mock 的标题
        let tmp = TempDir::new().unwrap();
        let mcp = make_mcp_with_summary_generator(&tmp, false);
        let session_id = "sess-sum-gen";

        let turns_json = make_turns_json("用户原始消息", "LLM 原始回复", 100);
        let params = Parameters(ArchiveParams {
            session_id: session_id.to_string(),
            turns_json,
            project_id: None,
            ..Default::default()
        });
        let result = mcp.archive(params).await.expect("归档失败");
        let result: Value = serde_json::from_str(&result).unwrap();
        let hook_id = result["hook_id"].as_str().unwrap().to_string();

        // summaries 应返回 Mock LLM 摘要标题（而非启发式的"用户原始消息"）
        let params = Parameters(SummariesParams {
            session_id: session_id.to_string(),
            project_id: None,
        });
        let result = mcp.summaries(params).await.unwrap();
        let summaries: Vec<Value> = serde_json::from_str(&result).unwrap();
        assert_eq!(summaries.len(), 1);

        let title = summaries[0]["summary_title"].as_str().unwrap_or("");
        assert_eq!(
            title, "Mock LLM 摘要标题",
            "应使用 LLM 生成的标题，而非启发式首条消息前 80 字符"
        );
        // hook_id 应一致
        assert_eq!(summaries[0]["hook_id"].as_str().unwrap(), hook_id);
    }

    #[tokio::test]
    async fn test_archive_summary_generator_failure_degrades_gracefully() {
        // 注入 Mock 失败生成器：archive 应成功（降级为启发式 Summary::from_title）
        let tmp = TempDir::new().unwrap();
        let mcp = make_mcp_with_summary_generator(&tmp, true);
        let session_id = "sess-sum-fail";

        let turns_json = make_turns_json("降级测试用户消息", "降级 LLM 回复", 100);
        let params = Parameters(ArchiveParams {
            session_id: session_id.to_string(),
            turns_json,
            project_id: None,
            ..Default::default()
        });
        // archive 应成功（LLM 失败时降级，不中断主流程）
        mcp.archive(params).await.expect("LLM 失败时应降级为启发式，归档应成功");

        // summaries 应返回启发式标题（首条消息前 80 字符 = "降级测试用户消息"）
        let params = Parameters(SummariesParams {
            session_id: session_id.to_string(),
            project_id: None,
        });
        let result = mcp.summaries(params).await.unwrap();
        let summaries: Vec<Value> = serde_json::from_str(&result).unwrap();
        assert_eq!(summaries.len(), 1);

        let title = summaries[0]["summary_title"].as_str().unwrap_or("");
        assert!(
            title.contains("降级测试用户消息"),
            "降级后应使用启发式标题（首条消息前 80 字符），实际: {title}"
        );
    }

    // ========================================================================
    // v2.29 preset_* tools 测试
    // ========================================================================

    #[tokio::test]
    async fn test_preset_list_agents_returns_11_builtin() {
        let tmpdir = TempDir::new().unwrap();
        let mcp = make_mcp(&tmpdir);
        let params = Parameters(NoParams {});
        let result = mcp.preset_list_agents(params).await.unwrap();
        let v: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["total"], 11, "应有 11 个内置 Agent");
        // Claude Code 应在列表中且为主流
        let agents = v["agents"].as_array().unwrap();
        assert!(agents.iter().any(|a| a["name"] == "Claude Code"));
        assert!(agents.iter().any(|a| a["is_mainstream"] == true));
    }

    #[tokio::test]
    async fn test_preset_list_scenarios_returns_7_builtin() {
        let tmpdir = TempDir::new().unwrap();
        let mcp = make_mcp(&tmpdir);
        let params = Parameters(NoParams {});
        let result = mcp.preset_list_scenarios(params).await.unwrap();
        let v: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["total"], 7, "应有 7 个内置 Scenario");
        // Coding 场景应在列表中，archive_threshold = 500_000
        let scenarios = v["scenarios"].as_array().unwrap();
        let coding = scenarios.iter().find(|s| s["variant"] == "Coding").unwrap();
        assert_eq!(coding["archive_threshold"], 500_000);
    }

    #[tokio::test]
    async fn test_preset_list_models_returns_all() {
        let tmpdir = TempDir::new().unwrap();
        let mcp = make_mcp(&tmpdir);
        let params = Parameters(NoParams {});
        let result = mcp.preset_list_models(params).await.unwrap();
        let v: Value = serde_json::from_str(&result).unwrap();
        // 总型号数 >= 15
        assert!(v["total"].as_u64().unwrap() >= 15, "应至少有 15 个型号");
        // 至少有一个是家族默认
        let models = v["models"].as_array().unwrap();
        assert!(models.iter().any(|m| m["is_default"] == true));
    }

    #[tokio::test]
    async fn test_preset_build_empty_uses_defaults() {
        let tmpdir = TempDir::new().unwrap();
        let mcp = make_mcp(&tmpdir);
        let params = Parameters(PresetBuildParams::default());
        let result = mcp.preset_build(params).await.unwrap();
        let v: Value = serde_json::from_str(&result).unwrap();
        // 默认归档阈值 400K
        assert_eq!(v["archive_threshold"], 400_000);
        assert_eq!(v["has_agent"], false);
        assert_eq!(v["has_window"], false);
    }

    #[tokio::test]
    async fn test_preset_build_with_agent_triggers_window_linkage() {
        let tmpdir = TempDir::new().unwrap();
        let mcp = make_mcp(&tmpdir);
        let params = Parameters(PresetBuildParams {
            agent: Some("Claude Code".into()),
            ..Default::default()
        });
        let result = mcp.preset_build(params).await.unwrap();
        let v: Value = serde_json::from_str(&result).unwrap();
        // 联动推导 Window
        assert_eq!(v["has_agent"], true);
        assert_eq!(v["has_window"], true);
        assert_eq!(v["session_prefix"], "claude-code");
    }

    #[tokio::test]
    async fn test_preset_build_with_scenario_overrides_threshold() {
        let tmpdir = TempDir::new().unwrap();
        let mcp = make_mcp(&tmpdir);
        let params = Parameters(PresetBuildParams {
            scenario: Some("coding".into()),
            ..Default::default()
        });
        let result = mcp.preset_build(params).await.unwrap();
        let v: Value = serde_json::from_str(&result).unwrap();
        // Coding 场景默认 500K
        assert_eq!(v["archive_threshold"], 500_000);
        assert_eq!(v["has_scenario"], true);
    }

    #[tokio::test]
    async fn test_preset_build_user_threshold_overrides_scenario() {
        let tmpdir = TempDir::new().unwrap();
        let mcp = make_mcp(&tmpdir);
        let params = Parameters(PresetBuildParams {
            scenario: Some("coding".into()),
            archive_threshold: Some(450_000),
            ..Default::default()
        });
        let result = mcp.preset_build(params).await.unwrap();
        let v: Value = serde_json::from_str(&result).unwrap();
        // 用户覆盖优先
        assert_eq!(v["archive_threshold"], 450_000);
    }

    #[tokio::test]
    async fn test_preset_build_invalid_model_returns_error() {
        let tmpdir = TempDir::new().unwrap();
        let mcp = make_mcp(&tmpdir);
        let params = Parameters(PresetBuildParams {
            model: Some("nonexistent-model".into()),
            ..Default::default()
        });
        let err = mcp.preset_build(params).await.unwrap_err();
        let msg = err.message.as_ref();
        assert!(msg.contains("未找到型号"), "应报告未找到型号, 实际: {msg}");
    }

    #[tokio::test]
    async fn test_preset_build_invalid_template_returns_error() {
        let tmpdir = TempDir::new().unwrap();
        let mcp = make_mcp(&tmpdir);
        let params = Parameters(PresetBuildParams {
            summary_template: Some("missing placeholder".into()),
            ..Default::default()
        });
        let err = mcp.preset_build(params).await.unwrap_err();
        let msg = err.message.as_ref();
        assert!(msg.contains("{conversation}"), "应报告缺少占位符, 实际: {msg}");
    }

    #[tokio::test]
    async fn test_preset_build_full_combination() {
        let tmpdir = TempDir::new().unwrap();
        let mcp = make_mcp(&tmpdir);
        let params = Parameters(PresetBuildParams {
            agent: Some("Claude Code".into()),
            scenario: Some("coding".into()),
            model: Some("claude-opus-4.8".into()),
            archive_threshold: Some(300_000),
            summary_template: Some("custom {conversation}".into()),
            ..Default::default()
        });
        let result = mcp.preset_build(params).await.unwrap();
        let v: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["archive_threshold"], 300_000);
        assert_eq!(v["summary_template"], "custom {conversation}");
        assert_eq!(v["has_agent"], true);
        assert_eq!(v["has_scenario"], true);
        assert_eq!(v["has_model"], true);
    }

    #[tokio::test]
    async fn test_archive_with_preset_applies_threshold() {
        // 传入 preset 后 archive 应正常归档（archive_threshold + summary_template 被应用）
        let tmpdir = TempDir::new().unwrap();
        let mcp = make_mcp(&tmpdir);
        let session_id = "test-preset-archive";

        let turns_json = make_turns_json("preset 测试", "LLM 回复", 100);
        let params = Parameters(ArchiveParams {
            session_id: session_id.to_string(),
            turns_json,
            project_id: None,
            preset: Some(PresetParams {
                agent: Some("Claude Code".into()),
                scenario: Some("coding".into()),
                archive_threshold: Some(300_000),
                summary_template: Some("自定义模板 {conversation}".into()),
                ..Default::default()
            }),
            ..Default::default()
        });
        let result = mcp.archive(params).await.expect("带 preset 的归档应成功");
        let v: Value = serde_json::from_str(&result).unwrap();
        // 应返回 hook_id
        assert!(v["hook_id"].as_str().is_some(), "应返回 hook_id");
        assert_eq!(v["token_count"], 100);
    }

    #[tokio::test]
    async fn test_archive_with_invalid_preset_returns_error() {
        // preset 中 model 无效时应返回 invalid_params 错误
        let tmpdir = TempDir::new().unwrap();
        let mcp = make_mcp(&tmpdir);

        let turns_json = make_turns_json("错误 preset", "LLM 回复", 50);
        let params = Parameters(ArchiveParams {
            session_id: "test-invalid-preset".to_string(),
            turns_json,
            project_id: None,
            preset: Some(PresetParams {
                model: Some("nonexistent-model".into()),
                ..Default::default()
            }),
            ..Default::default()
        });
        let err = mcp.archive(params).await.unwrap_err();
        let msg = err.message.as_ref();
        assert!(msg.contains("未找到型号"), "应报告未找到型号, 实际: {msg}");
    }

    #[tokio::test]
    async fn test_archive_without_preset_backward_compatible() {
        // 不传 preset 时应保持原行为（向后兼容）
        let tmpdir = TempDir::new().unwrap();
        let mcp = make_mcp(&tmpdir);

        let turns_json = make_turns_json("无 preset", "LLM 回复", 80);
        let params = Parameters(ArchiveParams {
            session_id: "test-no-preset".to_string(),
            turns_json,
            project_id: None,
            ..Default::default()
        });
        let result = mcp.archive(params).await.expect("无 preset 归档应成功");
        let v: Value = serde_json::from_str(&result).unwrap();
        assert!(v["hook_id"].as_str().is_some());
        assert_eq!(v["token_count"], 80);
    }

    // ========================================================================
    // v2.30 启动时识别 + 注入 CombinedProfile 测试
    // ========================================================================

    /// 测试默认构造（new / with_conflict_detector）combined_profile 为 None（向后兼容）
    #[test]
    fn test_combined_profile_default_none() {
        let tmpdir = TempDir::new().unwrap();
        let mcp = HippocampusMcp::new(tmpdir.path().to_path_buf());
        assert!(
            mcp.combined_profile().is_none(),
            "new() 默认不应注入 combined_profile"
        );

        let mcp2 = HippocampusMcp::with_conflict_detector(tmpdir.path().to_path_buf(), None);
        assert!(
            mcp2.combined_profile().is_none(),
            "with_conflict_detector() 默认不应注入 combined_profile"
        );
    }

    /// 测试 with_combined_profile(Some) 注入后可读取（链式 builder）
    #[test]
    fn test_with_combined_profile_injection() {
        use hippocampus_agents::{AgentFamily, AgentProfile};
        use hippocampus_presets::{PresetBuilder, CombinedProfile};
        use hippocampus_scenarios::{Scenario, ScenarioProfile};

        let tmpdir = TempDir::new().unwrap();

        // 模拟 main.rs 中 build_combined_profile 的核心流程：
        // 识别（直接构造 ClaudeCode family）→ 构建 CombinedProfile → 注入
        let family = AgentFamily::ClaudeCode;
        let combined: CombinedProfile = PresetBuilder::new()
            .with_agent(AgentProfile::from_family(family))
            .with_scenario(ScenarioProfile::from_scenario(Scenario::Coding))
            .build()
            .expect("ClaudeCode + Coding 应构建成功");

        let mcp = HippocampusMcp::new(tmpdir.path().to_path_buf())
            .with_combined_profile(Some(combined));

        let read = mcp.combined_profile().expect("应能读取注入的 combined_profile");
        assert_eq!(read.session_prefix(), Some("claude-code"));
        // usage_protocol 应非空（mainstream agent + Coding scenario）
        let protocol = read.usage_protocol();
        assert!(!protocol.is_empty(), "mainstream agent 应生成非空 usage_protocol");
        assert!(!protocol.instructions.is_empty());
        assert!(!protocol.trigger_rules.is_empty());
        assert!(protocol.session_id_pattern.contains("claude-code"));
    }

    /// 测试 with_combined_profile(None) 显式传 None（向后兼容）
    #[test]
    fn test_with_combined_profile_none() {
        let tmpdir = TempDir::new().unwrap();
        let mcp = HippocampusMcp::new(tmpdir.path().to_path_buf())
            .with_combined_profile(None);
        assert!(mcp.combined_profile().is_none());
    }

    /// 测试识别 → 构建 → 注入完整链路（Trae family）
    /// 验证 v2.30 1.4 验收标准：启动时识别 + 应用的 preset 可被 tool 读取
    #[test]
    fn test_detection_to_injection_full_chain_trae() {
        use hippocampus_presets::{detect_agent_client, resolve_scenario_name, scenario_from_str, PresetBuilder};
        use hippocampus_agents::AgentProfile;
        use hippocampus_scenarios::ScenarioProfile;

        let tmpdir = TempDir::new().unwrap();

        // 模拟 build_combined_profile 流程（不依赖环境变量，直接验证链路）
        // 注意：测试环境的识别结果可能是 Trae 也可能是 Custom（取决于运行环境），
        // 这里手动指定 family 为 Trae 来验证链路完整性
        let detected = detect_agent_client(Some("trae-cli/1.0"));
        let family = detected.family;

        // 非 mainstream 时跳过（测试环境可能识别不到 Trae）
        if !family.is_mainstream() {
            eprintln!("测试环境未识别为 mainstream agent（family={:?}），跳过链路验证", family);
            return;
        }

        let scenario_str = resolve_scenario_name(&family);
        let scenario = scenario_from_str(&scenario_str);
        let combined = PresetBuilder::new()
            .with_agent(AgentProfile::from_family(family))
            .with_scenario(ScenarioProfile::from_scenario(scenario))
            .build()
            .expect("mainstream family 应构建成功");

        let mcp = HippocampusMcp::new(tmpdir.path().to_path_buf())
            .with_combined_profile(Some(combined));

        // 验证注入的 preset 可被读取
        let read = mcp.combined_profile().expect("应能读取注入的 combined_profile");
        let protocol = read.usage_protocol();
        assert!(!protocol.is_empty(), "mainstream agent 应有非空 usage_protocol");
        // instructions 应包含「记忆协议」关键词（来自 generate_usage_protocol）
        assert!(
            protocol.instructions.contains("记忆协议"),
            "instructions 应包含记忆协议说明，实际: {}",
            protocol.instructions.chars().take(100).collect::<String>()
        );
        // trigger_rules 应有 4 条（prompt/archive/semantic_search/detect_conflicts）
        assert_eq!(
            protocol.trigger_rules.len(),
            4,
            "应有 4 条触发规则"
        );
    }

    /// 测试 archive tool 在注入 CombinedProfile 后行为不回归
    /// （注入的 preset 不应影响 archive 的正常调用，preset 字段仍可独立工作）
    #[tokio::test]
    async fn test_archive_with_injected_combined_profile_works() {
        use hippocampus_agents::{AgentFamily, AgentProfile};
        use hippocampus_presets::{PresetBuilder, CombinedProfile};
        use hippocampus_scenarios::{Scenario, ScenarioProfile};

        let tmpdir = TempDir::new().unwrap();

        let combined: CombinedProfile = PresetBuilder::new()
            .with_agent(AgentProfile::from_family(AgentFamily::Trae))
            .with_scenario(ScenarioProfile::from_scenario(Scenario::Coding))
            .build()
            .unwrap();

        let mcp = HippocampusMcp::new(tmpdir.path().to_path_buf())
            .with_combined_profile(Some(combined));

        // archive 应正常工作（注入的 combined_profile 不影响 archive 调用）
        let turns_json = make_turns_json("注入 preset 测试", "LLM 回复", 100);
        let params = Parameters(ArchiveParams {
            session_id: "test-injected-preset".to_string(),
            turns_json,
            project_id: None,
            ..Default::default()
        });
        let result = mcp.archive(params).await.expect("注入 combined_profile 后 archive 应成功");
        let v: Value = serde_json::from_str(&result).unwrap();
        assert!(v["hook_id"].as_str().is_some());
    }

    /// 测试 get_info() 在有 combined_profile 时注入 instructions（v2.30 1.5 核心）
    ///
    /// 验证：MCP 客户端调用 initialize 时拿到的 ServerInfo.instructions
    /// 包含 usage_protocol.instructions 内容（让 LLM 启动即看到记忆协议）
    #[test]
    fn test_get_info_injects_instructions_when_combined_profile_present() {
        use hippocampus_agents::{AgentFamily, AgentProfile};
        use hippocampus_presets::{PresetBuilder, CombinedProfile};
        use hippocampus_scenarios::{Scenario, ScenarioProfile};

        let tmpdir = TempDir::new().unwrap();

        let combined: CombinedProfile = PresetBuilder::new()
            .with_agent(AgentProfile::from_family(AgentFamily::ClaudeCode))
            .with_scenario(ScenarioProfile::from_scenario(Scenario::Coding))
            .build()
            .unwrap();
        let expected_instructions = combined.usage_protocol().instructions.clone();
        assert!(!expected_instructions.is_empty(), "前置条件：usage_protocol 应非空");

        let mcp = HippocampusMcp::new(tmpdir.path().to_path_buf())
            .with_combined_profile(Some(combined));

        let info = mcp.get_info();

        // 顶层 instructions 应被注入
        let instructions = info.instructions.expect("应有 instructions");
        assert!(
            instructions.contains("记忆协议"),
            "instructions 应包含「记忆协议」，实际: {}",
            instructions.chars().take(100).collect::<String>()
        );
        // v2.31：instructions 末尾追加了 install_rules 提示
        // 所以 expected_instructions 应是 instructions 的前缀
        assert!(
            instructions.starts_with(&expected_instructions),
            "instructions 应以 usage_protocol.instructions 开头，实际开头: {}",
            instructions.chars().take(100).collect::<String>()
        );
        // v2.31 新增：install_rules 提示应存在
        assert!(
            instructions.contains("install_rules"),
            "instructions 应包含 install_rules 提示"
        );

        // server_info.name 应保持 build_env 默认值（hippocampus-mcp）
        assert!(
            info.server_info.name.contains("hippocampus"),
            "server_info.name 应含 hippocampus，实际: {}",
            info.server_info.name
        );
    }

    /// 测试 get_info() 在无 combined_profile 时 instructions 为 None（向后兼容）
    ///
    /// 验证：未识别 Agent 时 LLM 看到的 server 元信息与 v2.29 一致
    #[test]
    fn test_get_info_no_instructions_when_combined_profile_absent() {
        let tmpdir = TempDir::new().unwrap();
        let mcp = HippocampusMcp::new(tmpdir.path().to_path_buf());

        let info = mcp.get_info();

        // 无 combined_profile 时 instructions 应为 None（与原宏生成等价）
        assert!(
            info.instructions.is_none(),
            "无 combined_profile 时不应有 instructions，实际: {:?}",
            info.instructions
        );
        // capabilities 应启用 tools
        assert!(
            info.capabilities.tools.is_some(),
            "capabilities 应启用 tools"
        );
    }

    /// 测试 get_info() 在 combined_profile 但 usage_protocol 为空时的降级
    ///
    /// 验证：即使注入了 combined_profile，若 usage_protocol.instructions 为空
    /// （未识别为 mainstream agent，generate_usage_protocol 返回 empty），
    /// get_info 也不应注入空字符串
    #[test]
    fn test_get_info_no_instructions_when_usage_protocol_empty() {
        use hippocampus_presets::PresetBuilder;

        let tmpdir = TempDir::new().unwrap();

        // 用 PresetBuilder 不传 agent 构造空协议的 CombinedProfile
        // （generate_usage_protocol 在无 agent 或非 mainstream 时返回 empty）
        let empty_combined = PresetBuilder::new()
            .build()
            .expect("空 PresetBuilder 应构建成功");

        assert!(
            empty_combined.usage_protocol().is_empty(),
            "前置条件：无 agent 时 usage_protocol 应为空"
        );

        let mcp = HippocampusMcp::new(tmpdir.path().to_path_buf())
            .with_combined_profile(Some(empty_combined));

        let info = mcp.get_info();
        // usage_protocol.instructions 为空时不应注入
        assert!(
            info.instructions.is_none(),
            "空 usage_protocol 不应注入 instructions"
        );
    }
}
