//! # MemoryCenter 归档核心引擎（v2.50 新增）
//!
//! 抽取 server `pre_compress` + `archive` handler 的核心归档逻辑，
//! 供 `memory-center-server` 和 `memory-center-sidecar` 共享，消除重复。
//!
//! ## 核心价值
//!
//! - **sidecar 直写存储**：sidecar 不再依赖 HTTP server 中转，直接调用 `ArchiveEngine` 写 LocalStorage
//! - **消除归档逻辑重复**：server 和 sidecar 共用同一套归档链路
//! - **组件复用**：LLM 组件（SummaryGenerator/ScenarioDetector/SessionSearchRouter）初始化逻辑共享
//!
//! ## 架构
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │               memory-center-archive-core                │
//! │                                                         │
//! │   ArchiveEngine                                         │
//! │   ├── pre_compress()  压缩前一次性完整归档              │
//! │   ├── archive()       日常归档                          │
//! │   └── health_check()  存储目录可写检查                  │
//! │                                                         │
//! │   组件构建（from_env）                                  │
//! │   ├── build_summary_generator()  LLM 摘要生成器         │
//! │   ├── build_scenario_detector()  场景识别器             │
//! │   └── build_session_search()     搜索索引路由器         │
//! └─────────────────────────────────────────────────────────┘
//!           ▲                              ▲
//!           │                              │
//!     ┌─────┴──────┐              ┌───────┴───────┐
//!     │   server   │              │   sidecar     │
//!     │ (HTTP API) │              │ (直写存储)    │
//!     └────────────┘              └───────────────┘
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use memory_center_core::archive::Archiver;
use memory_center_core::model::{
    apply_turn_defaults, ArchiveConfig, IndexHook, MessageTurn, TaskStateSnapshot,
};
use memory_center_core::retrieve::SummaryView;
use memory_center_core::storage::{LocalStorage, Storage};
use memory_center_search::SessionSearchRouter;

// ============================================================================
// 请求 / 响应结构（与 server handlers.rs 对齐，供 sidecar 复用）
// ============================================================================

/// 预设请求（与 server `PresetRequest` 对齐）
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct PresetRequest {
    pub agent: Option<String>,
    pub scenario: Option<String>,
    pub model: Option<String>,
    pub archive_threshold: Option<usize>,
    pub summary_template: Option<String>,
}

/// 任务状态快照请求（与 server `TaskStateSnapshotRequest` 对齐）
#[derive(Debug, Clone, serde::Deserialize)]
pub struct TaskStateSnapshotRequest {
    pub current_task: String,
    #[serde(default)]
    pub completed_steps: Vec<String>,
    #[serde(default)]
    pub in_progress_step: Option<String>,
    pub next_step: String,
}

/// pre_compress 结果（与 server 响应 JSON 对齐）
#[derive(Debug, Clone, serde::Serialize)]
pub struct PreCompressResult {
    pub hook_id: String,
    pub raw_context_path: String,
    pub parse_success: bool,
    pub parsed_turns_count: usize,
    pub archived_tokens: usize,
    pub estimated_total_tokens: usize,
    pub threshold: usize,
    pub threshold_ratio_percent: u64,
    pub suggestion: String,
    pub archived_at: String,
}

/// archive 结果（SummaryView + 搜索索引用的 turns_text）
#[derive(Debug, Clone)]
pub struct ArchiveResult {
    pub summary: SummaryView,
    /// 归档的 turns 文本（用于触发搜索索引）
    pub turns_text: String,
    /// 归档后的 IndexHook（用于触发搜索索引）
    pub hook: IndexHook,
}

// ============================================================================
// 错误类型
// ============================================================================

/// 归档错误
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("参数错误: {0}")]
    BadRequest(String),
    #[error("存储错误: {0}")]
    Storage(String),
    #[error("归档失败: {0}")]
    Archive(String),
    #[error("预设构建失败: {0}")]
    Preset(String),
}

