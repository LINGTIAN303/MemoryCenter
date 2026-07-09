//! MemoryStorage 基础 CRUD 测试
//!
//! 验证纯内存 Storage 实现的核心方法：
//! - write_memory / read_memory / delete_memory
//! - append_hook / read_index
//! - write_raw_context / read_raw_context / delete_raw_context

use memory_center_core_logic::model::*;
use memory_center_core_logic::storage::Storage;
use memory_center_wasm::MemoryStorage;
use chrono::Utc;
use uuid::Uuid;
use wasm_bindgen_test::*;

// Node.js 是 wasm-bindgen-test 的默认执行环境，无需 wasm_bindgen_test_configure!
// 通过 `wasm-pack test --node` 在 Node.js 中运行测试

/// 构造测试用 MemoryFile（含 1 个轮次）
fn make_test_memory_file() -> MemoryFile {
    let turn = MessageTurn {
        id: Uuid::new_v4(),
        user_message: MessageContent {
            text: Some("测试用户消息".to_string()),
            attachments: vec![],
            tool_calls: vec![],
            thinking: None,
            file_changes: Vec::new(),
        },
        llm_message: MessageContent {
            text: Some("测试 LLM 回复".to_string()),
            attachments: vec![],
            tool_calls: vec![],
            thinking: None,
            file_changes: Vec::new(),
        },
        tags: vec![Tag::Text],
        timestamp: Utc::now(),
        token_count: 10,
        stop_reason: None,
        cost: None,
    };
    MemoryFile::new(
        "test-session",
        Some("test-project".to_string()),
        vec![turn],
        ArchivePeriod::Daily,
    )
}

#[wasm_bindgen_test]
async fn test_memory_storage_write_read_memory() {
    let storage = MemoryStorage::new();
    let file = make_test_memory_file();
    let memory_id = storage.write_memory(&file).await.unwrap();

    // memory_id 格式应为 "memory-{uuid}"
    assert!(
        memory_id.starts_with("memory-"),
        "memory_id 应以 'memory-' 开头，实际: {}",
        memory_id
    );

    let read = storage.read_memory(&memory_id).await.unwrap();
    assert_eq!(read.id, file.id, "read.id 应等于 file.id");
    assert_eq!(read.session_id, file.session_id);
    assert_eq!(read.project_id, file.project_id);
}

#[wasm_bindgen_test]
async fn test_memory_storage_delete_memory() {
    let storage = MemoryStorage::new();
    let file = make_test_memory_file();
    let memory_id = storage.write_memory(&file).await.unwrap();

    // 删除前可读
    assert!(storage.read_memory(&memory_id).await.is_ok());

    storage.delete_memory(&memory_id).await.unwrap();

    // 删除后 read_memory 返回 Err
    let result = storage.read_memory(&memory_id).await;
    assert!(result.is_err(), "删除后 read_memory 应返回 Err");
}

#[wasm_bindgen_test]
async fn test_memory_storage_append_hook() {
    let storage = MemoryStorage::new();
    let file = make_test_memory_file();
    let memory_id = storage.write_memory(&file).await.unwrap();

    let hook = IndexHook::from_memory_file(&file, memory_id.clone());
    storage
        .append_hook(
            "test-session",
            Some("test-project"),
            ArchivePeriod::Daily,
            hook,
        )
        .await
        .unwrap();

    let index_doc = storage
        .read_index("test-session", Some("test-project"), ArchivePeriod::Daily)
        .await
        .unwrap();

    assert!(index_doc.is_some(), "索引文档应存在");
    assert_eq!(
        index_doc.unwrap().hooks.len(),
        1,
        "应含 1 个 hook"
    );
}

#[wasm_bindgen_test]
async fn test_memory_storage_raw_context() {
    let storage = MemoryStorage::new();
    let hook_id = Uuid::new_v4().to_string();
    let content = "测试原始上下文内容";

    // write_raw_context → read_raw_context
    let path = storage
        .write_raw_context("test-session", &hook_id, content)
        .await
        .unwrap();
    assert!(
        path.contains(&hook_id),
        "path 应包含 hook_id，实际: {}",
        path
    );

    let read_content = storage
        .read_raw_context("test-session", &hook_id)
        .await
        .unwrap();
    assert_eq!(read_content, content);

    // delete_raw_context 后 read_raw_context 应返回 Err
    storage
        .delete_raw_context("test-session", &hook_id)
        .await
        .unwrap();
    let result = storage.read_raw_context("test-session", &hook_id).await;
    assert!(result.is_err(), "删除后 read_raw_context 应返回 Err");
}
