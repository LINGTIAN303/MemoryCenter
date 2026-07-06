//! # HTTP API 全链路集成测试
//!
//! 启动真实 Axum HTTP 服务（随机端口），用 reqwest 客户端验证 5 个端点的全链路：
//! - archive → summaries → retrieve → prompt 全闭环
//! - 错误处理（空 turns / 不存在 hook_id / 无效 period）
//! - 周期任务全流程（weekly_merge / monthly_evict）
//! - 会话隔离 / 项目隔离

use hippocampus_server::{create_router, AppState};
use serde_json::{json, Value};
use std::path::PathBuf;
use tempfile::TempDir;
use tokio::net::TcpListener;

// ============================================================================
// 测试辅助
// ============================================================================

/// 测试用 HTTP 服务句柄
///
/// 持有临时目录（防止存储被清理）和服务端任务句柄
struct TestServer {
    /// 基础 URL（如 http://127.0.0.1:54321）
    base_url: String,
    /// 临时存储目录（drop 时自动清理）
    _tmpdir: TempDir,
}

impl TestServer {
    /// 启动一个新的测试服务（随机端口 + 独立临时目录）
    async fn start() -> Self {
        Self::start_with_session_search(None, None).await
    }

    /// 启动带 session_search 的测试服务（v2.8）
    async fn start_with_session_search(
        session_search: Option<std::sync::Arc<hippocampus_server::SessionSearchRouter>>,
        conflict_detector: Option<
            std::sync::Arc<dyn hippocampus_core::conflict::ConflictDetector>,
        >,
    ) -> Self {
        let tmpdir = TempDir::new().expect("创建临时目录失败");
        let storage_root: PathBuf = tmpdir.path().to_path_buf();

        let state = AppState {
            storage_root,
            session_search,
            conflict_detector,
            summary_generator: None,
        };
        let app = create_router(state);

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("绑定端口失败");
        let addr = listener.local_addr().expect("获取地址失败");
        let base_url = format!("http://{}", addr);

        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("服务异常退出");
        });

        Self {
            base_url,
            _tmpdir: tmpdir,
        }
    }

    /// 启动带冲突检测器的测试服务（v2.6 批次 8）
    async fn start_with_detector(
        conflict_detector: Option<
            std::sync::Arc<dyn hippocampus_core::conflict::ConflictDetector>,
        >,
    ) -> Self {
        Self::start_with_session_search(None, conflict_detector).await
    }

    /// 拼接完整 URL
    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }
}

/// 构造一个最小合法 MessageTurn JSON
fn make_turn_json(user_text: &str, llm_text: &str, tokens: usize) -> Value {
    json!({
        "id": uuid::Uuid::new_v4().to_string(),
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
    })
}

/// 构造一批 turns（n 个）
fn make_turns_json(n: usize, base_tokens: usize) -> Vec<Value> {
    (0..n)
        .map(|i| {
            make_turn_json(
                &format!("用户消息 #{}", i),
                &format!("助手回复 #{}", i),
                base_tokens + i,
            )
        })
        .collect()
}

// ============================================================================
// 基础端点测试
// ============================================================================

#[tokio::test]
async fn test_archive_success() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let body = json!({
        "turns": make_turns_json(3, 100),
        "project_id": null
    });

    let resp = client
        .post(server.url("/api/v1/sessions/sess-1/archive"))
        .json(&body)
        .send()
        .await
        .expect("请求失败");

    assert_eq!(resp.status(), 200);

    let summary: Value = resp.json().await.expect("解析响应失败");
    assert!(!summary["hook_id"].as_str().unwrap().is_empty());
    assert!(!summary["memory_id"].as_str().unwrap().is_empty());
    assert_eq!(summary["period"].as_str().unwrap(), "daily");
    assert_eq!(summary["token_count"].as_u64().unwrap(), 303); // 100+101+102
}

#[tokio::test]
async fn test_archive_empty_turns_returns_400() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let body = json!({ "turns": [], "project_id": null });

    let resp = client
        .post(server.url("/api/v1/sessions/sess-1/archive"))
        .json(&body)
        .send()
        .await
        .expect("请求失败");

    assert_eq!(resp.status(), 400);
    let err: Value = resp.json().await.expect("解析错误响应失败");
    assert_eq!(err["error"]["code"].as_str().unwrap(), "BAD_REQUEST");
    assert!(err["error"]["message"]
        .as_str()
        .unwrap()
        .contains("turns 不能为空"));
}

#[tokio::test]
async fn test_summaries_empty_session() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(server.url("/api/v1/sessions/never-exist/summaries"))
        .send()
        .await
        .expect("请求失败");

    assert_eq!(resp.status(), 200);
    let arr: Vec<Value> = resp.json().await.expect("解析响应失败");
    assert!(arr.is_empty());
}

#[tokio::test]
async fn test_summaries_after_archive() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    // 归档 2 次
    for _ in 0..2 {
        let body = json!({
            "turns": make_turns_json(2, 100),
            "project_id": null
        });
        client
            .post(server.url("/api/v1/sessions/sess-a/archive"))
            .json(&body)
            .send()
            .await
            .expect("请求失败");
    }

    let resp = client
        .get(server.url("/api/v1/sessions/sess-a/summaries"))
        .send()
        .await
        .expect("请求失败");

    assert_eq!(resp.status(), 200);
    let arr: Vec<Value> = resp.json().await.expect("解析响应失败");
    assert_eq!(arr.len(), 2);
    // 所有摘要都应是 daily 周期
    for s in &arr {
        assert_eq!(s["period"].as_str().unwrap(), "daily");
    }
}