// ============================================================================
// ArchiveEngine：归档核心引擎
// ============================================================================

/// 归档核心引擎（v2.50 新增）
///
/// 封装 server `pre_compress` + `archive` 的核心逻辑，供 server 和 sidecar 共享。
///
/// ## 组件注入
///
/// - `summary_generator`：LLM 摘要生成器（未注入时降级为启发式）
/// - `scenario_detector`：场景识别器（未注入时用 preset 原行为）
/// - `session_search`：搜索索引路由器（未注入时跳过索引）
///
/// ## 使用示例
///
/// ```no_run
/// use memory_center_archive_core::ArchiveEngine;
/// use std::path::PathBuf;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let engine = ArchiveEngine::new(PathBuf::from("./data"))
///     .with_summary_generator(/* ... */)
///     .with_session_search(/* ... */);
///
/// // 压缩前归档
/// let result = engine.pre_compress(
///     "session-001",
///     vec![/* turns */],
///     Some(50000),
///     Some("myproject"),
///     None,
///     None,
/// ).await?;
/// # Ok(())
/// # }
/// ```
pub struct ArchiveEngine {
    /// 存储根目录
    storage_root: PathBuf,
    /// 可选的 LLM 摘要生成器
    summary_generator: Option<Arc<dyn memory_center_core::generate::SummaryGenerator>>,
    /// 可选的场景识别器
    scenario_detector:
        Option<Arc<memory_center_presets::HybridScenarioDetector>>,
    /// 可选的搜索索引路由器
    session_search: Option<Arc<SessionSearchRouter>>,
}

impl ArchiveEngine {
    /// 创建新的归档引擎
    pub fn new(storage_root: PathBuf) -> Self {
        Self {
            storage_root,
            summary_generator: None,
            scenario_detector: None,
            session_search: None,
        }
    }

    /// 注入 LLM 摘要生成器
    pub fn with_summary_generator(
        mut self,
        gen: Arc<dyn memory_center_core::generate::SummaryGenerator>,
    ) -> Self {
        self.summary_generator = Some(gen);
        self
    }

    /// 注入场景识别器
    pub fn with_scenario_detector(
        mut self,
        det: Arc<memory_center_presets::HybridScenarioDetector>,
    ) -> Self {
        self.scenario_detector = Some(det);
        self
    }

    /// 注入搜索索引路由器
    pub fn with_session_search(mut self, router: Arc<SessionSearchRouter>) -> Self {
        self.session_search = Some(router);
        self
    }

    /// 获取存储根目录
    pub fn storage_root(&self) -> &std::path::Path {
        &self.storage_root
    }

    /// 获取摘要生成器引用
    pub fn summary_generator(&self) -> Option<&Arc<dyn memory_center_core::generate::SummaryGenerator>> {
        self.summary_generator.as_ref()
    }

    /// 获取搜索路由器引用
    pub fn session_search(&self) -> Option<&Arc<SessionSearchRouter>> {
        self.session_search.as_ref()
    }

    /// 健康检查：存储目录可写
    pub fn health_check(&self) -> Result<bool, ArchiveError> {
        if !self.storage_root.exists() {
            std::fs::create_dir_all(&self.storage_root).map_err(|e| {
                ArchiveError::Storage(format!("创建存储目录失败: {e}"))
            })?;
        }
        // 测试可写：尝试创建 .healthcheck 临时文件
        let test_file = self.storage_root.join(".archive_engine_healthcheck");
        std::fs::write(&test_file, b"ok").map_err(|e| {
            ArchiveError::Storage(format!("存储目录不可写: {e}"))
        })?;
        let _ = std::fs::remove_file(&test_file);
        Ok(true)
    }

    /// 创建 Storage 实例（每次调用创建，无内存缓存）
    fn create_storage(&self) -> Arc<dyn Storage> {
        Arc::new(LocalStorage::new(self.storage_root.clone()))
    }

