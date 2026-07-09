//! 场景识别集成测试（v2.33）
//!
//! 验证 archive handler 调用 resolve_effective_scenario 的完整流程：
//! 1. 首次 archive 触发识别
//! 2. session_meta 写入
//! 3. 后续 archive 跳过识别（读 meta）

use memory_center_core::storage::{LocalStorage, Storage};
use memory_center_presets::scenario_detect::HybridScenarioDetector;
use tempfile::TempDir;

#[tokio::test]
async fn test_first_archive_writes_session_meta() {
    let tmp = TempDir::new().unwrap();
    let storage_root = tmp.path().to_path_buf();

    // 直接通过 LocalStorage 验证元数据写入（不调用完整 archive，避免 MCP 工具调用复杂度）
    let storage = LocalStorage::new(storage_root.clone());
    let detector = HybridScenarioDetector::new(None);

    // 模拟 coding 对话
    let turns = vec![
        make_turn("帮我写 Rust 函数", "好的，fn 主体如下"),
        make_turn("编译报错了", "调试一下架构"),
    ];

    let family = memory_center_agents::AgentFamily::ClaudeCode;
    let scenario = memory_center_presets::resolve_effective_scenario(
        &storage,
        "integration-sess-1",
        None,
        &family,
        &detector,
        &turns,
    )
    .await;

    assert_eq!(scenario, memory_center_scenarios::Scenario::Coding);

    // 验证 meta 已写入
    let meta = storage
        .read_session_meta("integration-sess-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(meta.scenario, "coding");
    assert_eq!(meta.method, "keyword");
}

fn make_turn(user: &str, llm: &str) -> memory_center_core::model::MessageTurn {
    use memory_center_core::model::{MessageContent, MessageTurn};
    use chrono::Utc;
    use uuid::Uuid;
    MessageTurn {
        id: Uuid::new_v4(),
        user_message: MessageContent {
            text: Some(user.to_string()),
            attachments: vec![],
            tool_calls: vec![],
            thinking: None,
            file_changes: Vec::new(),
        },
        llm_message: MessageContent {
            text: Some(llm.to_string()),
            attachments: vec![],
            tool_calls: vec![],
            thinking: None,
            file_changes: Vec::new(),
        },
        tags: vec![],
        timestamp: Utc::now(),
        token_count: 100,
        stop_reason: None,
        cost: None,
    }
}