// ============================================================================
// 检索测试
// ============================================================================

#[tokio::test]
async fn test_retrieve_memory_full_chain() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    // 1. 归档
    let body = json!({
        "turns": make_turns_json(3, 50),
        "project_id": null
    });
    let resp = client
        .post(server.url("/api/v1/sessions/sess-r/archive"))
        .json(&body)
        .send()
        .await
        .expect("请求失败");
    let summary: Value = resp.json().await.expect("解析响应失败");
    let hook_id = summary["hook_id"].as_str().unwrap();

    // 2. 通过 hook_id 检索完整记忆
    let url = format!("/api/v1/sessions/sess-r/memories/{}", hook_id);
    let resp = client
        .get(server.url(&url))
        .send()
        .await
        .expect("请求失败");

    assert_eq!(resp.status(), 200);
    let memory: Value = resp.json().await.expect("解析响应失败");
    assert_eq!(memory["turns"].as_array().unwrap().len(), 3);
    assert_eq!(memory["session_id"].as_str().unwrap(), "sess-r");
    assert_eq!(memory["total_tokens"].as_u64().unwrap(), 153); // 50+51+52
}

#[tokio::test]
async fn test_retrieve_nonexistent_hook_returns_404() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let fake_id = uuid::Uuid::new_v4().to_string();
    let url = format!("/api/v1/sessions/sess-x/memories/{}", fake_id);
    let resp = client
        .get(server.url(&url))
        .send()
        .await
        .expect("请求失败");

    assert_eq!(resp.status(), 404);
    let err: Value = resp.json().await.expect("解析错误响应失败");
    assert_eq!(err["error"]["code"].as_str().unwrap(), "NOT_FOUND");
    assert!(err["error"]["message"]
        .as_str()
        .unwrap()
        .contains("不存在"));
}

// ============================================================================
// Prompt 渲染测试
// ============================================================================

#[tokio::test]
async fn test_render_prompt_empty() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(server.url("/api/v1/sessions/empty-sess/prompt"))
        .send()
        .await
        .expect("请求失败");

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("解析响应失败");
    assert_eq!(body["prompt"].as_str().unwrap(), "");
}

#[tokio::test]
async fn test_render_prompt_with_memory() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    // 归档一次
    let body = json!({
        "turns": make_turns_json(2, 100),
        "project_id": null
    });
    client
        .post(server.url("/api/v1/sessions/sess-p/archive"))
        .json(&body)
        .send()
        .await
        .expect("请求失败");

    // 渲染 prompt
    let resp = client
        .get(server.url("/api/v1/sessions/sess-p/prompt"))
        .send()
        .await
        .expect("请求失败");

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("解析响应失败");
    let prompt = body["prompt"].as_str().unwrap();
    assert!(!prompt.is_empty());
    assert!(prompt.contains("可用记忆索引"));
    assert!(prompt.contains("近期记忆"));
}

// ============================================================================
// 周期任务测试
// ============================================================================

#[tokio::test]
async fn test_compaction_invalid_period_returns_400() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let body = json!({ "period": "yearly", "project_id": null });
    let resp = client
        .post(server.url("/api/v1/sessions/sess-c/compaction"))
        .json(&body)
        .send()
        .await
        .expect("请求失败");

    assert_eq!(resp.status(), 400);
    let err: Value = resp.json().await.expect("解析错误响应失败");
    assert_eq!(err["error"]["code"].as_str().unwrap(), "BAD_REQUEST");
    assert!(err["error"]["message"]
        .as_str()
        .unwrap()
        .contains("无效的 period"));
}

#[tokio::test]
async fn test_compaction_weekly_without_daily_returns_500() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    // 无任何归档，直接 weekly_merge
    let body = json!({ "period": "weekly", "project_id": null });
    let resp = client
        .post(server.url("/api/v1/sessions/sess-w/compaction"))
        .json(&body)
        .send()
        .await
        .expect("请求失败");

    assert_eq!(resp.status(), 500);
    let err: Value = resp.json().await.expect("解析错误响应失败");
    assert_eq!(err["error"]["code"].as_str().unwrap(), "INTERNAL_ERROR");
}

#[tokio::test]
async fn test_compaction_full_workflow() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    // 1. 归档多次（产生 daily 记忆）
    for _ in 0..3 {
        let body = json!({
            "turns": make_turns_json(2, 100),
            "project_id": null
        });
        let resp = client
            .post(server.url("/api/v1/sessions/sess-fw/archive"))
            .json(&body)
            .send()
            .await
            .expect("请求失败");
        assert_eq!(resp.status(), 200);
    }

    // 2. 周级合并
    let body = json!({ "period": "weekly", "project_id": null });
    let resp = client
        .post(server.url("/api/v1/sessions/sess-fw/compaction"))
        .json(&body)
        .send()
        .await
        .expect("请求失败");
    assert_eq!(resp.status(), 200);
    let weekly_result: Value = resp.json().await.expect("解析响应失败");
    assert_eq!(weekly_result["period"].as_str().unwrap(), "weekly");
    assert!(weekly_result["total_turns"].as_u64().unwrap() != 0);
    assert!(weekly_result["hooks_count"].as_u64().unwrap() != 0);

    // 3. 再归档几次产生多个 weekly（用于月级淘汰）
    // 注意：monthly_evict 需要至少 1 个 weekly 文件，这里已有 1 个
    let body = json!({ "period": "monthly", "project_id": null });
    let resp = client
        .post(server.url("/api/v1/sessions/sess-fw/compaction"))
        .json(&body)
        .send()
        .await
        .expect("请求失败");
    assert_eq!(resp.status(), 200);
    let monthly_result: Value = resp.json().await.expect("解析响应失败");
    assert_eq!(monthly_result["period"].as_str().unwrap(), "monthly");
    assert!(monthly_result["total_turns"].as_u64().unwrap() != 0);
}

