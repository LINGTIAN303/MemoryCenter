//! # 启动期组件构造（v2.36 抽离）
//!
//! 把 `main.rs` 中的 `build_*` 函数抽离为公共 API，供两个入口复用：
//!
//! - `hippocampus-mcp` bin（stdio 传输）
//! - `hippocampus-server` bin（HTTP 传输，挂载到 axum Router）
//!
//! ## 设计原则
//!
//! - **零状态**：所有函数都是纯函数，从环境变量读取配置，返回构造好的组件
//! - **失败降级**：未配置 LLM API 时降级为启发式实现，不阻塞启动
//! - **降级状态回传**：每个 build_* 函数返回 `(组件, 降级状态字符串)`，
//!   供 `RuntimeStatus` 记录到 `get_config` 工具的响应中
//! - **复用 storage**：`build_session_search` 接收 `storage_root` 参数，
//!   MCP 端注入 storage 引用支持懒重建（进程重启后索引丢失时从 storage 重建）

use std::path::Path;
use std::sync::Arc;

use hippocampus_core::conflict::{ConflictDetector, HybridDetector};
use hippocampus_core::generate::SummaryGenerator;
use hippocampus_core::heuristic::HeuristicDetector;
use hippocampus_core::storage::{LocalStorage, Storage};
use hippocampus_llm::{HttpLlmDetector, LlmDetectorConfig};
use hippocampus_presets::{
    detect_agent_client, resolve_scenario_name, scenario_from_str, CombinedProfile, PresetBuilder,
};
use hippocampus_scenarios::ScenarioProfile;
use hippocampus_agents::AgentProfile;

/// 从环境变量构造冲突检测器（v2.11，v2.13 简化，v2.32 返回降级状态）
///
/// - 配置了 `HIPPOCAMPUS_DETECTOR_API_URL` + `API_KEY`：
///   返回 `(HybridDetector, "hybrid")`（串联 Heuristic + LLM，合并两份报告）
/// - 未配置：返回 `(HeuristicDetector, "heuristic")`（启发式纯算法，无 LLM 依赖）
///
/// ## 返回
///
/// - `Arc<dyn ConflictDetector>`：检测器实例
/// - `&'static str`：降级状态字符串（`"hybrid"` / `"heuristic"`），供 `RuntimeStatus` 记录
pub fn build_conflict_detector() -> (Arc<dyn ConflictDetector>, &'static str) {
    // v2.13：使用 LlmDetectorConfig::from_env() 统一环境变量读取
    let config = match LlmDetectorConfig::from_env() {
        Some(config) => config,
        None => {
            tracing::info!(
                "冲突检测器：未配置 LLM API，使用 HeuristicDetector（启发式纯算法，三维度检测）"
            );
            return (Arc::new(HeuristicDetector::new()), "heuristic");
        }
    };

    tracing::info!(
        api_url = %config.api_url,
        model = %config.model,
        max_tokens = config.max_tokens,
        "冲突检测器：LLM API 已配置，使用 HybridDetector（串联 Heuristic + LLM，失败时降级保留启发式结果）"
    );

    // v2.11：串联 Heuristic + LLM，合并两份报告
    let heuristic: Arc<dyn ConflictDetector> = Arc::new(HeuristicDetector::new());
    let llm: Arc<dyn ConflictDetector> = Arc::new(HttpLlmDetector::new(config));
    (Arc::new(HybridDetector::new(heuristic, llm)), "hybrid")
}

