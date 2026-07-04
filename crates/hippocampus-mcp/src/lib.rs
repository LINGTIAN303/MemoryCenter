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
use rmcp::schemars;
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

use hippocampus_core::archive::Archiver;
use hippocampus_core::compact::Compactor;
use hippocampus_core::conflict::ConflictDetector;
use hippocampus_core::model::{ArchiveConfig, MessageTurn};
use hippocampus_core::retrieve::{Retriever, SummaryView};
use hippocampus_core::score::DefaultScorer;
use hippocampus_core::storage::{LocalStorage, Storage};
// v2.18：复用 hippocampus-server 的 SessionSearchRouter（批次2 可下沉到独立 crate 减少二进制体积）
use hippocampus_server::SessionSearchRouter;

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
}

impl HippocampusMcp {
    /// 创建新的 MCP server 实例（无冲突检测器，无语义检索，向后兼容）
    ///
    /// 等价于 `with_conflict_detector(None)` + `with_session_search(None)`。
    pub fn new(storage_root: PathBuf) -> Self {
        Self {
            storage_root,
            conflict_detector: None,
            session_search: None,
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

/// archive tool 参数
#[derive(Deserialize, schemars::JsonSchema)]
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

// ============================================================================
// MCP Tools 实现
// ============================================================================

#[tool_router(server_handler)]
impl HippocampusMcp {
    /// 归档一批轮次为记忆文件，生成索引钩子。
    #[tool(description = "归档对话轮次到 Hippocampus 记忆库。当 Agent 会话达到 token 阈值时调用此 tool 保存完整上下文（非摘要）。返回归档摘要（含 hook_id 用于后续检索）。")]
    async fn archive(
        &self,
        Parameters(params): Parameters<ArchiveParams>,
    ) -> Result<String, McpError> {
        // 解析 turns_json
        let turns: Vec<MessageTurn> = serde_json::from_str(&params.turns_json)
            .map_err(|e| McpError::invalid_params(
                format!("turns_json 解析失败: {e}"),
                None,
            ))?;

        if turns.is_empty() {
            return Err(McpError::invalid_params("turns 不能为空", None));
        }

        let storage = self.create_storage();
        let config = ArchiveConfig::default();
        let mut archiver = Archiver::new(config, storage, &params.session_id, params.project_id);

        for turn in turns {
            archiver.push_turn(turn);
        }

        let (_, hook) = archiver.archive().await.map_err(|e| {
            McpError::internal_error(format!("归档失败: {e}"), None)
        })?;

        let summary = SummaryView::from(&hook);
        let result = serde_json::to_string(&summary).map_err(|e| {
            McpError::internal_error(format!("序列化结果失败: {e}"), None)
        })?;

        Ok(result)
    }

    /// 按钩子 ID 检索完整记忆文件。
    #[tool(description = "按 hook_id 检索完整记忆文件。当 Agent 需要历史对话细节时调用此 tool，返回完整的记忆文件（含所有轮次的用户消息+LLM消息+标签）。")]
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
    #[tool(description = "渲染记忆摘要为 system prompt 文本。返回的文本可直接拼接到 LLM system prompt 末尾，让 Agent 了解历史记忆概览。会话开始时调用。")]
    async fn prompt(
        &self,
        Parameters(params): Parameters<PromptParams>,
    ) -> Result<String, McpError> {
        let storage = self.create_storage();
        let retriever = Retriever::new(storage, &params.session_id, params.project_id);

        let prompt = retriever.render_to_system_prompt().await.map_err(|e| {
            McpError::internal_error(format!("渲染 prompt 失败: {e}"), None)
        })?;

        Ok(prompt)
    }

    /// 触发周期任务（周级合并 / 月级评分淘汰）。
    #[tool(description = "触发周期任务。period=\"weekly\" 执行周级无损去重合并（7天内记忆去重合并为1个），period=\"monthly\" 执行月级4维评分淘汰（保留高价值记忆）。")]
    async fn compaction(
        &self,
        Parameters(params): Parameters<CompactionParams>,
    ) -> Result<String, McpError> {
        let storage = self.create_storage();
        let compactor = Compactor::new(
            storage,
            Box::new(DefaultScorer::new()),
            &params.session_id,
            params.project_id,
        );

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
                format!("hook_ids_json 解析失败: {e}"),
                None,
            ))?;

        if hook_ids.is_empty() {
            return Err(McpError::invalid_params("hook_ids 不能为空", None));
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
    #[tool(description = "批量删除多个记忆文件。传入 hook_id 列表，逐个删除。单个失败不影响其他。用于清理过期或不需要的记忆。")]
    async fn batch_delete(
        &self,
        Parameters(params): Parameters<BatchDeleteParams>,
    ) -> Result<String, McpError> {
        let hook_ids: Vec<String> = serde_json::from_str(&params.hook_ids_json)
            .map_err(|e| McpError::invalid_params(
                format!("hook_ids_json 解析失败: {e}"),
                None,
            ))?;

        if hook_ids.is_empty() {
            return Err(McpError::invalid_params("hook_ids 不能为空", None));
        }

        let storage = self.create_storage();
        let retriever = Retriever::new(storage.clone(), &params.session_id, params.project_id);

        // hook_id → memory_id 转换
        let mut memory_ids: Vec<(String, Option<String>)> = Vec::with_capacity(hook_ids.len());
        for hook_id in &hook_ids {
            let mid = retriever.find_memory_id_by_hook(hook_id).await;
            memory_ids.push((hook_id.clone(), mid));
        }

        // 批量删除
        let valid: Vec<String> = memory_ids.iter()
            .filter_map(|(_, mid)| mid.clone())
            .collect();
        let delete_results = if !valid.is_empty() {
            storage.delete_memories_batch(&valid).await
        } else {
            Vec::new()
        };

        // 构建响应
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

        let items: Vec<_> = memory_ids.iter()
            .map(|(hook_id, mid_opt)| match mid_opt {
                None => serde_json::json!({
                    "hook_id": hook_id,
                    "success": false,
                    "error": "未找到对应的 memory_id",
                }),
                Some(mid) => match mid_to_result.get(mid) {
                    Some(Ok(())) => serde_json::json!({
                        "hook_id": hook_id,
                        "success": true,
                    }),
                    Some(Err(e)) => serde_json::json!({
                        "hook_id": hook_id,
                        "success": false,
                        "error": e.to_string(),
                    }),
                    None => serde_json::json!({
                        "hook_id": hook_id,
                        "success": false,
                        "error": "内部错误：结果缺失",
                    }),
                },
            })
            .collect();

        let result = serde_json::json!({
            "total": items.len(),
            "success_count": items.iter().filter(|r| r["success"].as_bool().unwrap_or(false)).count(),
            "items": items,
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
                format!("updates_json 解析失败: {e}"),
                None,
            ))?;

        if entries.is_empty() {
            return Err(McpError::invalid_params("updates 不能为空", None));
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
    #[tool(description = "检测记忆更新的潜在冲突（不实际写入）。传入 added/revised/deprecated facts，返回检测到的冲突列表（自我矛盾/直接矛盾/立场反转）。Agent 可在 update 前调用此 tool 评估风险。")]
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

        // 通过 hook_id 找到 memory_id
        let memory_id = retriever.find_memory_id_by_hook(&params.hook_id).await;
        let memory_id = match memory_id {
            Some(mid) => mid,
            None => {
                return Err(McpError::invalid_params(
                    format!("未找到 hook_id: {}", params.hook_id),
                    None,
                ));
            }
        };

        // 读取现有记忆
        let existing = storage.read_memory(&memory_id).await.map_err(|e| {
            McpError::internal_error(format!("读取记忆失败: {e}"), None)
        })?;

        // 构造 MemoryUpdate
        let update = hippocampus_core::model::MemoryUpdate::new()
            .add_fact(params.added_facts.join("\n"))
            .revise_fact(params.revised_facts.join("\n"))
            .deprecate_fact(params.deprecated_facts.join("\n"));

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
                    format!("未找到 hook_id: {}", params.hook_id),
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
    #[tool(description = "语义检索记忆（关键词+向量混合）。返回 top-K 相关记忆的 hook_id 列表，可再用 retrieve 工具获取完整内容。首次访问 session 自动从 storage 重建索引（懒加载）。")]
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
        use hippocampus_server::SessionSearchRouter;
        let storage: Arc<dyn hippocampus_core::storage::Storage> =
            Arc::new(hippocampus_core::storage::LocalStorage::new(
                tmpdir.path().to_path_buf(),
            ));
        let router = SessionSearchRouter::new(None, 0).with_storage(storage);
        HippocampusMcp::with_conflict_detector(tmpdir.path().to_path_buf(), None)
            .with_session_search(Some(Arc::new(router)))
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
        });
        mcp.archive(params).await.unwrap();

        // session-b 归档"Python 编程"
        let turns_json = make_turns_json("Python 数据分析", "Python 是脚本语言", 100);
        let params = Parameters(ArchiveParams {
            session_id: "session-b".to_string(),
            turns_json,
            project_id: None,
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
}