// ============================================================================
// 隔离性测试
// ============================================================================

#[tokio::test]
async fn test_session_isolation() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    // 会话 A 归档
    let body = json!({ "turns": make_turns_json(2, 100), "project_id": null });
    client
        .post(server.url("/api/v1/sessions/sess-iso-a/archive"))
        .json(&body)
        .send()
        .await
        .expect("请求失败");

    // 会话 B 查 summaries 应为空
    let resp = client
        .get(server.url("/api/v1/sessions/sess-iso-b/summaries"))
        .send()
        .await
        .expect("请求失败");
    let arr: Vec<Value> = resp.json().await.expect("解析响应失败");
    assert!(arr.is_empty(), "会话 B 不应看到会话 A 的记忆");

    // 会话 A 查 summaries 应有 1 个
    let resp = client
        .get(server.url("/api/v1/sessions/sess-iso-a/summaries"))
        .send()
        .await
        .expect("请求失败");
    let arr: Vec<Value> = resp.json().await.expect("解析响应失败");
    assert_eq!(arr.len(), 1);
}

#[tokio::test]
async fn test_project_id_isolation() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    // project-A 归档
    let body = json!({
        "turns": make_turns_json(2, 100),
        "project_id": "proj-a"
    });
    client
        .post(server.url("/api/v1/sessions/sess-proj/archive"))
        .json(&body)
        .send()
        .await
        .expect("请求失败");

    // project-B 查 summaries 应为空
    let resp = client
        .get(server.url("/api/v1/sessions/sess-proj/summaries?project_id=proj-b"))
        .send()
        .await
        .expect("请求失败");
    let arr: Vec<Value> = resp.json().await.expect("解析响应失败");
    assert!(arr.is_empty(), "project-B 不应看到 project-A 的记忆");

    // project-A 查 summaries 应有 1 个
    let resp = client
        .get(server.url("/api/v1/sessions/sess-proj/summaries?project_id=proj-a"))
        .send()
        .await
        .expect("请求失败");
    let arr: Vec<Value> = resp.json().await.expect("解析响应失败");
    assert_eq!(arr.len(), 1);
}

// ============================================================================
// 完整工作流测试
// ============================================================================

#[tokio::test]
async fn test_full_agent_workflow() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();
    let sid = "agent-full";

    // 1. 模拟 Agent 一轮对话：归档
    let body = json!({
        "turns": make_turns_json(5, 200),
        "project_id": "demo-project"
    });
    let resp = client
        .post(server.url(&format!("/api/v1/sessions/{}/archive", sid)))
        .json(&body)
        .send()
        .await
        .expect("归档失败");
    assert_eq!(resp.status(), 200);
    let summary: Value = resp.json().await.expect("解析摘要失败");
    let hook_id = summary["hook_id"].as_str().unwrap().to_string();

    // 2. 获取摘要列表（注入 system prompt 用）
    let resp = client
        .get(server.url(&format!("/api/v1/sessions/{}/summaries?project_id=demo-project", sid)))
        .send()
        .await
        .expect("获取摘要失败");
    assert_eq!(resp.status(), 200);
    let summaries: Vec<Value> = resp.json().await.expect("解析摘要失败");
    assert_eq!(summaries.len(), 1);

    // 3. 渲染 system prompt
    let resp = client
        .get(server.url(&format!("/api/v1/sessions/{}/prompt?project_id=demo-project", sid)))
        .send()
        .await
        .expect("渲染 prompt 失败");
    assert_eq!(resp.status(), 200);
    let prompt_body: Value = resp.json().await.expect("解析 prompt 失败");
    assert!(prompt_body["prompt"].as_str().unwrap().contains("可用记忆索引"));

    // 4. LLM 通过 tool 主动检索详细记忆
    let resp = client
        .get(server.url(&format!(
            "/api/v1/sessions/{}/memories/{}?project_id=demo-project",
            sid, hook_id
        )))
        .send()
        .await
        .expect("检索记忆失败");
    assert_eq!(resp.status(), 200);
    let memory: Value = resp.json().await.expect("解析记忆失败");
    assert_eq!(memory["turns"].as_array().unwrap().len(), 5);
    assert_eq!(memory["session_id"].as_str().unwrap(), sid);
}

// ============================================================================
// v2.4 批次 3：记忆迭代更新测试
// ============================================================================

