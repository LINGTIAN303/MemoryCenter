//! # API 端点处理器
//!
//! 5 个核心端点的 handler 实现。

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::AppError;
use crate::AppState;
use hippocampus_core::archive::Archiver;
use hippocampus_core::compact::Compactor;
use hippocampus_core::model::{ArchiveConfig, MemoryFile, MessageTurn};
use hippocampus_core::retrieve::{Retriever, SummaryView};
use hippocampus_core::score::DefaultScorer;
use hippocampus_core::storage::{LocalStorage, Storage};

// ============================================================================
// 请求 / 响应结构
// ============================================================================

/// archive 请求体
#[derive(Deserialize)]
pub struct ArchiveRequest {
    /// 待归档的轮次列表
    pub turns: Vec<MessageTurn>,
    /// 项目 ID（可选，影响存储路径）
    pub project_id: Option<String>,
    /// 预设配置（v2.29，可选）
    ///
    /// 传入后服务端即时构建 CombinedProfile，应用：
    /// - `archive_threshold` 覆盖默认 ArchiveConfig.token_threshold
    /// - `summary_template` 通过 `with_summary_template_override` 注入
    ///
    /// 未传入时保持原行为（`ArchiveConfig::default()` + 内部模板）。
    pub preset: Option<PresetRequest>,
}

/// 预设请求体（archive 内联参数，v2.29）
///
/// 所有字段可选，未提供的字段使用默认值或联动推导。
/// 与 `POST /api/v1/presets/build` 的请求体结构一致。
#[derive(Deserialize, Default)]
pub struct PresetRequest {
    /// Agent display_name（如 "Claude Code"）
    pub agent: Option<String>,
    /// Scenario 名称（大小写不敏感）
    pub scenario: Option<String>,
    /// ModelVariant 名称
    pub model: Option<String>,
    /// 用户覆盖：归档阈值
    pub archive_threshold: Option<usize>,
    /// 用户覆盖：摘要模板（需含 {conversation}）
    pub summary_template: Option<String>,
}

/// 任务状态快照请求体（v2.34 pre_compress 新增）
///
/// 与 MCP 端 `TaskStateSnapshotParams` 字段对等。
/// 用于压缩前持久化任务状态，下次 prompt 时返回，
/// 供 LLM 校准 Trae Summary 第8章节 Current Work。
#[derive(Deserialize)]
pub struct TaskStateSnapshotRequest {
    /// 当前任务名称（如 "批次A-数据完整性修复"）
    pub current_task: String,
    /// 已完成步骤列表
    #[serde(default)]
    pub completed_steps: Vec<String>,
    /// 进行中步骤（如果有，表示被压缩打断的任务）
    #[serde(default)]
    pub in_progress_step: Option<String>,
    /// 下一建议步骤
    pub next_step: String,
}

/// pre_compress 请求体（v2.34 新增）
///
/// 与 archive 的 `ArchiveRequest` 字段集对等，但用 `full_context`（完整字符串）
/// 替代 `turns`（结构化轮次），实现压缩前一次性完整归档。
#[derive(Deserialize)]
pub struct PreCompressRequest {
    /// 完整上下文字符串
    ///
    /// 客户端 dump 整个对话或 LLM 拼接关键内容。
    /// 支持 JSON 数组（`[{user_message, llm_message}]`）或 `User:`/`Assistant:` 分隔符格式，
    /// 无法识别时仅存 raw_context 不阻塞（parse_success=false）。
    pub full_context: String,
    /// 客户端估算的原始 token 数（可选）
    ///
    /// 未传时服务端按 `full_context.len() / 3` 估算。
    #[serde(default)]
    pub estimated_tokens: Option<usize>,
    /// 预设配置（可选，与 archive 的 PresetRequest 一致）
    #[serde(default)]
    pub preset: Option<PresetRequest>,
    /// 任务状态快照（可选）
    #[serde(default)]
    pub task_state_snapshot: Option<TaskStateSnapshotRequest>,
    /// 项目 ID（可选，影响存储路径）
    #[serde(default)]
    pub project_id: Option<String>,
}

/// compaction 请求体
#[derive(Deserialize)]
pub struct CompactionRequest {
    /// 周期类型："weekly" 或 "monthly"
    pub period: String,
    /// 项目 ID（可选）
    pub project_id: Option<String>,
}

/// compaction 响应（精简结构，与 FFI 层一致）
#[derive(Serialize)]
pub struct CompactionResult {
    pub memory_file_id: String,
    pub total_turns: usize,
    pub total_tokens: usize,
    pub hooks_count: usize,
    pub period: String,
}

/// prompt 响应
#[derive(Serialize)]
pub struct PromptResponse {
    pub prompt: String,
}

/// project_id 查询参数（GET 请求用）
#[derive(Deserialize)]
pub struct ProjectQuery {
    pub project_id: Option<String>,
}

// ============================================================================
// 辅助函数
// ============================================================================

/// 创建 Storage 实例（每次请求创建，无内存缓存）
fn create_storage(state: &AppState) -> Arc<dyn Storage> {
    Arc::new(LocalStorage::new(state.storage_root.clone()))
}

// ============================================================================
// 5 个端点 handler
// ============================================================================

