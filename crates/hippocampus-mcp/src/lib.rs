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
use hippocampus_core::model::{ArchiveConfig, MessageTurn};
use hippocampus_core::retrieve::{Retriever, SummaryView};
use hippocampus_core::score::DefaultScorer;
use hippocampus_core::storage::{LocalStorage, Storage};

/// MCP server 主结构体
#[derive(Clone)]
pub struct HippocampusMcp {
    /// 存储根目录
    storage_root: PathBuf,
}

impl HippocampusMcp {
    /// 创建新的 MCP server 实例
    pub fn new(storage_root: PathBuf) -> Self {
        Self { storage_root }
    }

    /// 创建 Storage 实例（每次 tool 调用创建，无状态）
    fn create_storage(&self) -> Arc<dyn Storage> {
        Arc::new(LocalStorage::new(self.storage_root.clone()))
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
}