#[tokio::test]
async fn test_update_memory_success() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    // 1. 先归档一次获取 hook_id
    let body = json!({
        "turns": make_turns_json(2, 100),
        "project_id": null
    });
    let resp = client
        .post(server.url("/api/v1/sessions/sess-upd/archive"))
        .json(&body)
        .send()
        .await
        .expect("归档失败");
    let summary: Value = resp.json().await.expect("解析摘要失败");
    let hook_id = summary["hook_id"].as_str().unwrap();

    // 2. 调用 PATCH 更新记忆
    let update_body = json!({
        "added_facts": ["新事实：v2.4 批次 3 完成"],
        "revised_facts": ["修正：原计划改为 v2.4"],
        "deprecated_facts": ["废弃：旧逻辑已过时"],
        "project_id": null
    });
    let resp = client
        .patch(server.url(&format!(
            "/api/v1/sessions/sess-upd/memories/{}",
            hook_id
        )))
        .json(&update_body)
        .send()
        .await
        .expect("更新失败");

    assert_eq!(resp.status(), 200);
    let result: Value = resp.json().await.expect("解析响应失败");
    assert_eq!(result["success"], true);
    assert_eq!(result["added"], 1);
    assert_eq!(result["revised"], 1);
    assert_eq!(result["deprecated"], 1);

    // 3. 检索验证 updates 字段已更新（v2.4 风险点修复：独立存储）
    let resp = client
        .get(server.url(&format!(
            "/api/v1/sessions/sess-upd/memories/{}",
            hook_id
        )))
        .send()
        .await
        .expect("检索失败");
    let memory: Value = resp.json().await.expect("解析记忆失败");

    // 验证 updates 字段（独立存储，不污染 turns）
    let updates = memory["updates"].as_array().expect("updates 应为数组");
    assert_eq!(updates.len(), 1, "应有 1 条更新记录");
    assert!(
        updates[0]["updated_at"].as_str().is_some(),
        "更新记录应含 updated_at"
    );
    assert_eq!(
        updates[0]["added_facts"][0],
        "新事实：v2.4 批次 3 完成"
    );
    assert_eq!(
        updates[0]["revised_facts"][0],
        "修正：原计划改为 v2.4"
    );
    assert_eq!(
        updates[0]["deprecated_facts"][0],
        "废弃：旧逻辑已过时"
    );

    // 验证原始 turns.text 未被污染
    let first_turn_text = memory["turns"][0]["user_message"]["text"]
        .as_str()
        .unwrap();
    assert!(
        !first_turn_text.contains("[新增事实]"),
        "原始 text 不应包含 update 标记"
    );
}

#[tokio::test]
async fn test_update_memory_empty_returns_400() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    // 先归档
    let body = json!({
        "turns": make_turns_json(1, 100),
        "project_id": null
    });
    let resp = client
        .post(server.url("/api/v1/sessions/sess-empty-upd/archive"))
        .json(&body)
        .send()
        .await
        .expect("归档失败");
    let summary: Value = resp.json().await.expect("解析摘要失败");
    let hook_id = summary["hook_id"].as_str().unwrap();

    // 空更新应返回 400
    let update_body = json!({
        "added_facts": [],
        "revised_facts": [],
        "deprecated_facts": [],
        "project_id": null
    });
    let resp = client
        .patch(server.url(&format!(
            "/api/v1/sessions/sess-empty-upd/memories/{}",
            hook_id
        )))
        .json(&update_body)
        .send()
        .await
        .expect("请求失败");

    assert_eq!(resp.status(), 400);
    let err: Value = resp.json().await.expect("解析错误失败");
    assert_eq!(err["error"]["code"].as_str().unwrap(), "BAD_REQUEST");
}

#[tokio::test]
async fn test_update_memory_nonexistent_hook_returns_404() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let fake_id = uuid::Uuid::new_v4().to_string();
    let update_body = json!({
        "added_facts": ["test fact"],
        "project_id": null
    });
    let resp = client
        .patch(server.url(&format!(
            "/api/v1/sessions/sess-x/memories/{}",
            fake_id
        )))
        .json(&update_body)
        .send()
        .await
        .expect("请求失败");

    assert_eq!(resp.status(), 404);
    let err: Value = resp.json().await.expect("解析错误失败");
    assert_eq!(err["error"]["code"].as_str().unwrap(), "NOT_FOUND");
}

// ============================================================================
// 批量操作端点测试（v2.5 批次 6）
// ============================================================================

/// 辅助：归档并返回 hook_id
async fn archive_and_get_hook(server: &TestServer, client: &reqwest::Client, sid: &str) -> String {
    let body = json!({
        "turns": make_turns_json(2, 100),
        "project_id": null
    });
    let resp = client
        .post(server.url(&format!("/api/v1/sessions/{}/archive", sid)))
        .json(&body)
        .send()
        .await
        .expect("归档失败");
    let summary: Value = resp.json().await.expect("解析摘要失败");
    summary["hook_id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn test_batch_retrieve_success() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    // 归档 3 次获取 3 个 hook_id
    let h1 = archive_and_get_hook(&server, &client, "sess-br").await;
    let h2 = archive_and_get_hook(&server, &client, "sess-br").await;
    let h3 = archive_and_get_hook(&server, &client, "sess-br").await;

    // 批量检索
    let body = json!({
        "hook_ids": [h1, h2, h3],
        "project_id": null
    });
    let resp = client
        .post(server.url("/api/v1/sessions/sess-br/memories/batch-retrieve"))
        .json(&body)
        .send()
        .await
        .expect("请求失败");

    assert_eq!(resp.status(), 200);
    let results: Value = resp.json().await.expect("解析响应失败");
    let arr = results.as_array().expect("应为数组");
    assert_eq!(arr.len(), 3, "应返回 3 条结果");
    for item in arr {
        assert_eq!(item["success"], true, "全部应成功");
        assert!(
            item["data"]["turns"].as_array().is_some(),
            "应有 data.turns 字段"
        );
    }
}

#[tokio::test]
async fn test_batch_retrieve_partial_failure() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let good = archive_and_get_hook(&server, &client, "sess-brpf").await;
    let bad = uuid::Uuid::new_v4().to_string();

    let body = json!({
        "hook_ids": [good.clone(), bad, good.clone()],
        "project_id": null
    });
    let resp = client
        .post(server.url("/api/v1/sessions/sess-brpf/memories/batch-retrieve"))
        .json(&body)
        .send()
        .await
        .expect("请求失败");

    assert_eq!(resp.status(), 200);
    let results: Value = resp.json().await.expect("解析响应失败");
    let arr = results.as_array().unwrap();
    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0]["success"], true, "第 1 个应成功");
    assert_eq!(arr[1]["success"], false, "第 2 个应失败");
    assert!(
        arr[1]["error"].as_str().is_some(),
        "失败项应有 error 字段"
    );
    assert_eq!(arr[2]["success"], true, "第 3 个应成功（不受前一个影响）");
}