/// POST /api/v1/sessions/{sid}/archive
///
/// 归档一批轮次为记忆文件，生成索引钩子。
///
/// v2.29：支持 `preset` 字段（内联预设），传入后：
/// - 用 `archive_threshold` 覆盖默认 ArchiveConfig.token_threshold
/// - 用 `summary_template` 通过 `with_summary_template_override` 注入
pub async fn archive(
    State(state): State<AppState>,
    Path(sid): Path<String>,
    Json(req): Json<ArchiveRequest>,
) -> Result<Json<SummaryView>, AppError> {
    if req.turns.is_empty() {
        return Err(AppError::BadRequest("turns 不能为空".to_string()));
    }

    // v2.33：场景识别（仅首次 archive 时识别，后续读 session_meta 跳过）
    // 优先级：用户显式 preset.scenario > session_meta > 识别 > Agent 默认
    // 识别失败不阻塞 archive，降级到 Agent 默认场景
    let effective_scenario_name: Option<String> = if let Some(detector) = &state.scenario_detector {
        // 推导 Agent family（preset.agent 解析 + Custom 兜底）
        let family = req.preset.as_ref()
            .and_then(|p| p.agent.as_deref())
            .and_then(hippocampus_agents::AgentFamily::from_str)
            .unwrap_or_else(|| hippocampus_agents::AgentFamily::Custom("unknown".to_string()));

        // 提取 preset.scenario 作为用户显式（若存在）
        let user_explicit = req.preset.as_ref()
            .and_then(|p| p.scenario.as_deref());

        let storage_for_detect = create_storage(&state);
        let scenario = hippocampus_presets::resolve_effective_scenario(
            storage_for_detect.as_ref(),
            &sid,
            user_explicit,
            &family,
            detector.as_ref(),
            &req.turns,
        ).await;

        Some(hippocampus_presets::scenario_to_str(&scenario))
    } else {
        // 未注入识别器：保留原行为（preset.scenario 或 None）
        req.preset.as_ref().and_then(|p| p.scenario.clone())
    };

    // v2.33：若识别到场景（且 preset 未显式指定），用识别的场景重新 build CombinedProfile
    // 以应用对应场景的 summary_template / archive_threshold / priority_tags 等
    let (archive_threshold, summary_template) = if let Some(preset_req) = &req.preset {
        // 用户传了 preset，按 preset build（识别结果仅作记录已写入 session_meta）
        let combined = crate::presets::build_combined_from_request(preset_req)
            .map_err(AppError::BadRequest)?;
        (
            Some(combined.archive_threshold()),
            Some(combined.summary_template().to_string()),
        )
    } else if let Some(scenario_name) = effective_scenario_name {
        // 无 preset 但识别到场景 → 用识别的场景 build
        let combined = hippocampus_presets::build_from_strings(
            None,
            Some(&scenario_name),
            None,
            None,
            None,
        ).map_err(|e| AppError::Internal(format!("识别场景构建预设失败: {e}")))?;
        (
            Some(combined.archive_threshold()),
            Some(combined.summary_template().to_string()),
        )
    } else {
        (None, None)
    };

    let storage = create_storage(&state);
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
    let mut archiver = Archiver::new(config, storage, &sid, req.project_id);

    // v2.21 批次 8b: 若注入了 summary_generator，注入到 Archiver
    // 注入后 archive() 时调用 LLM 生成结构化摘要，失败时降级为启发式
    if let Some(gen) = &state.summary_generator {
        archiver = archiver.with_summary_generator(gen.clone());
    }

    // v2.29：若构建出 summary_template，注入到 Archiver
    // 通过 with_summary_template_override 覆盖 HttpSummaryGenerator 的内部模板
    if let Some(tpl) = summary_template {
        archiver = archiver.with_summary_template_override(tpl);
    }

    for turn in req.turns {
        archiver.push_turn(turn);
    }

    let (_, hook) = archiver.archive().await?;
    let summary = SummaryView::from(&hook);

    // v2.31：归档后触发搜索索引（按 session 隔离）
    // v2.8 起由 session_search 替代全局 search_indexer，未配置时跳过（向后兼容）
    if let Some(router) = &state.session_search {
        router.index_hook(&sid, &hook).await;
    }

    tracing::info!(
        session = %sid,
        hook_id = %summary.hook_id,
        tokens = summary.token_count,
        has_preset = archive_threshold.is_some(),
        "归档成功"
    );

    Ok(Json(summary))
}

// ============================================================================
// v2.34：pre_compress 端点（压缩前一次性完整归档）
// ============================================================================

/// POST /api/v1/sessions/{sid}/pre-compress
///
/// 压缩前一次性完整归档（v2.34 新增）。
///
/// 与 archive 互补：
/// - archive：日常归档，输入结构化 turns
/// - pre-compress：压缩前一次性完整归档，输入 full_context 字符串
///
/// 双轨处理（spec 第五章）：
/// 1. raw_context 永远先存（失败才阻塞返回 500）
/// 2. 尝试解析 turns：成功复用 Archiver 归档；失败仅存 raw_context（parse_success=false）
///
/// 与 MCP 端 `pre_compress_hook` tool 行为一致。
pub async fn pre_compress(
    State(state): State<AppState>,
    Path(sid): Path<String>,
    Json(req): Json<PreCompressRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    // 1. 生成 hook_id（提前生成，用于 raw_context 文件命名）
    //    注意：Archiver 内部会生成另一个 hook_id 给 IndexHook，
    //         返回的 hook_id 是 raw_context 的 hook_id（与 MCP 端一致）。
    let hook_id = uuid::Uuid::new_v4().to_string();

    // 2. 写 raw_context（spec 第七章：永远先存，失败才阻塞返回错误）
    let storage = create_storage(&state);
    let raw_context_path = storage
        .write_raw_context(&sid, &hook_id, &req.full_context)
        .await
        .map_err(|e| {
            AppError::Internal(format!(
                "写 raw_context 失败: {e}\n\n\
                 raw_context 是 pre_compress 的核心兜底，失败则阻塞返回。\
                 后续解析/归档步骤不会执行。"
            ))
        })?;

    // 3. 估算 token（用 full_context 字符数 / 3，或客户端传入的 estimated_tokens）
    let estimated_total_tokens = req
        .estimated_tokens
        .unwrap_or_else(|| req.full_context.len() / 3);

    // 4. 尝试解析 turns（context_parser 支持 JSON 数组 + User:/Assistant: 分隔符）
    let parse_result = hippocampus_core::context_parser::parse_context(&req.full_context);

    // 5. 根据解析结果走不同分支（spec 第五章数据流）
    let (archived_tokens, parsed_turns_count, parse_success) = match parse_result {
        Some(parsed) => {
            // 5a. 解析成功：复用 Archiver 归档（应用 preset + 写 task_state_snapshot）
            let turns_count = parsed.turns.len();
            match archive_parsed_turns_in_pre_compress(
                &state,
                &sid,
                req.project_id.as_deref(),
                parsed.turns,
                &req.preset,
                &req.task_state_snapshot,
            )
            .await
            {
                Ok(tokens) => (tokens, turns_count, true),
                Err(e) => {
                    // Archiver 失败，降级为仅 raw_context（spec 7.1：不阻塞）
                    tracing::warn!(
                        session = %sid,
                        hook_id = %hook_id,
                        error = %e,
                        "Archiver 归档失败，降级为仅 raw_context（parse_success=false）"
                    );
                    (estimated_total_tokens, 0, false)
                }
            }
        }
        None => {
            // 5b. 解析失败：仅存 raw_context（不阻塞）
            tracing::info!(
                session = %sid,
                hook_id = %hook_id,
                "full_context 解析失败（非 JSON 数组、无 User:/Assistant: 标记），仅存 raw_context"
            );
            (estimated_total_tokens, 0, false)
        }
    };

    // 6. 计算 threshold / ratio / suggestion（与 archive 反馈循环一致）
    let threshold = get_archive_threshold_for_pre_compress(&req.preset);
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
        session = %sid,
        hook_id = %hook_id,
        parse_success,
        parsed_turns_count,
        archived_tokens,
        threshold,
        ratio_percent = ratio,
        "pre_compress 完成"
    );

    // 7. 构建响应 JSON（与 MCP 端 PreCompressResult 字段一致）
    let response = serde_json::json!({
        "hook_id": hook_id,
        "raw_context_path": raw_context_path,
        "parse_success": parse_success,
        "parsed_turns_count": parsed_turns_count,
        "archived_tokens": archived_tokens,
        "estimated_total_tokens": estimated_total_tokens,
        "threshold": threshold,
        "threshold_ratio_percent": ratio,
        "suggestion": suggestion,
        "archived_at": chrono::Utc::now().to_rfc3339(),
    });

    Ok(Json(response))
}