    // ========================================================================
    // pre_compress：压缩前一次性完整归档
    // ========================================================================

    /// 压缩前一次性完整归档（抽取自 server `pre_compress` handler）
    ///
    /// 双轨处理：
    /// 1. raw_context 永远先存（失败才阻塞返回错误）
    /// 2. 尝试解析 turns：成功复用 Archiver 归档；失败仅存 raw_context
    ///
    /// # 参数
    ///
    /// - `session_id`: 会话 ID
    /// - `turns`: 结构化轮次列表（保留 tool_calls/thinking）
    /// - `estimated_tokens`: 客户端估算的 token 数（None 时服务端按内容长度 / 3 估算）
    /// - `project_id`: 项目 ID（可选，影响存储路径）
    /// - `preset`: 预设配置（可选）
    /// - `task_state_snapshot`: 任务状态快照（可选，持久化供下次 prompt 校准）
    pub async fn pre_compress(
        &self,
        session_id: &str,
        turns: Vec<MessageTurn>,
        estimated_tokens: Option<usize>,
        project_id: Option<&str>,
        preset: Option<&PresetRequest>,
        task_state_snapshot: Option<&TaskStateSnapshotRequest>,
    ) -> Result<PreCompressResult, ArchiveError> {
        if turns.is_empty() {
            return Err(ArchiveError::BadRequest(
                "turns 不能为空".to_string(),
            ));
        }

        // 1. 生成 hook_id（提前生成，用于 raw_context 文件命名）
        let hook_id = uuid::Uuid::new_v4().to_string();

        // 2. 确定 raw_context 内容：用 turns 的 JSON 序列化
        let raw_context_content = serde_json::to_string_pretty(&turns)
            .unwrap_or_else(|_| "<turns 序列化失败>".to_string());

        // 3. 写 raw_context（spec 第七章：永远先存，失败才阻塞返回错误）
        let storage = self.create_storage();
        let raw_context_path = storage
            .write_raw_context(session_id, &hook_id, &raw_context_content)
            .await
            .map_err(|e| {
                ArchiveError::Storage(format!(
                    "写 raw_context 失败: {e}\n\n\
                     raw_context 是 pre_compress 的核心兜底，失败则阻塞返回。\
                     后续解析/归档步骤不会执行。"
                ))
            })?;

        // 4. 估算 token
        let estimated_total_tokens =
            estimated_tokens.unwrap_or_else(|| raw_context_content.len() / 3);

        // 5. 路径 A：turns 直接用（结构化，保留 tool_calls/thinking）
        let parsed_turns = turns;
        let parse_source = "structured";

        // 6. 归档 turns
        let (archived_tokens, parsed_turns_count, parse_success) = if parsed_turns.is_empty() {
            tracing::info!(
                session = %session_id,
                hook_id = %hook_id,
                parse_source,
                "解析得到空 turns，仅存 raw_context"
            );
            (estimated_total_tokens, 0, false)
        } else {
            let turns_count = parsed_turns.len();
            match self
                .archive_parsed_turns_in_pre_compress(
                    session_id,
                    project_id,
                    parsed_turns,
                    preset,
                    task_state_snapshot,
                    &hook_id,
                    &raw_context_path,
                )
                .await
            {
                Ok(tokens) => (tokens, turns_count, true),
                Err(e) => {
                    tracing::warn!(
                        session = %session_id,
                        hook_id = %hook_id,
                        error = %e,
                        "Archiver 归档失败，降级为仅 raw_context（parse_success=false）"
                    );
                    (estimated_total_tokens, 0, false)
                }
            }
        };

        // 7. 计算 threshold / ratio / suggestion
        let threshold = get_archive_threshold(preset);
        let ratio = if threshold > 0 {
            (archived_tokens as f64 / threshold as f64 * 100.0).round() as u64
        } else {
            0
        };
        let suggestion = if parse_success {
            format!(
                "压缩前归档完成，共 {} 轮，原始 ~{} tokens（阈值 {}，当前 {}%）。可安全压缩。",
                parsed_turns_count, estimated_total_tokens, threshold, ratio
            )
        } else {
            format!(
                "压缩前归档完成（仅 raw_context，解析失败），原始 ~{} tokens（阈值 {}，当前 {}%）。可安全压缩。",
                estimated_total_tokens, threshold, ratio
            )
        };

        tracing::info!(
            session = %session_id,
            hook_id = %hook_id,
            parse_success,
            parsed_turns_count,
            archived_tokens,
            threshold,
            ratio_percent = ratio,
            "pre_compress 完成"
        );

        Ok(PreCompressResult {
            hook_id,
            raw_context_path,
            parse_success,
            parsed_turns_count,
            archived_tokens,
            estimated_total_tokens,
            threshold,
            threshold_ratio_percent: ratio,
            suggestion,
            archived_at: chrono::Utc::now().to_rfc3339(),
        })
    }