/// 从环境变量构造 SessionSearchRouter（v2.18 新增，v2.32 返回降级状态）
///
/// 与 server 端 `build_session_search` 的关键差异：
/// **MCP 端必须注入 storage 引用**（用 `with_storage`），因为 MCP 进程是
/// 短生命周期子进程，每次启动后内存索引为空，必须从 storage 懒重建。
///
/// - 配置了 `HIPPOCAMPUS_EMBEDDER_API_URL` + `API_KEY`：
///   返回 `(Some(router), "hybrid", Some(dim))`（混合检索）
/// - 未配置：返回 `(Some(router), "keyword_only", None)`（降级，但仍带 storage 懒重建）
///
/// ## 返回
///
/// - `Option<Arc<SessionSearchRouter>>`：路由器实例（当前总返回 Some）
/// - `&'static str`：降级状态字符串（`"hybrid"` / `"keyword_only"`）
/// - `Option<usize>`：Embedder 向量维度（仅 hybrid 模式有值）
pub fn build_session_search(
    storage_root: &Path,
) -> (
    Option<Arc<hippocampus_search::SessionSearchRouter>>,
    &'static str,
    Option<usize>,
) {
    use hippocampus_core::semantic::Embedder;
    use hippocampus_llm::{EmbedderConfig, HttpEmbedder};
    use hippocampus_search::SessionSearchRouter;

    // 构造 storage 后端（供 SessionSearchRouter 重建索引用）
    let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(storage_root.to_path_buf()));

    // v2.13：使用 EmbedderConfig::from_env() 统一环境变量读取
    let embedder_config = match EmbedderConfig::from_env() {
        Some(config) => config,
        None => {
            // 降级模式：仅关键词检索 + storage 懒重建
            tracing::info!(
                "语义检索：未配置 Embedder API，降级为仅关键词检索（KeywordOnlyRetriever，session 级隔离 + storage 懒重建）"
            );
            let router = SessionSearchRouter::new(None, 0).with_storage(storage);
            return (Some(Arc::new(router)), "keyword_only", None);
        }
    };

    let dim = embedder_config.dim;
    tracing::info!(
        api_url = %embedder_config.api_url,
        model = %embedder_config.model,
        dim,
        "语义检索：Embedder 已配置，启用 session 级混合检索（HybridRetriever + storage 懒重建 + embed_batch 批量优化）"
    );

    let embedder: Arc<dyn Embedder> = Arc::new(HttpEmbedder::new(embedder_config));
    let router = SessionSearchRouter::new(Some(embedder), dim).with_storage(storage);
    (Some(Arc::new(router)), "hybrid", Some(dim))
}

/// 从环境变量构造 LLM 摘要生成器（v2.21 批次 8c，v2.32 返回降级状态）
///
/// | 环境变量 | 说明 | 默认值 |
/// |---------|------|--------|
/// | `HIPPOCAMPUS_GENERATOR_API_URL` | LLM API 地址（OpenAI 兼容 `/v1/chat/completions`） | 空（降级为启发式） |
/// | `HIPPOCAMPUS_GENERATOR_API_KEY` | API Key | 空 |
/// | `HIPPOCAMPUS_GENERATOR_MODEL` | 模型名 | `gpt-5.5-instant` |
/// | `HIPPOCAMPUS_GENERATOR_TIMEOUT` | 超时秒数 | `60` |
/// | `HIPPOCAMPUS_GENERATOR_MAX_TOKENS` | LLM 最大输出 token | `500` |
///
/// ## 返回
///
/// - `Option<Arc<dyn SummaryGenerator>>`：生成器实例（None 表示使用启发式）
/// - `&'static str`：降级状态字符串（`"llm"` / `"heuristic"`）
pub fn build_summary_generator() -> (
    Option<Arc<dyn SummaryGenerator>>,
    &'static str,
) {
    use hippocampus_core::generate::LlmGeneratorConfig;
    use hippocampus_llm::HttpSummaryGenerator;

    let config = match LlmGeneratorConfig::from_env() {
        Some(config) => config,
        None => {
            tracing::info!(
                "摘要生成器：未配置 LLM API（HIPPOCAMPUS_GENERATOR_API_URL），使用启发式 Summary::from_title"
            );
            return (None, "heuristic");
        }
    };

    tracing::info!(
        api_url = %config.api_url,
        model = %config.model,
        max_tokens = config.max_tokens,
        "摘要生成器：LLM API 已配置，启用 HttpSummaryGenerator（归档时生成结构化摘要）"
    );

    (Some(Arc::new(HttpSummaryGenerator::new(config))), "llm")
}

/// 从环境变量构造场景识别器（v2.33 新增）
///
/// 复用 `HIPPOCAMPUS_DETECTOR_*` 环境变量（与冲突检测器、摘要生成器共享 LLM 配置）：
///
/// - 配置了 `HIPPOCAMPUS_DETECTOR_API_URL` + `API_KEY`：
///   返回关键词 + LLM 兜底的 HybridScenarioDetector
/// - 未配置：返回仅关键词模式的 HybridScenarioDetector
pub fn build_scenario_detector() -> Arc<hippocampus_presets::HybridScenarioDetector> {
    use hippocampus_llm::LlmDetectorConfig;
    use hippocampus_presets::scenario_detect::HttpScenarioDetector;

    let llm_config = match LlmDetectorConfig::from_env() {
        Some(config) => {
            tracing::info!(
                api_url = %config.api_url,
                model = %config.model,
                "场景识别器：LLM API 已配置，启用关键词 + LLM 兜底"
            );
            Some(Arc::new(HttpScenarioDetector::new(config)))
        }
        None => {
            tracing::info!(
                "场景识别器：未配置 LLM API，仅用关键词规则识别（7 场景 × 15 关键词）"
            );
            None
        }
    };

    Arc::new(hippocampus_presets::HybridScenarioDetector::new(llm_config))
}