#[tokio::test]
async fn test_batch_retrieve_empty_returns_400() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let body = json!({
        "hook_ids": [],
        "project_id": null
    });
    let resp = client
        .post(server.url("/api/v1/sessions/sess-x/memories/batch-retrieve"))
        .json(&body)
        .send()
        .await
        .expect("请求失败");

    assert_eq!(resp.status(), 400);
    let err: Value = resp.json().await.expect("解析错误失败");
    assert_eq!(err["error"]["code"].as_str().unwrap(), "BAD_REQUEST");
}

#[tokio::test]
async fn test_batch_delete_success() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let h1 = archive_and_get_hook(&server, &client, "sess-bd").await;
    let h2 = archive_and_get_hook(&server, &client, "sess-bd").await;
    let h3 = archive_and_get_hook(&server, &client, "sess-bd").await;

    // 批量删除前 2 个
    let body = json!({
        "hook_ids": [h1, h2],
        "project_id": null
    });
    let resp = client
        .post(server.url("/api/v1/sessions/sess-bd/memories/batch-delete"))
        .json(&body)
        .send()
        .await
        .expect("请求失败");

    assert_eq!(resp.status(), 200);
    let results: Value = resp.json().await.expect("解析响应失败");
    let arr = results.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert!(arr.iter().all(|i| i["success"] == true));

    // 验证 h3 仍然可检索（不受删除影响）
    let resp = client
        .get(server.url(&format!(
            "/api/v1/sessions/sess-bd/memories/{}",
            h3
        )))
        .send()
        .await
        .expect("请求失败");
    assert_eq!(resp.status(), 200, "h3 应仍可检索");

    // 验证 h1 已删除
    let resp = client
        .get(server.url(&format!(
            "/api/v1/sessions/sess-bd/memories/{}",
            h1
        )))
        .send()
        .await
        .expect("请求失败");
    assert_eq!(resp.status(), 404, "h1 应已被删除");
}

#[tokio::test]
async fn test_batch_delete_partial_failure() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let good = archive_and_get_hook(&server, &client, "sess-bdpf").await;
    let bad = uuid::Uuid::new_v4().to_string();

    let body = json!({
        "hook_ids": [good.clone(), bad],
        "project_id": null
    });
    let resp = client
        .post(server.url("/api/v1/sessions/sess-bdpf/memories/batch-delete"))
        .json(&body)
        .send()
        .await
        .expect("请求失败");

    assert_eq!(resp.status(), 200);
    let results: Value = resp.json().await.expect("解析响应失败");
    let arr = results.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["success"], true, "存在的应删除成功");
    assert_eq!(arr[1]["success"], false, "不存在的应返回错误");
}

#[tokio::test]
async fn test_batch_update_success() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let h1 = archive_and_get_hook(&server, &client, "sess-bu").await;
    let h2 = archive_and_get_hook(&server, &client, "sess-bu").await;

    let body = json!({
        "updates": [
            {
                "hook_id": h1,
                "added_facts": ["事实 A"],
                "revised_facts": [],
                "deprecated_facts": []
            },
            {
                "hook_id": h2,
                "added_facts": ["事实 B"],
                "revised_facts": ["修正 X"],
                "deprecated_facts": ["废弃 Y"]
            }
        ],
        "project_id": null
    });
    let resp = client
        .post(server.url("/api/v1/sessions/sess-bu/memories/batch-update"))
        .json(&body)
        .send()
        .await
        .expect("请求失败");

    assert_eq!(resp.status(), 200);
    let results: Value = resp.json().await.expect("解析响应失败");
    let arr = results.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["success"], true);
    assert_eq!(arr[0]["added"], 1);
    assert_eq!(arr[1]["success"], true);
    assert_eq!(arr[1]["added"], 1);
    assert_eq!(arr[1]["revised"], 1);
    assert_eq!(arr[1]["deprecated"], 1);

    // 验证 h1 的更新已应用
    let resp = client
        .get(server.url(&format!(
            "/api/v1/sessions/sess-bu/memories/{}",
            h1
        )))
        .send()
        .await
        .expect("请求失败");
    let memory: Value = resp.json().await.expect("解析失败");
    let updates = memory["updates"].as_array().unwrap();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0]["added_facts"][0], "事实 A");
}

#[tokio::test]
async fn test_batch_update_partial_failure() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let good = archive_and_get_hook(&server, &client, "sess-bupf").await;
    let bad = uuid::Uuid::new_v4().to_string();

    let body = json!({
        "updates": [
            {
                "hook_id": good,
                "added_facts": ["OK"],
                "revised_facts": [],
                "deprecated_facts": []
            },
            {
                "hook_id": bad,
                "added_facts": ["FAIL"],
                "revised_facts": [],
                "deprecated_facts": []
            }
        ],
        "project_id": null
    });
    let resp = client
        .post(server.url("/api/v1/sessions/sess-bupf/memories/batch-update"))
        .json(&body)
        .send()
        .await
        .expect("请求失败");

    assert_eq!(resp.status(), 200);
    let results: Value = resp.json().await.expect("解析响应失败");
    let arr = results.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["success"], true, "存在的应更新成功");
    assert_eq!(arr[1]["success"], false, "不存在的应返回错误");
}