    /// pre_compress 内部辅助：解析成功后复用 Archiver 归档 turns
    ///
    /// 提取自 server `archive_parsed_turns_in_pre_compress`。
    /// 场景识别 + 构建 Archiver + 应用 preset + 注入 summary_generator
    /// + 写 task_state_snapshot + 触发搜索索引。
    async fn archive_parsed_turns_in_pre_compress(
        &self,
        session_id: &str,
        project_id: Option<&str>,
        turns: Vec<MessageTurn>,
        preset: Option<&PresetRequest>,
        task_state_snapshot: Option<&TaskStateSnapshotRequest>,
        hook_id: &str,
        raw_context_path: &str,
    ) -> Result<usize, String> {
        // 1. 场景识别（仅首次 archive 时识别，后续读 session_meta 跳过）
        let effective_scenario_name: Option<String> = if let Some(detector) = &self.scenario_detector
        {
            let family = preset
                .and_then(|p| p.agent.as_deref())
                .and_then(memory_center_agents::AgentFamily::from_str)
                .unwrap_or_else(|| {
                    memory_center_agents::AgentFamily::Custom("unknown".to_string())
                });

            let user_explicit = preset.and_then(|p| p.scenario.as_deref());

            let storage_for_detect = self.create_storage();
            let scenario = memory_center_presets::resolve_effective_scenario(
                storage_for_detect.as_ref(),
                session_id,
                user_explicit,
                &family,
                detector.as_ref(),
                &turns,
            )
            .await;
            Some(memory_center_presets::scenario_to_str(&scenario))
        } else {
            preset.and_then(|p| p.scenario.clone())
        };

        // 2. 构建 preset（archive_threshold + summary_template）
        let (archive_threshold, summary_template) = if let Some(preset_req) = preset {
            let combined = build_combined_from_request(preset_req)
                .map_err(|e| format!("预设构建失败: {e}"))?;
            (
                Some(combined.archive_threshold()),
                Some(combined.summary_template().to_string()),
            )
        } else if let Some(scenario_name) = effective_scenario_name {
            let combined = memory_center_presets::build_from_strings(
                None,
                Some(&scenario_name),
                None,
                None,
                None,
            )
            .map_err(|e| format!("识别场景构建预设失败: {e}"))?;
            (
                Some(combined.archive_threshold()),
                Some(combined.summary_template().to_string()),
            )
        } else {
            (None, None)
        };

        // 3. 构建 Archiver
        let storage = self.create_storage();
        let config = if let Some(threshold) = archive_threshold {
            ArchiveConfig {
                token_threshold: threshold,
                force_truncate_limit: threshold * 3 / 2,
                wait_for_turn_completion: true,
            }
        } else {
            ArchiveConfig::default()
        };
        let storage_for_snapshot = storage.clone();
        let mut archiver = Archiver::new(
            config,
            storage,
            session_id,
            project_id.map(|s| s.to_string()),
        );

        // 4. 注入 summary_generator
        if let Some(gen) = &self.summary_generator {
            archiver = archiver.with_summary_generator(gen.clone());
        }

        // 5. 注入 summary_template
        if let Some(tpl) = summary_template {
            archiver = archiver.with_summary_template_override(tpl);
        }

        // 6. 注入覆盖（hook_id 一致性 + archive_reason + raw_context_path）
        archiver = archiver
            .with_override_hook_id(hook_id)
            .with_archive_reason("pre_compress")
            .with_raw_context_path(raw_context_path);

        // 7. 提取 turns 文本用于索引（在 move 消费前 borrow）
        let turns_text = memory_center_search::extract_turns_text(&turns);

        // 8. 对每个 turn 应用默认值补全（推断 tags + 估算 token_count）
        for mut turn in turns {
            apply_turn_defaults(&mut turn);
            archiver.push_turn(turn);
        }

        let (_, hook) = archiver
            .archive()
            .await
            .map_err(|e| format!("归档失败: {e}"))?;

        // 9. 归档后触发搜索索引（按 session 隔离）
        if let Some(router) = &self.session_search {
            router.index_hook(session_id, &hook, &turns_text).await;
        }

        // 10. 写 task_state_snapshot（若有，失败不影响归档结果）
        if let Some(snap) = task_state_snapshot {
            let snapshot = TaskStateSnapshot {
                current_task: snap.current_task.clone(),
                completed_steps: snap.completed_steps.clone(),
                in_progress_step: snap.in_progress_step.clone(),
                next_step: snap.next_step.clone(),
                snapshot_at: chrono::Utc::now(),
            };
            if let Err(e) = storage_for_snapshot
                .write_session_state(session_id, &snapshot)
                .await
            {
                tracing::warn!(
                    session = %session_id,
                    error = %e,
                    "task_state_snapshot 持久化失败（不影响归档结果）"
                );
            }
        }

        Ok(hook.token_count)
    }

