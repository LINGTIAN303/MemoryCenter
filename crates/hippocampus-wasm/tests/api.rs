//! HippocampusCore 端到端 API 测试
//!
//! 验证 HippocampusCore JS API 的 4 个核心方法：
//! - archive: 归档轮次，返回 hook_id
//! - list_memories: 列出指定 session + period 的所有记忆
//! - read_memory: 按 memory_id 读取记忆文件
//! - read_index: 读取索引文档
//!
//! 通过 MemoryStorage 作为后端，端到端验证 Archiver + Storage 链路。

use hippocampus_core_logic::model::{IndexDocument, MemoryFile, MessageContent, MessageTurn, Tag};
use hippocampus_wasm::{HippocampusCore, MemoryStorage};
use uuid::Uuid;
use wasm_bindgen::JsValue;
use wasm_bindgen_test::*;

// Node.js 是 wasm-bindgen-test 的默认执行环境，无需 wasm_bindgen_test_configure!
// 通过 `wasm-pack test --node` 在 Node.js 中运行测试

/// 构造一个测试用 MessageTurn
fn make_turn(user_text: &str, llm_text: &str, token_count: usize) -> MessageTurn {
    MessageTurn {
        id: Uuid::new_v4(),
        user_message: MessageContent {
            text: Some(user_text.to_string()),
            attachments: vec![],
            tool_calls: vec![],
            thinking: None,
        },
        llm_message: MessageContent {
            text: Some(llm_text.to_string()),
            attachments: vec![],
            tool_calls: vec![],
            thinking: None,
        },
        tags: vec![Tag::Text],
        timestamp: chrono::Utc::now(),
        token_count,
    }
}

/// 将 Vec<MessageTurn> 序列化为 JsValue（供 HippocampusCore.archive 接收）
fn turns_to_js(turns: Vec<MessageTurn>) -> JsValue {
    serde_wasm_bindgen::to_value(&turns).expect("turns 序列化失败")
}

/// 测试 1：archive 一条消息后返回 hook_id，list_memories 验证有 1 条
#[wasm_bindgen_test]
async fn test_hippocampus_core_archive_and_retrieve() {
    let storage = MemoryStorage::new();
    let core = HippocampusCore::with_memory_storage(storage);

    let turns = vec![make_turn("用户问题 A", "LLM 回答 A", 100)];
    let turns_js = turns_to_js(turns);

    // archive 返回 hook_id（uuid 字符串，36 字符）
    let hook_id = core.archive("sess-api-001", turns_js).await;
    assert!(
        hook_id.is_ok(),
        "archive 应成功，错误: {:?}",
        hook_id.err()
    );
    let hook_id = hook_id.unwrap();
    assert_eq!(
        hook_id.len(),
        36,
        "hook_id 应为 36 字符的 uuid，实际: {}",
        hook_id
    );

    // list_memories 验证有 1 条
    let list_js = core.list_memories("sess-api-001", "daily").await;
    assert!(list_js.is_ok(), "list_memories 应成功");
    let list: Vec<String> =
        serde_wasm_bindgen::from_value(list_js.unwrap()).expect("反序列化 list 失败");
    assert_eq!(list.len(), 1, "归档后应有 1 条记忆，实际: {}", list.len());
}

/// 测试 2：归档多条记忆，list_memories 验证数量
#[wasm_bindgen_test]
async fn test_hippocampus_core_list_memories() {
    let storage = MemoryStorage::new();
    let core = HippocampusCore::with_memory_storage(storage);

    // 第一次归档（1 个 turn）
    let turns1 = vec![make_turn("问题1", "回答1", 50)];
    let hook1 = core
        .archive("sess-api-002", turns_to_js(turns1))
        .await
        .expect("第一次 archive 失败");
    assert_eq!(hook1.len(), 36);

    // 第二次归档（2 个 turn）
    let turns2 = vec![
        make_turn("问题2", "回答2", 60),
        make_turn("问题3", "回答3", 70),
    ];
    let hook2 = core
        .archive("sess-api-002", turns_to_js(turns2))
        .await
        .expect("第二次 archive 失败");
    assert_eq!(hook2.len(), 36);
    assert_ne!(hook1, hook2, "两次归档的 hook_id 应不同");

    // list_memories 应有 2 条
    let list_js = core
        .list_memories("sess-api-002", "daily")
        .await
        .expect("list_memories 失败");
    let list: Vec<String> =
        serde_wasm_bindgen::from_value(list_js).expect("反序列化 list 失败");
    assert_eq!(list.len(), 2, "应有 2 条记忆，实际: {}", list.len());
}