#[tokio::test]
async fn test_batch_update_empty_returns_400() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let body = json!({
        "updates": [],
        "project_id": null
    });
    let resp = client
        .post(server.url("/api/v1/sessions/sess-x/memories/batch-update"))
        .json(&body)
        .send()
        .await
        .expect("请求失败");

    assert_eq!(resp.status(), 400);
    let err: Value = resp.json().await.expect("解析错误失败");
    assert_eq!(err["error"]["code"].as_str().unwrap(), "BAD_REQUEST");
}

// ============================================================================
// 语义检索端点测试（v2.5 批次 7）
// ============================================================================

#[tokio::test]
async fn test_search_without_session_search_returns_501() {
    // 默认 TestServer 未配置 session_search
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let body = json!({ "query": "Rust 编程", "top_k": 5 });
    let resp = client
        .post(server.url("/api/v1/sessions/sess-s1/search"))
        .json(&body)
        .send()
        .await
        .expect("请求失败");

    assert_eq!(resp.status(), 501);
    let err: Value = resp.json().await.expect("解析错误失败");
    assert_eq!(err["error"]["code"].as_str().unwrap(), "NOT_IMPLEMENTED");
    assert!(err["error"]["message"]
        .as_str()
        .unwrap()
        .contains("语义检索未配置"));
}

#[tokio::test]
async fn test_search_empty_query_returns_400() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    // 空字符串
    let body = json!({ "query": "", "top_k": 5 });
    let resp = client
        .post(server.url("/api/v1/sessions/sess-s2/search"))
        .json(&body)
        .send()
        .await
        .expect("请求失败");
    assert_eq!(resp.status(), 400);

    // 纯空白
    let body = json!({ "query": "   ", "top_k": 5 });
    let resp = client
        .post(server.url("/api/v1/sessions/sess-s2/search"))
        .json(&body)
        .send()
        .await
        .expect("请求失败");
    assert_eq!(resp.status(), 400);

    let err: Value = resp.json().await.expect("解析错误失败");
    assert_eq!(err["error"]["code"].as_str().unwrap(), "BAD_REQUEST");
    assert!(err["error"]["message"]
        .as_str()
        .unwrap()
        .contains("query 不能为空"));
}

// ============================================================================
// v2.6 批次 8：冲突检测端到端测试
// ============================================================================