    // ========================================================================
    // archive：日常归档
    // ========================================================================

    /// 日常归档（抽取自 server `archive` handler）
    ///
    /// 归档一批轮次为记忆文件，生成索引钩子。
    pub async fn archive(
        &self,
        session_id: &str,
        turns: Vec<MessageTurn>,
        project_id: Option<&str>,
        preset: Option<&PresetRequest>,
    ) -> Result<ArchiveResult, ArchiveError> {
        if turns.is_empty() {
            return Err(ArchiveError::BadRequest(
                "turns 不能为空".to_string(),
            ));
        }

        // 1. 场景识别
        let effective_scenario_name: Option<String> = if let Some(detector) = &self.scenario_detector
        {
            let family = preset
                .and_then(|p| p.agent.as_deref())
                .and_then(memory_center_agents::AgentFamily::from_str)
                .unwrap_or_else(|| {
                    memory_center_agents::AgentFamily::Custom("unknown".to_string())
                });

            let user_explicit = preset.and_then(|p| p.scenario.as_deref());

            let storage_for_detect = self.create_storage();
            let scenario = memory_center_presets::resolve_effective_scenario(
                storage_for_detect.as_ref(),
                session_id,
                user_explicit,
                &family,
                detector.as_ref(),
                &turns,
            )
            .await;
            Some(memory_center_presets::scenario_to_str(&scenario))
        } else {
            preset.and_then(|p| p.scenario.clone())
        };

        // 2. 构建 preset
        let (archive_threshold, summary_template) = if let Some(preset_req) = preset {
            let combined = build_combined_from_request(preset_req)
                .map_err(|e| ArchiveError::Preset(e))?;
            (
                Some(combined.archive_threshold()),
                Some(combined.summary_template().to_string()),
            )
        } else if let Some(scenario_name) = effective_scenario_name {
            let combined = memory_center_presets::build_from_strings(
                None,
                Some(&scenario_name),
                None,
                None,
                None,
            )
            .map_err(|e| ArchiveError::Preset(format!("识别场景构建预设失败: {e}")))?;
            (
                Some(combined.archive_threshold()),
                Some(combined.summary_template().to_string()),
            )
        } else {
            (None, None)
        };

        // 3. 构建 Archiver
        let storage = self.create_storage();
        let config = if let Some(threshold) = archive_threshold {
            ArchiveConfig {
                token_threshold: threshold,
                force_truncate_limit: threshold * 3 / 2,
                wait_for_turn_completion: true,
            }
        } else {
            ArchiveConfig::default()
        };
        let mut archiver = Archiver::new(
            config,
            storage,
            session_id,
            project_id.map(|s| s.to_string()),
        );

        // 4. 注入 summary_generator
        if let Some(gen) = &self.summary_generator {
            archiver = archiver.with_summary_generator(gen.clone());
        }

        // 5. 注入 summary_template
        if let Some(tpl) = summary_template {
            archiver = archiver.with_summary_template_override(tpl);
        }

        // 6. 提取 turns 文本用于索引
        let turns_text = memory_center_search::extract_turns_text(&turns);

        // 7. apply_turn_defaults + push
        for mut turn in turns {
            apply_turn_defaults(&mut turn);
            archiver.push_turn(turn);
        }

        let (_, hook) = archiver.archive().await.map_err(|e| {
            ArchiveError::Archive(format!("归档失败: {e}"))
        })?;
        let summary = SummaryView::from(&hook);

        // 8. 触发搜索索引
        if let Some(router) = &self.session_search {
            router.index_hook(session_id, &hook, &turns_text).await;
        }

        tracing::info!(
            session = %session_id,
            hook_id = %summary.hook_id,
            tokens = summary.token_count,
            has_preset = archive_threshold.is_some(),
            "归档成功"
        );

        Ok(ArchiveResult {
            summary,
            turns_text,
            hook,
        })
    }
}