/// 启动时识别 Agent 客户端 + 构建 CombinedProfile（v2.30 新增）
///
/// 执行流程：
/// 1. 调用 `detect_agent_client(None)` 进行 3 层信号融合识别
///    （Layer 1 显式 env > Layer 2 MCP ClientInfo > Layer 3 父进程/env 前缀 > Layer 4 降级）
/// 2. 若识别为 mainstream family（ClaudeCode/Cursor/Trae/Codex）：
///    - 按 family 推导 scenario（HIPPOCAMPUS_PRESET_SCENARIO 优先，否则 coding/daily）
///    - 调用 PresetBuilder 构建 CombinedProfile（含 usage_protocol）
/// 3. 若识别为 Custom/降级：返回 None（tool 行为与 v2.29 一致）
///
/// ## 降级策略
///
/// - PresetBuilder::build 失败：日志警告，返回 None（不阻塞启动）
/// - 识别为 Custom：返回 None（向后兼容）
///
/// ## 注入路径（v2.30 1.5 + 1.8 已完成）
///
/// - `usage_protocol.instructions` → MCP `InitializeResult.instructions` 顶层字段
///   （由 `HippocampusMcp::get_info()` 注入，客户端把它注入 LLM system prompt）
/// - 不写入 SERVER_METADATA.json 文件（避免多 MCP 进程并发写入冲突）
/// - debug 级日志输出完整 instructions，方便调试（`RUST_LOG=debug` 可查看）
///
/// ## 日志输出
///
/// 启动时输出识别结果 + 应用的 preset 摘要：
/// ```text
/// Agent 客户端识别：family=Trae, source=EnvVarPrefix
/// 应用预设：scenario=Coding, archive_threshold=400000, session_prefix=trae
/// 行为契约生成完成：usage_protocol 已注入 MCP InitializeResult.instructions（LLM 启动即看到）
/// ```
pub fn build_combined_profile() -> Option<CombinedProfile> {
    let detected = detect_agent_client(None);

    tracing::info!(
        family = ?detected.family,
        source = ?detected.source,
        "Agent 客户端识别：3 层信号融合完成"
    );

    // 非 mainstream agent：降级，不构建 CombinedProfile
    if !detected.family.is_mainstream() {
        tracing::info!(
            family = ?detected.family,
            "未识别为主流 Agent（ClaudeCode/Cursor/Trae/Codex），跳过 preset 注入（向后兼容 v2.29）"
        );
        return None;
    }

    // 推导 scenario（环境变量 HIPPOCAMPUS_PRESET_SCENARIO 优先）
    let scenario_str = resolve_scenario_name(&detected.family);
    let scenario = scenario_from_str(&scenario_str);

    tracing::info!(
        family = ?detected.family,
        scenario = %scenario_str,
        "应用预设：按 Agent family 推导 scenario"
    );

    // 构建 CombinedProfile（Agent + Scenario，联动推导 Window）
    let agent_profile = AgentProfile::from_family(detected.family);
    let scenario_profile = ScenarioProfile::from_scenario(scenario);

    match PresetBuilder::new()
        .with_agent(agent_profile)
        .with_scenario(scenario_profile)
        .build()
    {
        Ok(combined) => {
            let protocol = combined.usage_protocol();
            tracing::info!(
                archive_threshold = combined.archive_threshold(),
                session_prefix = ?combined.session_prefix(),
                instructions_len = protocol.instructions.len(),
                trigger_rules_count = protocol.trigger_rules.len(),
                "行为契约生成完成：usage_protocol 已注入 MCP InitializeResult.instructions（LLM 启动即看到）"
            );
            // v2.30 1.8：debug 级输出完整 instructions，方便调试和验证注入内容
            // 生产环境 RUST_LOG=info 不会输出，RUST_LOG=debug 可查看完整行为契约
            tracing::debug!(
                instructions = %protocol.instructions,
                session_id_pattern = %protocol.session_id_pattern,
                trigger_rules = ?protocol
                    .trigger_rules
                    .iter()
                    .map(|r| format!("{}/{}", r.condition, r.tool))
                    .collect::<Vec<_>>(),
                "完整 usage_protocol 内容（debug 级）"
            );
            Some(combined)
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "CombinedProfile 构建失败：跳过 preset 注入（不阻塞启动，tool 行为与 v2.29 一致）"
            );
            None
        }
    }
}