/// 测试 3：归档后用 list_memories 拿到 memory_id，再 read_memory 验证内容
#[wasm_bindgen_test]
async fn test_hippocampus_core_read_memory() {
    let storage = MemoryStorage::new();
    let core = HippocampusCore::with_memory_storage(storage);

    let turns = vec![make_turn("read_memory 测试用户", "read_memory 测试回复", 80)];
    let _hook_id = core
        .archive("sess-api-003", turns_to_js(turns))
        .await
        .expect("archive 失败");

    // 获取 memory_id
    let list_js = core
        .list_memories("sess-api-003", "daily")
        .await
        .expect("list_memories 失败");
    let list: Vec<String> =
        serde_wasm_bindgen::from_value(list_js).expect("反序列化 list 失败");
    assert_eq!(list.len(), 1);
    let memory_id = list[0].clone();

    // read_memory 验证内容
    let mem_js = core
        .read_memory(&memory_id)
        .await
        .expect("read_memory 失败");
    // 反序列化为 MemoryFile 强类型校验
    let mem_obj: MemoryFile =
        serde_wasm_bindgen::from_value(mem_js).expect("反序列化 memory 失败");

    assert_eq!(mem_obj.session_id, "sess-api-003", "session_id 应匹配");
    assert_eq!(mem_obj.turns.len(), 1, "应有 1 个 turn");
    assert_eq!(
        mem_obj.turns[0].user_message.text.as_deref(),
        Some("read_memory 测试用户"),
        "user_message.text 应匹配"
    );
    assert_eq!(
        mem_obj.turns[0].llm_message.text.as_deref(),
        Some("read_memory 测试回复"),
        "llm_message.text 应匹配"
    );
    assert_eq!(mem_obj.total_tokens, 80, "total_tokens 应为 80");
}

/// 测试 4：归档后 read_index 验证返回 Some
#[wasm_bindgen_test]
async fn test_hippocampus_core_read_index() {
    let storage = MemoryStorage::new();
    let core = HippocampusCore::with_memory_storage(storage);

    let turns = vec![make_turn("read_index 用户", "read_index 回复", 90)];
    let _hook_id = core
        .archive("sess-api-004", turns_to_js(turns))
        .await
        .expect("archive 失败");

    // read_index 应返回 Some(IndexDocument)
    let index_js = core
        .read_index("sess-api-004", "daily")
        .await
        .expect("read_index 失败");
    // 反序列化为 Option<IndexDocument> 强类型校验
    let index_val: Option<IndexDocument> =
        serde_wasm_bindgen::from_value(index_js).expect("反序列化 index 失败");

    // 应该是 Some
    let index_doc = index_val.expect("read_index 应返回 Some");
    assert_eq!(index_doc.session_id, "sess-api-004", "session_id 应匹配");
    assert_eq!(index_doc.hooks.len(), 1, "应有 1 个 hook");
    assert!(
        !index_doc.hooks[0].memory_id.is_empty(),
        "memory_id 应非空"
    );

    // 未归档的 session 应返回 None
    let empty_js = core
        .read_index("sess-not-exist", "daily")
        .await
        .expect("read_index 应不报错");
    let empty_val: Option<IndexDocument> =
        serde_wasm_bindgen::from_value(empty_js).expect("反序列化 empty index 失败");
    assert!(
        empty_val.is_none(),
        "未归档 session 的 read_index 应为 None"
    );
}

/// 测试 5：未归档的 session 调用 list_memories 应返回空数组（不报错）
#[wasm_bindgen_test]
async fn test_hippocampus_core_list_memories_empty() {
    let storage = MemoryStorage::new();
    let core = HippocampusCore::with_memory_storage(storage);

    let list_js = core
        .list_memories("sess-empty", "daily")
        .await
        .expect("list_memories 应不报错");
    let list: Vec<String> =
        serde_wasm_bindgen::from_value(list_js).expect("反序列化 list 失败");
    assert!(list.is_empty(), "未归档 session 应返回空数组");
}

/// 测试 6：非法 period 字符串应返回错误
#[wasm_bindgen_test]
async fn test_hippocampus_core_invalid_period() {
    let storage = MemoryStorage::new();
    let core = HippocampusCore::with_memory_storage(storage);

    let result = core.list_memories("sess-xxx", "yearly").await;
    assert!(
        result.is_err(),
        "非法 period 应返回错误，实际: {:?}",
        result
    );
}