// ============================================================================
// 辅助函数
// ============================================================================

/// 获取当前 archive 阈值
///
/// 优先级：
/// 1. preset.archive_threshold（用户显式覆盖，最高优先级）
/// 2. preset 构建的 CombinedProfile.archive_threshold()
/// 3. 默认 120000
fn get_archive_threshold(preset: Option<&PresetRequest>) -> usize {
    if let Some(preset_req) = preset {
        if let Some(t) = preset_req.archive_threshold {
            return t;
        }
        if let Ok(combined) = build_combined_from_request(preset_req) {
            return combined.archive_threshold();
        }
    }
    120000
}

/// 从 PresetRequest 构建 CombinedProfile
///
/// 抽取自 server `presets::build_combined_from_request`，
/// 供 archive-core 内部复用（不依赖 server 模块）。
fn build_combined_from_request(
    req: &PresetRequest,
) -> Result<memory_center_presets::CombinedProfile, String> {
    memory_center_presets::build_from_strings(
        req.agent.as_deref(),
        req.scenario.as_deref(),
        req.model.as_deref(),
        req.archive_threshold,
        req.summary_template.as_deref(),
    )
    .map_err(|e| e.to_string())
}

// ============================================================================
// 组件构建函数（从环境变量构造，供 sidecar 复用）
// ============================================================================