/// pre_compress 内部辅助函数：解析成功后复用 Archiver 归档 turns（v2.34 新增）
///
/// 提取 archive handler 的公共逻辑：场景识别 + 构建 Archiver + 应用 preset + 注入 summary_generator
/// + 写 task_state_snapshot + 触发搜索索引。失败时调用方应降级为仅 raw_context（parse_success=false）。
///
/// 返回值为归档后的 token_count（IndexHook.token_count），用于反馈循环。
async fn archive_parsed_turns_in_pre_compress(
    state: &AppState,
    session_id: &str,
    project_id: Option<&str>,
    turns: Vec<MessageTurn>,
    preset: &Option<PresetRequest>,
    task_state_snapshot: &Option<TaskStateSnapshotRequest>,
) -> Result<usize, String> {
    // v2.33：场景识别（仅首次 archive 时识别，后续读 session_meta 跳过）
    // 优先级：用户显式 preset.scenario > session_meta > 识别 > Agent 默认
    // 识别失败不阻塞，降级到 Agent 默认场景（与 archive 一致）
    let effective_scenario_name: Option<String> = if let Some(detector) = &state.scenario_detector
    {
        let family = preset
            .as_ref()
            .and_then(|p| p.agent.as_deref())
            .and_then(hippocampus_agents::AgentFamily::from_str)
            .unwrap_or_else(|| hippocampus_agents::AgentFamily::Custom("unknown".to_string()));

        let user_explicit = preset.as_ref().and_then(|p| p.scenario.as_deref());

        let storage_for_detect = create_storage(state);
        let scenario = hippocampus_presets::resolve_effective_scenario(
            storage_for_detect.as_ref(),
            session_id,
            user_explicit,
            &family,
            detector.as_ref(),
            &turns,
        )
        .await;
        Some(hippocampus_presets::scenario_to_str(&scenario))
    } else {
        // 未注入识别器：保留原行为（preset.scenario 或 None）
        preset.as_ref().and_then(|p| p.scenario.clone())
    };

    // 构建 preset（与 archive 一致：preset > 识别场景 > 默认）
    let (archive_threshold, summary_template) = if let Some(preset_req) = preset {
        let combined = crate::presets::build_combined_from_request(preset_req)
            .map_err(|e| format!("预设构建失败: {e}"))?;
        (
            Some(combined.archive_threshold()),
            Some(combined.summary_template().to_string()),
        )
    } else if let Some(scenario_name) = effective_scenario_name {
        // 无 preset 但识别到场景 → 用识别的场景 build
        let combined = hippocampus_presets::build_from_strings(
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

    let storage = create_storage(state);
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

    // 注入 summary_generator（若注入）
    if let Some(gen) = &state.summary_generator {
        archiver = archiver.with_summary_generator(gen.clone());
    }

    // 注入 summary_template（若构建出）
    if let Some(tpl) = summary_template {
        archiver = archiver.with_summary_template_override(tpl);
    }

    for turn in turns {
        archiver.push_turn(turn);
    }

    let (_, hook) = archiver
        .archive()
        .await
        .map_err(|e| format!("归档失败: {e}"))?;

    // v2.31：归档后触发搜索索引（按 session 隔离）
    if let Some(router) = &state.session_search {
        router.index_hook(session_id, &hook).await;
    }

    // 写 task_state_snapshot（若有，与 MCP 端一致：失败不影响归档结果）
    if let Some(snap) = task_state_snapshot {
        let snapshot = hippocampus_core::model::TaskStateSnapshot {
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

    // 返回归档后的 token_count（IndexHook.token_count，供反馈循环）
    Ok(hook.token_count)
}

/// 获取当前 archive 阈值（v2.34 新增）
///
/// 优先级（与 archive 一致）：
/// 1. preset.archive_threshold（用户显式覆盖，最高优先级）
/// 2. preset 构建的 CombinedProfile.archive_threshold()
/// 3. 默认 120000
///
/// 注意：server 端 AppState 没有 `combined_profile` 字段（只 MCP 端有），
///      所以不支持 MCP 端的第3优先级（`self.combined_profile()`），直接降级到默认 120000。
fn get_archive_threshold_for_pre_compress(preset: &Option<PresetRequest>) -> usize {
    if let Some(preset_req) = preset {
        // 用户显式 archive_threshold 最高优先级
        if let Some(t) = preset_req.archive_threshold {
            return t;
        }
        // 用 preset 构建 CombinedProfile（仅取 archive_threshold，不传 user_override）
        if let Ok(combined) = crate::presets::build_combined_from_request(preset_req) {
            return combined.archive_threshold();
        }
    }
    // 默认阈值（与 ArchiveConfig::default().token_threshold 一致）
    120000
}

/// GET /api/v1/sessions/{sid}/memories/{hook_id}
///
/// 按钩子 ID 检索完整记忆文件。
pub async fn retrieve(
    State(state): State<AppState>,
    Path((sid, hook_id)): Path<(String, String)>,
    Query(query): Query<ProjectQuery>,
) -> Result<Json<MemoryFile>, AppError> {
    let storage = create_storage(&state);
    let retriever = Retriever::new(storage, &sid, query.project_id);

    let memory = retriever.retrieve_memory(&hook_id).await?;

    tracing::info!(
        session = %sid,
        hook_id = %hook_id,
        turns = memory.turns.len(),
        "检索成功"
    );

    Ok(Json(memory))
}

/// GET /api/v1/sessions/{sid}/summaries
///
/// 获取所有周期的摘要视图列表。
pub async fn get_summaries(
    State(state): State<AppState>,
    Path(sid): Path<String>,
    Query(query): Query<ProjectQuery>,
) -> Result<Json<Vec<SummaryView>>, AppError> {
    let storage = create_storage(&state);
    let retriever = Retriever::new(storage, &sid, query.project_id);

    let summaries = retriever.get_summaries().await?;

    tracing::info!(
        session = %sid,
        count = summaries.len(),
        "获取摘要成功"
    );

    Ok(Json(summaries))
}

/// GET /api/v1/sessions/{sid}/prompt
///
/// 渲染摘要为 system prompt 文本。
pub async fn render_prompt(
    State(state): State<AppState>,
    Path(sid): Path<String>,
    Query(query): Query<ProjectQuery>,
) -> Result<Json<PromptResponse>, AppError> {
    let storage = create_storage(&state);
    let retriever = Retriever::new(storage, &sid, query.project_id);

    let prompt = retriever.render_to_system_prompt().await?;

    tracing::info!(
        session = %sid,
        prompt_len = prompt.len(),
        "渲染 prompt 成功"
    );

    Ok(Json(PromptResponse { prompt }))
}

/// POST /api/v1/sessions/{sid}/compaction
///
/// 触发周期任务（周级合并 / 月级评分淘汰）。
pub async fn run_compaction(
    State(state): State<AppState>,
    Path(sid): Path<String>,
    Json(req): Json<CompactionRequest>,
) -> Result<Json<CompactionResult>, AppError> {
    let storage = create_storage(&state);
    let mut compactor = Compactor::new(
        storage,
        Box::new(DefaultScorer::new()),
        &sid,
        req.project_id,
    );

    // v2.22: 若注入了 summary_generator，注入到 Compactor（compaction 也用 LLM 摘要）
    if let Some(gen) = &state.summary_generator {
        compactor = compactor.with_summary_generator(gen.clone());
    }

    let (memory, index_doc) = match req.period.as_str() {
        "weekly" => compactor.weekly_merge().await?,
        "monthly" => compactor.monthly_evict().await?,
        other => {
            return Err(AppError::BadRequest(format!(
                "无效的 period 值: {}（支持: weekly, monthly）",
                other
            )));
        }
    };

    let result = CompactionResult {
        memory_file_id: memory.id.to_string(),
        total_turns: memory.turns.len(),
        total_tokens: memory.total_tokens,
        hooks_count: index_doc.hooks.len(),
        period: req.period,
    };

    tracing::info!(
        session = %sid,
        period = %result.period,
        turns = result.total_turns,
        "周期任务完成"
    );

    Ok(Json(result))
}

// ============================================================================
// v2.4 批次 3：记忆迭代更新端点
// ============================================================================

/// update_memory 请求体
#[derive(Deserialize)]
pub struct UpdateMemoryRequest {
    /// 新增的事实列表
    #[serde(default)]
    pub added_facts: Vec<String>,
    /// 修正的事实列表
    #[serde(default)]
    pub revised_facts: Vec<String>,
    /// 废弃的事实列表
    #[serde(default)]
    pub deprecated_facts: Vec<String>,
    /// 项目 ID（可选，影响索引查找范围）
    pub project_id: Option<String>,
}

/// update_memory 响应体
#[derive(Serialize)]
pub struct UpdateMemoryResponse {
    /// 是否更新成功
    pub success: bool,
    /// 更新的事实数量统计
    pub added: usize,
    pub revised: usize,
    pub deprecated: usize,
    /// 检测到的冲突数量（v2.6 批次 8）
    pub conflicts: usize,
    /// 是否存在 Critical 级别冲突（v2.6 批次 8）
    pub has_critical: bool,
}

/// conflicts 查询响应体（v2.6 批次 8）
#[derive(Serialize)]
pub struct ConflictsResponse {
    /// 冲突总数
    pub total: usize,
    /// Critical 级别冲突数
    pub critical_count: usize,
    /// 所有冲突记录（扁平化，按 updates 时间顺序）
    pub conflicts: Vec<hippocampus_core::conflict::ConflictRecord>,
}

/// PATCH /api/v1/sessions/{sid}/memories/{hook_id}
///
/// 按钩子 ID 更新记忆文件（added/revised/deprecated facts）。
///
/// 流程：
/// 1. 通过 hook_id 从索引文档查找对应的 memory_id
/// 2. 调用 Storage::update_memory 应用更新
/// 3. 返回更新结果统计
pub async fn update_memory(
    State(state): State<AppState>,
    Path((sid, hook_id)): Path<(String, String)>,
    Json(req): Json<UpdateMemoryRequest>,
) -> Result<Json<UpdateMemoryResponse>, AppError> {
    // 空更新校验
    if req.added_facts.is_empty() && req.revised_facts.is_empty() && req.deprecated_facts.is_empty()
    {
        return Err(AppError::BadRequest(
            "更新内容不能为空：至少需要一项 added/revised/deprecated facts".into(),
        ));
    }

    let storage = create_storage(&state);
    let retriever = Retriever::new(storage.clone(), &sid, req.project_id.clone());

    // v2.27.1：使用 find_hook_by_id 获取完整 IndexHook（含 summary.key_facts）
    let hook = retriever.find_hook_by_id(&hook_id).await.ok_or_else(|| {
        AppError::NotFound(format!("未找到钩子 ID: {}", hook_id))
    })?;
    let memory_id = hook.memory_id.clone();

    // 构造 MemoryUpdate（逐条添加保持事实粒度，v2.25.1）
    let mut updates = hippocampus_core::model::MemoryUpdate::new();
    for fact in &req.added_facts {
        updates = updates.add_fact(fact.clone());
    }
    for fact in &req.revised_facts {
        updates = updates.revise_fact(fact.clone());
    }
    for fact in &req.deprecated_facts {
        updates = updates.deprecate_fact(fact.clone());
    }

    let added_count = req.added_facts.len();
    let revised_count = req.revised_facts.len();
    let deprecated_count = req.deprecated_facts.len();

    // v2.6 批次 8：update 前同步检测冲突
    //
    // 配置了 conflict_detector 时：读取现有记忆 → 检测 → 持久化冲突记录
    // 未配置时：跳过检测，直接 update_memory（向后兼容）
    let conflicts: Vec<hippocampus_core::conflict::ConflictRecord> =
        if let Some(detector) = &state.conflict_detector {
            let mut existing = storage.read_memory(&memory_id).await?;
            // v2.27.1：从 IndexHook.key_facts 注入历史事实（与 detect_conflicts 一致）
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
            }
            let report = detector.detect(&updates, &existing).await;
            let conflict_count = report.count();
            let has_critical = report.has_critical();
            tracing::info!(
                session = %sid,
                hook_id = %hook_id,
                memory_id = %memory_id,
                conflict_count = conflict_count,
                has_critical = has_critical,
                "冲突检测完成"
            );
            report.conflicts
        } else {
            Vec::new()
        };

    // 执行更新（携带冲突记录）
    storage
        .update_memory_with_conflicts(&memory_id, updates, conflicts.clone())
        .await?;

    let conflict_count = conflicts.len();
    let has_critical = conflicts
        .iter()
        .any(|c| c.severity == hippocampus_core::conflict::Severity::Critical);

    tracing::info!(
        session = %sid,
        hook_id = %hook_id,
        memory_id = %memory_id,
        added = added_count,
        revised = revised_count,
        deprecated = deprecated_count,
        conflict_count = conflict_count,
        has_critical = has_critical,
        "记忆迭代更新成功（含冲突检测）"
    );

    Ok(Json(UpdateMemoryResponse {
        success: true,
        added: added_count,
        revised: revised_count,
        deprecated: deprecated_count,
        conflicts: conflict_count,
        has_critical,
    }))
}

// ============================================================================
// v2.6 批次 8：冲突查询端点
// ============================================================================

/// GET /api/v1/sessions/{sid}/memories/{hook_id}/conflicts
///
/// 获取指定记忆文件的所有冲突记录（来自历史 updates 的 conflicts 字段）。
///
/// 返回扁平化的冲突记录列表，按时间顺序（updates 的追加顺序）。
pub async fn get_conflicts(
    State(state): State<AppState>,
    Path((sid, hook_id)): Path<(String, String)>,
    Query(query): Query<ProjectQuery>,
) -> Result<Json<ConflictsResponse>, AppError> {
    let storage = create_storage(&state);
    let retriever = Retriever::new(storage, &sid, query.project_id);

    // 通过 hook_id 找到 memory_id
    let memory_id = retriever
        .find_memory_id_by_hook(&hook_id)
        .await
        .ok_or_else(|| AppError::NotFound(format!("未找到钩子 ID: {}", hook_id)))?;

    // 读取记忆文件
    let storage = create_storage(&state);
    let memory = storage.read_memory(&memory_id).await?;

    // 扁平化所有 updates 中的 conflicts
    let mut all_conflicts: Vec<hippocampus_core::conflict::ConflictRecord> = Vec::new();
    for record in &memory.updates {
        all_conflicts.extend(record.conflicts.iter().cloned());
    }

    let total = all_conflicts.len();
    let critical_count = all_conflicts
        .iter()
        .filter(|c| c.severity == hippocampus_core::conflict::Severity::Critical)
        .count();

    tracing::info!(
        session = %sid,
        hook_id = %hook_id,
        memory_id = %memory_id,
        total = total,
        critical = critical_count,
        "查询冲突记录成功"
    );

    Ok(Json(ConflictsResponse {
        total,
        critical_count,
        conflicts: all_conflicts,
    }))
}

// ============================================================================
// v2.27：冲突预检测端点（不实际写入）
// ============================================================================

/// POST /api/v1/sessions/{sid}/memories/{hook_id}/detect-conflicts
///
/// 检测单次记忆更新的潜在冲突（不实际写入）。
/// 与 MCP 端 `detect_conflicts` tool 行为一致：
/// - 读取 IndexHook 的 summary.key_facts 作为历史事实集
/// - 注入到 memory.updates（若为空），让 detector 能看到 archive 时的结构化事实
/// - 调用注入的 conflict_detector（HybridDetector / HeuristicDetector）检测
///
/// 与 `update_memory` 的区别：不持久化更新和冲突记录，仅返回检测报告。
/// 用于 Agent 在 update 前评估风险。
pub async fn detect_conflicts(
    State(state): State<AppState>,
    Path((sid, hook_id)): Path<(String, String)>,
    Json(req): Json<UpdateMemoryRequest>,
) -> Result<Json<ConflictsResponse>, AppError> {
    // 空更新校验
    if req.added_facts.is_empty() && req.revised_facts.is_empty() && req.deprecated_facts.is_empty()
    {
        return Err(AppError::BadRequest(
            "更新内容不能为空：至少需要一项 added/revised/deprecated facts".into(),
        ));
    }

    let storage = create_storage(&state);
    let retriever = Retriever::new(storage.clone(), &sid, req.project_id);

    // v2.27：使用 find_hook_by_id 获取完整 IndexHook（含 summary.key_facts）
    let hook = retriever.find_hook_by_id(&hook_id).await.ok_or_else(|| {
        AppError::NotFound(format!("未找到钩子 ID: {}", hook_id))
    })?;

    // 读取现有记忆
    let mut existing = storage.read_memory(&hook.memory_id).await?;

    // v2.27：若 memory.updates 为空但 IndexHook 有 key_facts，
    // 把 key_facts 作为虚拟 MemoryUpdateRecord 注入，让 detector 能看到历史事实。
    // 解决 archive 只写 turns 不写 updates 的设计缺陷（与 MCP 端保持一致）。
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
            session = %sid,
            hook_id = %hook_id,
            facts_count = hook.summary.key_facts.len(),
            "detect_conflicts: 已从 IndexHook 注入 key_facts 作为历史事实集"
        );
    }

    // 构造 MemoryUpdate（逐条添加保持事实粒度，v2.25.1）
    let mut update = hippocampus_core::model::MemoryUpdate::new();
    for fact in &req.added_facts {
        update = update.add_fact(fact.clone());
    }
    for fact in &req.revised_facts {
        update = update.revise_fact(fact.clone());
    }
    for fact in &req.deprecated_facts {
        update = update.deprecate_fact(fact.clone());
    }

    // 调用检测器（未配置时降级为 HeuristicDetector）
    let detector: Arc<dyn hippocampus_core::conflict::ConflictDetector> =
        match &state.conflict_detector {
            Some(d) => Arc::clone(d),
            None => Arc::new(hippocampus_core::heuristic::HeuristicDetector::new()),
        };
    let report = detector.detect(&update, &existing).await;

    let total = report.count();
    let has_critical = report.has_critical();
    let critical_count = report
        .conflicts
        .iter()
        .filter(|c| c.severity == hippocampus_core::conflict::Severity::Critical)
        .count();

    tracing::info!(
        session = %sid,
        hook_id = %hook_id,
        memory_id = %hook.memory_id,
        total = total,
        critical = critical_count,
        has_critical = has_critical,
        "冲突预检测完成（未写入）"
    );

    Ok(Json(ConflictsResponse {
        total,
        critical_count,
        conflicts: report.conflicts,
    }))
}

// ============================================================================
// v2.5 批次 6：批量操作端点
// ============================================================================

/// batch-retrieve 请求体
#[derive(Deserialize)]
pub struct BatchRetrieveRequest {
    /// 要检索的 hook_id 列表
    pub hook_ids: Vec<String>,
    /// 项目 ID（可选）
    pub project_id: Option<String>,
}

/// batch-retrieve 单条结果
#[derive(Serialize)]
pub struct BatchRetrieveItem {
    /// 钩子 ID
    pub hook_id: String,
    /// 是否成功
    pub success: bool,
    /// 成功时的记忆文件
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<MemoryFile>,
    /// 失败时的错误信息
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// POST /api/v1/sessions/{sid}/memories/batch-retrieve
///
/// 批量按 hook_id 列表检索记忆文件。单个失败不影响其他条目。
pub async fn batch_retrieve(
    State(state): State<AppState>,
    Path(sid): Path<String>,
    Json(req): Json<BatchRetrieveRequest>,
) -> Result<Json<Vec<BatchRetrieveItem>>, AppError> {
    if req.hook_ids.is_empty() {
        return Err(AppError::BadRequest("hook_ids 不能为空".to_string()));
    }

    let storage = create_storage(&state);
    let retriever = Retriever::new(storage, &sid, req.project_id);

    // v2.16 IMP-08：并发检索（Semaphore 限制 8 并发 + JoinSet 收集结果）
    // v2.18 修复：保持结果顺序与输入 hook_ids 一致（按 index 回填到预分配 Vec）
    //
    // 串行循环改为并发执行，提升批量检索性能。
    // 单个失败不影响其他（保持原有容错语义）。
    // 8 是经验值：平衡并发开销与系统负载，适合大多数存储后端。
    use std::sync::Arc;
    use tokio::sync::Semaphore;
    let semaphore = Arc::new(Semaphore::new(8));
    let mut tasks = tokio::task::JoinSet::new();
    for (idx, hook_id) in req.hook_ids.iter().cloned().enumerate() {
        let retriever = retriever.clone();
        let sem = semaphore.clone();
        tasks.spawn(async move {
            let _permit = match sem.acquire().await {
                Ok(p) => p,
                Err(e) => {
                    return (
                        idx,
                        BatchRetrieveItem {
                            hook_id,
                            success: false,
                            data: None,
                            error: Some(format!("获取并发许可失败: {e}")),
                        },
                    );
                }
            };
            let item = match retriever.retrieve_memory(&hook_id).await {
                Ok(memory) => BatchRetrieveItem {
                    hook_id,
                    success: true,
                    data: Some(memory),
                    error: None,
                },
                Err(e) => BatchRetrieveItem {
                    hook_id,
                    success: false,
                    data: None,
                    error: Some(e.to_string()),
                },
            };
            (idx, item)
        });
    }

    // 按 idx 回填结果，保持顺序与输入 hook_ids 一致
    let mut results: Vec<Option<BatchRetrieveItem>> =
        (0..req.hook_ids.len()).map(|_| None).collect();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((idx, item)) => results[idx] = Some(item),
            Err(e) => {
                // 任务 panic：无法定位 idx，找第一个空位填入错误项
                tracing::error!(error = %e, "batch_retrieve 任务 panic，跳过");
                if let Some(slot) = results.iter_mut().find(|r| r.is_none()) {
                    *slot = Some(BatchRetrieveItem {
                        hook_id: String::new(),
                        success: false,
                        data: None,
                        error: Some(format!("内部任务错误: {e}")),
                    });
                }
            }
        }
    }

    // 展开结果（理论已全部填满，兜底处理未完成的槽位）
    let results: Vec<BatchRetrieveItem> = results
        .into_iter()
        .map(|x| x.unwrap_or_else(|| BatchRetrieveItem {
            hook_id: String::new(),
            success: false,
            data: None,
            error: Some("内部错误：任务未完成".to_string()),
        }))
        .collect();

    tracing::info!(
        session = %sid,
        total = results.len(),
        success = results.iter().filter(|r| r.success).count(),
        "批量检索完成"
    );

    Ok(Json(results))
}