/// 辅助：归档一条记忆，返回 hook_id
async fn archive_one(server: &TestServer, client: &reqwest::Client, sid: &str) -> String {
    let body = json!({
        "turns": make_turns_json(2, 100),
        "project_id": null
    });
    let resp = client
        .post(server.url(&format!("/api/v1/sessions/{}/archive", sid)))
        .json(&body)
        .send()
        .await
        .expect("归档失败");
    let summary: Value = resp.json().await.expect("解析失败");
    summary["hook_id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn test_update_without_detector_returns_zero_conflicts() {
    // 未配置冲突检测器时，update 响应应返回 conflicts=0, has_critical=false
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let hook_id = archive_one(&server, &client, "sess-no-det").await;

    let body = json!({
        "added_facts": ["用户喜欢咖啡"],
        "revised_facts": [],
        "deprecated_facts": [],
        "project_id": null,
    });
    let url = format!("/api/v1/sessions/sess-no-det/memories/{}", hook_id);
    let resp = client
        .patch(server.url(&url))
        .json(&body)
        .send()
        .await
        .expect("更新失败");

    assert_eq!(resp.status(), 200);
    let result: Value = resp.json().await.expect("解析失败");
    assert_eq!(result["success"], true);
    assert_eq!(result["conflicts"], 0);
    assert_eq!(result["has_critical"], false);
}

#[tokio::test]
async fn test_update_with_detector_detects_direct_contradiction() {
    // 配置 HeuristicDetector，添加与历史事实矛盾的新事实 → 应检测到 DirectContradict
    let detector: std::sync::Arc<dyn hippocampus_core::conflict::ConflictDetector> =
        std::sync::Arc::new(hippocampus_core::heuristic::HeuristicDetector::new());
    let server = TestServer::start_with_detector(Some(detector)).await;
    let client = reqwest::Client::new();

    let hook_id = archive_one(&server, &client, "sess-detect").await;

    // 第一次 update：添加"用户喜欢咖啡"
    let body = json!({
        "added_facts": ["用户喜欢咖啡"],
        "project_id": null,
    });
    let url = format!("/api/v1/sessions/sess-detect/memories/{}", hook_id);
    client.patch(server.url(&url)).json(&body).send().await.unwrap();

    // 第二次 update：添加"用户不喜欢咖啡" → 应检测到 DirectContradict
    let body = json!({
        "added_facts": ["用户不喜欢咖啡"],
        "project_id": null,
    });
    let resp = client
        .patch(server.url(&url))
        .json(&body)
        .send()
        .await
        .expect("更新失败");

    assert_eq!(resp.status(), 200);
    let result: Value = resp.json().await.expect("解析失败");
    assert_eq!(result["success"], true);
    assert!(result["conflicts"].as_u64().unwrap() >= 1, "应检测到至少 1 个冲突");
    assert_eq!(result["has_critical"], true);
}

#[tokio::test]
async fn test_update_with_detector_clean_update() {
    // 无冲突的更新应返回 conflicts=0
    let detector: std::sync::Arc<dyn hippocampus_core::conflict::ConflictDetector> =
        std::sync::Arc::new(hippocampus_core::heuristic::HeuristicDetector::new());
    let server = TestServer::start_with_detector(Some(detector)).await;
    let client = reqwest::Client::new();

    let hook_id = archive_one(&server, &client, "sess-clean").await;

    let body = json!({
        "added_facts": ["用户住在上海"],
        "project_id": null,
    });
    let url = format!("/api/v1/sessions/sess-clean/memories/{}", hook_id);
    let resp = client
        .patch(server.url(&url))
        .json(&body)
        .send()
        .await
        .expect("更新失败");

    assert_eq!(resp.status(), 200);
    let result: Value = resp.json().await.expect("解析失败");
    assert_eq!(result["conflicts"], 0);
    assert_eq!(result["has_critical"], false);
}

#[tokio::test]
async fn test_get_conflicts_returns_persisted_records() {
    // 验证 GET /conflicts 端点能返回持久化的冲突记录
    let detector: std::sync::Arc<dyn hippocampus_core::conflict::ConflictDetector> =
        std::sync::Arc::new(hippocampus_core::heuristic::HeuristicDetector::new());
    let server = TestServer::start_with_detector(Some(detector)).await;
    let client = reqwest::Client::new();

    let hook_id = archive_one(&server, &client, "sess-get-conf").await;

    // 添加"用户喜欢咖啡"
    let url = format!("/api/v1/sessions/sess-get-conf/memories/{}", hook_id);
    client
        .patch(server.url(&url))
        .json(&json!({ "added_facts": ["用户喜欢咖啡"], "project_id": null }))
        .send()
        .await
        .unwrap();

    // 添加"用户不喜欢咖啡"（产生冲突）
    client
        .patch(server.url(&url))
        .json(&json!({ "added_facts": ["用户不喜欢咖啡"], "project_id": null }))
        .send()
        .await
        .unwrap();

    // GET 查询冲突
    let conflicts_url = format!(
        "/api/v1/sessions/sess-get-conf/memories/{}/conflicts",
        hook_id
    );
    let resp = client
        .get(server.url(&conflicts_url))
        .send()
        .await
        .expect("查询失败");

    assert_eq!(resp.status(), 200);
    let result: Value = resp.json().await.expect("解析失败");
    assert!(result["total"].as_u64().unwrap() >= 1, "应有至少 1 个冲突");
    assert!(result["critical_count"].as_u64().unwrap() >= 1, "应有至少 1 个 Critical");

    let conflicts = result["conflicts"].as_array().unwrap();
    let has_direct = conflicts
        .iter()
        .any(|c| c["kind"] == "direct_contradict");
    assert!(has_direct, "应包含 direct_contradict 类型冲突");
}

#[tokio::test]
async fn test_get_conflicts_nonexistent_hook_returns_404() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let fake_id = uuid::Uuid::new_v4().to_string();
    let url = format!("/api/v1/sessions/sess-x/memories/{}/conflicts", fake_id);
    let resp = client.get(server.url(&url)).send().await.expect("请求失败");

    assert_eq!(resp.status(), 404);
    let err: Value = resp.json().await.expect("解析失败");
    assert_eq!(err["error"]["code"].as_str().unwrap(), "NOT_FOUND");
}