/// 从环境变量构造 LLM 摘要生成器
///
/// 未配置 `MEMORY_CENTER_GENERATOR_API_URL` 时返回 None（降级为启发式）。
pub fn build_summary_generator(
) -> Option<Arc<dyn memory_center_core::generate::SummaryGenerator>> {
    use memory_center_core::generate::LlmGeneratorConfig;
    use memory_center_llm::HttpSummaryGenerator;

    let config = match LlmGeneratorConfig::from_env() {
        Some(config) => config,
        None => {
            tracing::info!(
                "摘要生成器：未配置 LLM API（MEMORY_CENTER_GENERATOR_API_URL），使用启发式 Summary::from_title"
            );
            return None;
        }
    };

    tracing::info!(
        api_url = %config.api_url,
        model = %config.model,
        max_tokens = config.max_tokens,
        "摘要生成器：LLM API 已配置，启用 HttpSummaryGenerator"
    );

    Some(Arc::new(HttpSummaryGenerator::new(config)))
}

/// 从环境变量构造场景识别器
pub fn build_scenario_detector() -> Arc<memory_center_presets::HybridScenarioDetector> {
    use memory_center_llm::LlmDetectorConfig;
    use memory_center_presets::scenario_detect::HttpScenarioDetector;

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

    Arc::new(memory_center_presets::HybridScenarioDetector::new(llm_config))
}

/// 从环境变量构造 SessionSearchRouter
///
/// 未配置 `MEMORY_CENTER_EMBEDDER_API_URL` 时降级为仅关键词检索。
pub fn build_session_search(
    storage_root: &std::path::Path,
) -> Option<Arc<SessionSearchRouter>> {
    use memory_center_core::semantic::Embedder;
    use memory_center_llm::{EmbedderConfig, HttpEmbedder};

    let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(storage_root.to_path_buf()));

    let embedder_config = match EmbedderConfig::from_env() {
        Some(config) => config,
        None => {
            tracing::info!(
                "语义检索：未配置 Embedder API，降级为仅关键词检索（KeywordOnlyRetriever + storage 懒重建）"
            );
            let router = SessionSearchRouter::new(None, 0).with_storage(storage);
            return Some(Arc::new(router));
        }
    };

    let dim = embedder_config.dim;
    tracing::info!(
        api_url = %embedder_config.api_url,
        model = %embedder_config.model,
        dim,
        "语义检索：Embedder 已配置，启用 session 级混合检索"
    );

    let embedder: Arc<dyn Embedder> = Arc::new(HttpEmbedder::new(embedder_config));
    let router = SessionSearchRouter::new(Some(embedder), dim).with_storage(storage);
    Some(Arc::new(router))
}

/// 从环境变量构造完整 ArchiveEngine（便捷函数）
///
/// 自动注入 SummaryGenerator + ScenarioDetector + SessionSearchRouter。
/// 未配置 LLM API 时各组件降级。
pub fn build_engine_from_env(storage_root: PathBuf) -> ArchiveEngine {
    let summary_generator = build_summary_generator();
    let scenario_detector = build_scenario_detector();
    let session_search = build_session_search(&storage_root);

    let mut engine = ArchiveEngine::new(storage_root);
    if let Some(gen) = summary_generator {
        engine = engine.with_summary_generator(gen);
    }
    engine = engine.with_scenario_detector(scenario_detector);
    if let Some(router) = session_search {
        engine = engine.with_session_search(router);
    }
    engine
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_engine_new() {
        let engine = ArchiveEngine::new(PathBuf::from("/tmp/test"));
        assert_eq!(engine.storage_root(), std::path::Path::new("/tmp/test"));
        assert!(engine.summary_generator().is_none());
        assert!(engine.session_search().is_none());
    }

    #[test]
    fn test_get_archive_threshold_default() {
        let threshold = get_archive_threshold(None);
        assert_eq!(threshold, 120000);
    }

    #[test]
    fn test_get_archive_threshold_user_override() {
        let preset = PresetRequest {
            archive_threshold: Some(50000),
            ..Default::default()
        };
        let threshold = get_archive_threshold(Some(&preset));
        assert_eq!(threshold, 50000);
    }

    #[test]
    fn test_health_check_creates_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = ArchiveEngine::new(tmp.path().to_path_buf());
        assert!(engine.health_check().unwrap());
    }
}