/// batch-delete 请求体
#[derive(Deserialize)]
pub struct BatchDeleteRequest {
    /// 要删除的 hook_id 列表
    pub hook_ids: Vec<String>,
    /// 项目 ID（可选）
    pub project_id: Option<String>,
}

/// batch-delete 单条结果
#[derive(Serialize)]
pub struct BatchDeleteItem {
    /// 钩子 ID
    pub hook_id: String,
    /// 是否成功
    pub success: bool,
    /// 失败时的错误信息
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// DELETE /api/v1/sessions/{sid}/memories/batch
///
/// 批量按 hook_id 列表删除记忆文件。单个失败不影响其他条目。
///
/// v2.31：改用 `delete_memory_complete`（软删除方案）：
/// - 删除记忆文件 + 将索引钩子标记为 `FileStatus::Deleted`
/// - 同步调用 `unindex_hook` 清理 SessionSearchRouter 内存索引
/// - 避免 semantic_search 返回幽灵记忆 / retrieve 崩溃
pub async fn batch_delete(
    State(state): State<AppState>,
    Path(sid): Path<String>,
    Json(req): Json<BatchDeleteRequest>,
) -> Result<Json<Vec<BatchDeleteItem>>, AppError> {
    if req.hook_ids.is_empty() {
        return Err(AppError::BadRequest("hook_ids 不能为空".to_string()));
    }

    let storage = create_storage(&state);
    let retriever = Retriever::new(storage.clone(), &sid, req.project_id.clone());

    // 1. 查找每个 hook_id 的完整 IndexHook（含 memory_id 和 period）
    let mut hooks_info: Vec<(String, Option<(String, hippocampus_core::model::ArchivePeriod)> )> =
        Vec::with_capacity(req.hook_ids.len());
    for hook_id in &req.hook_ids {
        let info = retriever.find_hook_by_id(hook_id).await.map(|h| {
            (h.memory_id.clone(), h.period)
        });
        hooks_info.push((hook_id.clone(), info));
    }

    // 2. 逐条调用 delete_memory_complete（软删除：删文件 + 标记索引）
    let mut results: Vec<BatchDeleteItem> = Vec::with_capacity(req.hook_ids.len());
    for (hook_id, info_opt) in &hooks_info {
        match info_opt {
            None => {
                results.push(BatchDeleteItem {
                    hook_id: hook_id.clone(),
                    success: false,
                    error: Some("未找到对应的 memory_id".to_string()),
                });
            }
            Some((memory_id, period)) => {
                let r = storage
                    .delete_memory_complete(
                        memory_id,
                        hook_id,
                        &sid,
                        req.project_id.as_deref(),
                        *period,
                    )
                    .await;

                // v2.31：同步清理 SessionSearchRouter 内存索引（即使 storage 删除失败也尝试清理）
                if let Some(router) = &state.session_search {
                    router.unindex_hook(&sid, hook_id).await;
                }

                match r {
                    Ok(()) => results.push(BatchDeleteItem {
                        hook_id: hook_id.clone(),
                        success: true,
                        error: None,
                    }),
                    Err(e) => results.push(BatchDeleteItem {
                        hook_id: hook_id.clone(),
                        success: false,
                        error: Some(e.to_string()),
                    }),
                }
            }
        }
    }

    tracing::info!(
        session = %sid,
        total = results.len(),
        success = results.iter().filter(|r| r.success).count(),
        "批量删除完成（v2.31 软删除：文件删除 + 索引标记 Deleted + 内存索引清理）"
    );

    Ok(Json(results))
}

/// batch-update 单条更新条目
#[derive(Deserialize)]
pub struct BatchUpdateEntry {
    /// 钩子 ID
    pub hook_id: String,
    /// 新增的事实
    #[serde(default)]
    pub added_facts: Vec<String>,
    /// 修正的事实
    #[serde(default)]
    pub revised_facts: Vec<String>,
    /// 废弃的事实
    #[serde(default)]
    pub deprecated_facts: Vec<String>,
}

/// batch-update 请求体
#[derive(Deserialize)]
pub struct BatchUpdateRequest {
    /// 更新条目列表
    pub updates: Vec<BatchUpdateEntry>,
    /// 项目 ID（可选）
    pub project_id: Option<String>,
}

/// batch-update 单条结果
#[derive(Serialize)]
pub struct BatchUpdateItem {
    /// 钩子 ID
    pub hook_id: String,
    /// 是否成功
    pub success: bool,
    /// 成功时的事实数量统计
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revised: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<usize>,
    /// 失败时的错误信息
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 检测到的冲突数量（v2.12：集成 conflict_detector 时有值）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflicts: Option<usize>,
    /// 是否存在 Critical 级别冲突（v2.12：集成 conflict_detector 时有值）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_critical: Option<bool>,
}

/// PATCH /api/v1/sessions/{sid}/memories/batch
///
/// 批量按 hook_id 列表更新记忆文件。单个失败不影响其他条目。
pub async fn batch_update(
    State(state): State<AppState>,
    Path(sid): Path<String>,
    Json(req): Json<BatchUpdateRequest>,
) -> Result<Json<Vec<BatchUpdateItem>>, AppError> {
    if req.updates.is_empty() {
        return Err(AppError::BadRequest("updates 不能为空".to_string()));
    }

    let storage = create_storage(&state);
    let retriever = Retriever::new(storage.clone(), &sid, req.project_id);

    // v2.12：用具名结构体替代 6 元组，支持冲突检测字段
    struct UpdatePair {
        memory_id: String,
        hook_id: String,
        update: hippocampus_core::model::MemoryUpdate,
        added: usize,
        revised: usize,
        deprecated: usize,
        conflicts_count: usize,
        has_critical: bool,
        result: Option<hippocampus_core::Result<()>>,
    }

    // 1. 将 hook_id 转为 memory_id，构造更新对
    let mut pairs: Vec<UpdatePair> = Vec::new();
    for entry in &req.updates {
        let hook = retriever.find_hook_by_id(&entry.hook_id).await;
        match hook {
            Some(h) => {
                let added = entry.added_facts.len();
                let revised = entry.revised_facts.len();
                let deprecated = entry.deprecated_facts.len();
                // v2.27.1：逐条 add_fact 保持事实粒度（与 detect_conflicts 一致）
                let mut update = hippocampus_core::model::MemoryUpdate::new();
                for fact in &entry.added_facts {
                    update = update.add_fact(fact.clone());
                }
                for fact in &entry.revised_facts {
                    update = update.revise_fact(fact.clone());
                }
                for fact in &entry.deprecated_facts {
                    update = update.deprecate_fact(fact.clone());
                }
                pairs.push(UpdatePair {
                    memory_id: h.memory_id.clone(),
                    hook_id: entry.hook_id.clone(),
                    update,
                    added,
                    revised,
                    deprecated,
                    conflicts_count: 0,
                    has_critical: false,
                    result: None,
                });
            }
            None => {
                // hook_id 无效的条目稍后处理为失败
                pairs.push(UpdatePair {
                    memory_id: String::new(),
                    hook_id: entry.hook_id.clone(),
                    update: hippocampus_core::model::MemoryUpdate::new(),
                    added: 0,
                    revised: 0,
                    deprecated: 0,
                    conflicts_count: 0,
                    has_critical: false,
                    result: None,
                });
            }
        }
    }

    // 2. 执行更新（v2.12：有检测器时逐条检测 + 持久化冲突，与 MCP 行为对齐）
    if let Some(detector) = &state.conflict_detector {
        // v2.12：有检测器时逐条检测 + 持久化冲突到 MemoryUpdateRecord.conflicts
        for pair in &mut pairs {
            if pair.memory_id.is_empty() {
                continue;
            }
            let result = match storage.read_memory(&pair.memory_id).await {
                Ok(mut existing) => {
                    // v2.27.1：从 IndexHook.key_facts 注入历史事实（与 detect_conflicts 一致）
                    // 解决 archive 只写 turns 不写 updates 的设计缺陷
                    if existing.updates.is_empty() {
                        if let Some(h) = retriever.find_hook_by_id(&pair.hook_id).await {
                            if !h.summary.key_facts.is_empty() {
                                use hippocampus_core::model::MemoryUpdateRecord;
                                let mut virtual_update = hippocampus_core::model::MemoryUpdate::new();
                                for fact in &h.summary.key_facts {
                                    virtual_update = virtual_update.add_fact(fact.clone());
                                }
                                existing.updates.push(MemoryUpdateRecord {
                                    updated_at: h.archived_at,
                                    update: virtual_update,
                                    conflicts: vec![],
                                });
                            }
                        }
                    }
                    let report = detector.detect(&pair.update, &existing).await;
                    pair.conflicts_count = report.count();
                    pair.has_critical = report.has_critical();
                    storage.update_memory_with_conflicts(
                        &pair.memory_id,
                        pair.update.clone(),
                        report.conflicts,
                    ).await
                }
                Err(e) => Err(e),
            };
            pair.result = Some(result);
        }
    } else {
        // 无检测器：保持原有 batch 行为（向后兼容，v2.6 行为）
        let valid_updates: Vec<(String, hippocampus_core::model::MemoryUpdate)> = pairs
            .iter()
            .filter(|p| !p.memory_id.is_empty())
            .map(|p| (p.memory_id.clone(), p.update.clone()))
            .collect();

        let update_results = if !valid_updates.is_empty() {
            storage.update_memories_batch(&valid_updates).await
        } else {
            Vec::new()
        };

        let mut idx = 0;
        for pair in &mut pairs {
            if !pair.memory_id.is_empty() && idx < update_results.len() {
                pair.result = Some(update_results[idx].clone());
                idx += 1;
            }
        }
    }

    // 3. 构建响应
    let results: Vec<BatchUpdateItem> = pairs
        .iter()
        .map(|p| {
            if p.memory_id.is_empty() {
                return BatchUpdateItem {
                    hook_id: p.hook_id.clone(),
                    success: false,
                    added: None,
                    revised: None,
                    deprecated: None,
                    error: Some("未找到对应的 memory_id".to_string()),
                    conflicts: None,
                    has_critical: None,
                };
            }
            match &p.result {
                Some(Ok(())) => BatchUpdateItem {
                    hook_id: p.hook_id.clone(),
                    success: true,
                    added: Some(p.added),
                    revised: Some(p.revised),
                    deprecated: Some(p.deprecated),
                    error: None,
                    conflicts: Some(p.conflicts_count),
                    has_critical: Some(p.has_critical),
                },
                Some(Err(e)) => BatchUpdateItem {
                    hook_id: p.hook_id.clone(),
                    success: false,
                    added: None,
                    revised: None,
                    deprecated: None,
                    error: Some(e.to_string()),
                    conflicts: None,
                    has_critical: None,
                },
                None => BatchUpdateItem {
                    hook_id: p.hook_id.clone(),
                    success: false,
                    added: None,
                    revised: None,
                    deprecated: None,
                    error: Some("内部错误：结果缺失".to_string()),
                    conflicts: None,
                    has_critical: None,
                },
            }
        })
        .collect();

    tracing::info!(
        session = %sid,
        total = results.len(),
        success = results.iter().filter(|r| r.success).count(),
        "批量更新完成"
    );

    Ok(Json(results))
}

// ============================================================================
// v2.5 批次 7：语义检索端点
// ============================================================================

/// search 请求体
#[derive(Deserialize)]
pub struct SearchRequest {
    /// 搜索查询文本
    pub query: String,
    /// 返回 top-K 结果（默认 5）
    pub top_k: Option<usize>,
}

/// search 响应体
#[derive(Serialize)]
pub struct SearchResponse {
    /// 搜索结果列表（按相关性降序）
    pub results: Vec<hippocampus_core::semantic::SearchHit>,
    /// 检索模式（keyword / semantic / hybrid）
    pub mode: String,
}

/// POST /api/v1/sessions/{sid}/search
///
/// 语义检索记忆文件。
///
/// 需要在服务启动时配置 SessionSearchRouter（通过 AppState.session_search）。
/// 未配置时返回 501 Not Implemented。
pub async fn search(
    State(state): State<AppState>,
    Path(sid): Path<String>,
    Json(req): Json<SearchRequest>,
) -> Result<Json<SearchResponse>, AppError> {
    // 校验查询文本
    let query = req.query.trim().to_string();
    if query.is_empty() {
        return Err(AppError::BadRequest("query 不能为空".to_string()));
    }

    let top_k = req.top_k.unwrap_or(5);

    // v2.31：使用 session_search（session 级隔离）
    // v2.8 起由 session_search 替代全局 retriever，未配置时返回错误
    let results = if let Some(router) = &state.session_search {
        router.search(&sid, &query, top_k).await?
    } else {
        return Err(AppError::NotImplemented(
            "语义检索未配置：请通过环境变量配置 Embedder API 后重启服务".to_string(),
        ));
    };

    // 推断检索模式
    let mode = if results.is_empty() {
        "empty"
    } else {
        match results[0].source {
            hippocampus_core::semantic::RetrievalSource::Keyword => "keyword",
            hippocampus_core::semantic::RetrievalSource::Semantic => "semantic",
            hippocampus_core::semantic::RetrievalSource::Hybrid => "hybrid",
        }
    };

    tracing::info!(
        session = %sid,
        query = %query,
        top_k,
        results_count = results.len(),
        mode,
        "语义检索完成"
    );

    Ok(Json(SearchResponse {
        results,
        mode: mode.to_string(),
    }))
}
