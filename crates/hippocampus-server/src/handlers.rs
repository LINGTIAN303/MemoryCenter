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
pub async fn archive(
    State(state): State<AppState>,
    Path(sid): Path<String>,
    Json(req): Json<ArchiveRequest>,
) -> Result<Json<SummaryView>, AppError> {
    if req.turns.is_empty() {
        return Err(AppError::BadRequest("turns 不能为空".to_string()));
    }

    let storage = create_storage(&state);
    let config = ArchiveConfig::default();
    let mut archiver = Archiver::new(config, storage, &sid, req.project_id);

    for turn in req.turns {
        archiver.push_turn(turn);
    }

    let (_, hook) = archiver.archive().await?;
    let summary = SummaryView::from(&hook);

    // v2.5 批次 7：归档后触发搜索索引（关键词 + 向量）
    if let Some(indexer) = &state.search_indexer {
        indexer.index_hook(&hook).await;
    }

    tracing::info!(
        session = %sid,
        hook_id = %summary.hook_id,
        tokens = summary.token_count,
        "归档成功"
    );

    Ok(Json(summary))
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
    let compactor = Compactor::new(
        storage,
        Box::new(DefaultScorer::new()),
        &sid,
        req.project_id,
    );

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

    // 通过 hook_id 找到 memory_id
    let memory_id = retriever.find_memory_id_by_hook(&hook_id).await.ok_or_else(|| {
        AppError::NotFound(format!("未找到钩子 ID: {}", hook_id))
    })?;

    // 构造 MemoryUpdate
    let updates = hippocampus_core::model::MemoryUpdate::new()
        .add_fact(req.added_facts.join("\n"))
        .revise_fact(req.revised_facts.join("\n"))
        .deprecate_fact(req.deprecated_facts.join("\n"));

    let added_count = req.added_facts.len();
    let revised_count = req.revised_facts.len();
    let deprecated_count = req.deprecated_facts.len();

    // v2.6 批次 8：update 前同步检测冲突
    //
    // 配置了 conflict_detector 时：读取现有记忆 → 检测 → 持久化冲突记录
    // 未配置时：跳过检测，直接 update_memory（向后兼容）
    let conflicts: Vec<hippocampus_core::conflict::ConflictRecord> =
        if let Some(detector) = &state.conflict_detector {
            let existing = storage.read_memory(&memory_id).await?;
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

    let mut results = Vec::with_capacity(req.hook_ids.len());
    for hook_id in &req.hook_ids {
        match retriever.retrieve_memory(hook_id).await {
            Ok(memory) => results.push(BatchRetrieveItem {
                hook_id: hook_id.clone(),
                success: true,
                data: Some(memory),
                error: None,
            }),
            Err(e) => results.push(BatchRetrieveItem {
                hook_id: hook_id.clone(),
                success: false,
                data: None,
                error: Some(e.to_string()),
            }),
        }
    }

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
pub async fn batch_delete(
    State(state): State<AppState>,
    Path(sid): Path<String>,
    Json(req): Json<BatchDeleteRequest>,
) -> Result<Json<Vec<BatchDeleteItem>>, AppError> {
    if req.hook_ids.is_empty() {
        return Err(AppError::BadRequest("hook_ids 不能为空".to_string()));
    }

    let storage = create_storage(&state);
    let retriever = Retriever::new(storage.clone(), &sid, req.project_id);

    // 1. 将 hook_id 列表转换为 memory_id 列表（保持对应关系）
    let mut memory_ids: Vec<(String, Option<String>)> = Vec::with_capacity(req.hook_ids.len());
    for hook_id in &req.hook_ids {
        let mid = retriever.find_memory_id_by_hook(hook_id).await;
        memory_ids.push((hook_id.clone(), mid));
    }

    // 2. 过滤出有效的 memory_id，调用批量删除
    let valid: Vec<String> = memory_ids
        .iter()
        .filter_map(|(_, mid)| mid.clone())
        .collect();

    let delete_results = if !valid.is_empty() {
        storage.delete_memories_batch(&valid).await
    } else {
        Vec::new()
    };

    // 3. 构建响应（按原始 hook_id 顺序）
    let mut mid_to_result: std::collections::HashMap<String, &hippocampus_core::Result<()>> =
        std::collections::HashMap::new();
    let mut idx = 0;
    for (_, mid_opt) in &memory_ids {
        if let Some(mid) = mid_opt {
            if idx < delete_results.len() {
                mid_to_result.insert(mid.clone(), &delete_results[idx]);
                idx += 1;
            }
        }
    }

    let results: Vec<BatchDeleteItem> = memory_ids
        .iter()
        .map(|(hook_id, mid_opt)| match mid_opt {
            None => BatchDeleteItem {
                hook_id: hook_id.clone(),
                success: false,
                error: Some("未找到对应的 memory_id".to_string()),
            },
            Some(mid) => {
                let r = mid_to_result.get(mid);
                match r {
                    Some(Ok(())) => BatchDeleteItem {
                        hook_id: hook_id.clone(),
                        success: true,
                        error: None,
                    },
                    Some(Err(e)) => BatchDeleteItem {
                        hook_id: hook_id.clone(),
                        success: false,
                        error: Some(e.to_string()),
                    },
                    None => BatchDeleteItem {
                        hook_id: hook_id.clone(),
                        success: false,
                        error: Some("内部错误：结果缺失".to_string()),
                    },
                }
            }
        })
        .collect();

    tracing::info!(
        session = %sid,
        total = results.len(),
        success = results.iter().filter(|r| r.success).count(),
        "批量删除完成"
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

    // 1. 将 hook_id 转为 memory_id，构造 (memory_id, MemoryUpdate) 列表
    let mut pairs: Vec<(String, hippocampus_core::model::MemoryUpdate, String, usize, usize, usize)> =
        Vec::new();
    for entry in &req.updates {
        let mid = retriever.find_memory_id_by_hook(&entry.hook_id).await;
        match mid {
            Some(memory_id) => {
                let added = entry.added_facts.len();
                let revised = entry.revised_facts.len();
                let deprecated = entry.deprecated_facts.len();
                let updates = hippocampus_core::model::MemoryUpdate::new()
                    .add_fact(entry.added_facts.join("\n"))
                    .revise_fact(entry.revised_facts.join("\n"))
                    .deprecate_fact(entry.deprecated_facts.join("\n"));
                pairs.push((memory_id, updates, entry.hook_id.clone(), added, revised, deprecated));
            }
            None => {
                // hook_id 无效的条目稍后处理为失败
                pairs.push((
                    String::new(),
                    hippocampus_core::model::MemoryUpdate::new(),
                    entry.hook_id.clone(),
                    0,
                    0,
                    0,
                ));
            }
        }
    }

    // 2. 过滤出有效的更新对，调用批量更新
    let valid_updates: Vec<(String, hippocampus_core::model::MemoryUpdate)> = pairs
        .iter()
        .filter(|(mid, _, _, _, _, _)| !mid.is_empty())
        .map(|(mid, upd, _, _, _, _)| (mid.clone(), upd.clone()))
        .collect();

    let update_results = if !valid_updates.is_empty() {
        storage.update_memories_batch(&valid_updates).await
    } else {
        Vec::new()
    };

    // 3. 构建响应
    let mut mid_to_result: std::collections::HashMap<String, &hippocampus_core::Result<()>> =
        std::collections::HashMap::new();
    let mut idx = 0;
    for (mid, _, _, _, _, _) in &pairs {
        if !mid.is_empty() && idx < update_results.len() {
            mid_to_result.insert(mid.clone(), &update_results[idx]);
            idx += 1;
        }
    }

    let results: Vec<BatchUpdateItem> = pairs
        .iter()
        .map(|(mid, _, hook_id, added, revised, deprecated)| {
            if mid.is_empty() {
                return BatchUpdateItem {
                    hook_id: hook_id.clone(),
                    success: false,
                    added: None,
                    revised: None,
                    deprecated: None,
                    error: Some("未找到对应的 memory_id".to_string()),
                };
            }
            match mid_to_result.get(mid) {
                Some(Ok(())) => BatchUpdateItem {
                    hook_id: hook_id.clone(),
                    success: true,
                    added: Some(*added),
                    revised: Some(*revised),
                    deprecated: Some(*deprecated),
                    error: None,
                },
                Some(Err(e)) => BatchUpdateItem {
                    hook_id: hook_id.clone(),
                    success: false,
                    added: None,
                    revised: None,
                    deprecated: None,
                    error: Some(e.to_string()),
                },
                None => BatchUpdateItem {
                    hook_id: hook_id.clone(),
                    success: false,
                    added: None,
                    revised: None,
                    deprecated: None,
                    error: Some("内部错误：结果缺失".to_string()),
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
/// 需要在服务启动时配置 SemanticRetriever（通过 AppState.retriever）。
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

    // 检查是否配置了 SemanticRetriever
    let retriever = match &state.retriever {
        Some(r) => r.clone(),
        None => {
            return Err(AppError::NotImplemented(
                "语义检索未配置：请通过环境变量配置 Embedder API 后重启服务".to_string(),
            ));
        }
    };

    // 调用检索器
    let results = retriever.search(&query, top_k).await?;

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