#[tokio::test]
async fn test_get_conflicts_no_conflicts_returns_empty() {
    // 无冲突的记忆，GET /conflicts 应返回 total=0
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let hook_id = archive_one(&server, &client, "sess-empty-conf").await;

    // 添加无冲突的事实
    let url = format!("/api/v1/sessions/sess-empty-conf/memories/{}", hook_id);
    client
        .patch(server.url(&url))
        .json(&json!({ "added_facts": ["普通事实"], "project_id": null }))
        .send()
        .await
        .unwrap();

    let conflicts_url = format!(
        "/api/v1/sessions/sess-empty-conf/memories/{}/conflicts",
        hook_id
    );
    let resp = client
        .get(server.url(&conflicts_url))
        .send()
        .await
        .expect("查询失败");

    assert_eq!(resp.status(), 200);
    let result: Value = resp.json().await.expect("解析失败");
    assert_eq!(result["total"], 0);
    assert_eq!(result["critical_count"], 0);
    assert!(result["conflicts"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_update_with_self_contradiction() {
    // 同一批 update 中 added 和 deprecated 包含相同事实 → SelfContradict
    let detector: std::sync::Arc<dyn hippocampus_core::conflict::ConflictDetector> =
        std::sync::Arc::new(hippocampus_core::heuristic::HeuristicDetector::new());
    let server = TestServer::start_with_detector(Some(detector)).await;
    let client = reqwest::Client::new();

    let hook_id = archive_one(&server, &client, "sess-self-c").await;

    let body = json!({
        "added_facts": ["用户喜欢咖啡"],
        "deprecated_facts": ["用户喜欢咖啡"],
        "project_id": null,
    });
    let url = format!("/api/v1/sessions/sess-self-c/memories/{}", hook_id);
    let resp = client
        .patch(server.url(&url))
        .json(&body)
        .send()
        .await
        .expect("更新失败");

    assert_eq!(resp.status(), 200);
    let result: Value = resp.json().await.expect("解析失败");
    assert!(result["conflicts"].as_u64().unwrap() >= 1, "应检测到自我矛盾");
    assert_eq!(result["has_critical"], true);
}

// ============================================================================
// v2.8 批次：Session 级索引隔离端到端测试
// ============================================================================

#[tokio::test]
async fn test_session_search_isolation_e2e() {
    // 核心端到端：不同 session 归档不同内容，/search 只返回本 session 结果
    use hippocampus_server::SessionSearchRouter;
    use std::sync::Arc;

    // 启动带 SessionSearchRouter 的服务（降级模式：仅关键词）
    let router = Arc::new(SessionSearchRouter::new(None, 0));
    let server = TestServer::start_with_session_search(Some(router), None).await;
    let client = reqwest::Client::new();

    // session-A 归档含 "Rust" 的内容
    let body_a = json!({
        "turns": [{
            "id": uuid::Uuid::new_v4().to_string(),
            "user_message": {"text": "讲讲 Rust 编程", "attachments": [], "tool_calls": [], "thinking": null},
            "llm_message": {"text": "Rust 是系统编程语言", "attachments": [], "tool_calls": [], "thinking": null},
            "tags": [{"kind": "Text"}],
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "token_count": 100
        }],
        "project_id": null
    });
    let resp = client
        .post(server.url("/api/v1/sessions/sess-a/archive"))
        .json(&body_a)
        .send()
        .await
        .expect("归档 A 失败");
    assert_eq!(resp.status(), 200);

    // session-B 归档含 "Python" 的内容
    let body_b = json!({
        "turns": [{
            "id": uuid::Uuid::new_v4().to_string(),
            "user_message": {"text": "讲讲 Python 编程", "attachments": [], "tool_calls": [], "thinking": null},
            "llm_message": {"text": "Python 是脚本语言", "attachments": [], "tool_calls": [], "thinking": null},
            "tags": [{"kind": "Text"}],
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "token_count": 100
        }],
        "project_id": null
    });
    let resp = client
        .post(server.url("/api/v1/sessions/sess-b/archive"))
        .json(&body_b)
        .send()
        .await
        .expect("归档 B 失败");
    assert_eq!(resp.status(), 200);

    // session-A 搜索 "Rust" → 应有结果
    let body = json!({ "query": "Rust", "top_k": 5 });
    let resp = client
        .post(server.url("/api/v1/sessions/sess-a/search"))
        .json(&body)
        .send()
        .await
        .expect("搜索 A 失败");
    assert_eq!(resp.status(), 200);
    let result: Value = resp.json().await.expect("解析失败");
    let results_a = result["results"].as_array().unwrap();
    assert!(!results_a.is_empty(), "sess-a 搜 Rust 应有结果");

    // session-A 搜索 "Python" → 应无结果（隔离）
    let body = json!({ "query": "Python", "top_k": 5 });
    let resp = client
        .post(server.url("/api/v1/sessions/sess-a/search"))
        .json(&body)
        .send()
        .await
        .expect("搜索 A Python 失败");
    assert_eq!(resp.status(), 200);
    let result: Value = resp.json().await.expect("解析失败");
    let results_a_py = result["results"].as_array().unwrap();
    assert!(
        results_a_py.is_empty(),
        "sess-a 搜 Python 应无结果（session 隔离）"
    );

    // session-B 搜索 "Python" → 应有结果
    let body = json!({ "query": "Python", "top_k": 5 });
    let resp = client
        .post(server.url("/api/v1/sessions/sess-b/search"))
        .json(&body)
        .send()
        .await
        .expect("搜索 B 失败");
    assert_eq!(resp.status(), 200);
    let result: Value = resp.json().await.expect("解析失败");
    let results_b = result["results"].as_array().unwrap();
    assert!(!results_b.is_empty(), "sess-b 搜 Python 应有结果");
}

#[tokio::test]
async fn test_session_search_no_router_returns_501() {
    // 未配置 session_search 也未配置 retriever → /search 返回 501
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let body = json!({ "query": "test", "top_k": 5 });
    let resp = client
        .post(server.url("/api/v1/sessions/sess-x/search"))
        .json(&body)
        .send()
        .await
        .expect("请求失败");

    assert_eq!(resp.status(), 501);
}

#[tokio::test]
async fn test_session_search_multiple_hooks_same_session() {
    // 同 session 归档多个 hook，搜索应返回多个结果
    use hippocampus_server::SessionSearchRouter;
    use std::sync::Arc;

    let router = Arc::new(SessionSearchRouter::new(None, 0));
    let server = TestServer::start_with_session_search(Some(router), None).await;
    let client = reqwest::Client::new();

    // 归档 3 个含 "文档" 的轮次
    for i in 0..3 {
        let body = json!({
            "turns": [{
                "id": uuid::Uuid::new_v4().to_string(),
                "user_message": {"text": format!("文档 {}", i), "attachments": [], "tool_calls": [], "thinking": null},
                "llm_message": {"text": format!("文档 {} 的内容", i), "attachments": [], "tool_calls": [], "thinking": null},
                "tags": [{"kind": "Text"}],
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "token_count": 50
            }],
            "project_id": null
        });
        let resp = client
            .post(server.url("/api/v1/sessions/sess-multi/archive"))
            .json(&body)
            .send()
            .await
            .expect("归档失败");
        assert_eq!(resp.status(), 200);
    }

    // 搜索 "文档" → 应找到 3 个
    let body = json!({ "query": "文档", "top_k": 10 });
    let resp = client
        .post(server.url("/api/v1/sessions/sess-multi/search"))
        .json(&body)
        .send()
        .await
        .expect("搜索失败");

    assert_eq!(resp.status(), 200);
    let result: Value = resp.json().await.expect("解析失败");
    let results = result["results"].as_array().unwrap();
    assert_eq!(results.len(), 3, "应找到 3 个文档");
}
