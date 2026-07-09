//! # 同记忆并发更新测试
//!
//! 验证同一 memory 文件被并发 PATCH 时写锁串行化的正确性：
//! - 所有更新操作串行执行，无丢失
//! - 最终状态包含所有更新的事实
//! - 无数据竞争或文件损坏

use memory_center_core::{
    archive::Archiver,
    model::{ArchiveConfig, MemoryUpdate, MessageContent, MessageTurn, Tag},
    storage::{LocalStorage, Storage},
};
use std::sync::Arc;
use tempfile::TempDir;
use uuid::Uuid;

/// 构造测试用 MessageTurn
fn make_turn(text: &str, token_count: usize) -> MessageTurn {
    MessageTurn {
        id: Uuid::new_v4(),
        user_message: MessageContent {
            text: Some(text.into()),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            thinking: None,
            file_changes: Vec::new(),
        },
        llm_message: MessageContent {
            text: Some("LLM 回复".into()),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            thinking: None,
            file_changes: Vec::new(),
        },
        tags: vec![Tag::Text],
        timestamp: chrono::Utc::now(),
        token_count,
        stop_reason: None,
        cost: None,
    }
}

/// 同记忆并发更新 10 次：验证所有更新都保留，无丢失
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_concurrent_update_same_memory() {
    let tmp = TempDir::new().unwrap();
    let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));

    // 预置一个记忆
    let config = ArchiveConfig::default();
    let mut archiver = Archiver::new(config, storage.clone(), "sess-cu", None);
    for i in 0..5 {
        archiver.push_turn(make_turn(&format!("消息{}", i), 100));
    }
    let (_, hook) = archiver.archive().await.unwrap();
    let memory_id = hook.memory_id;

    // 并发更新同一 memory 10 次
    let update_count = 10;
    let mut handles = Vec::new();
    for i in 0..update_count {
        let storage = storage.clone();
        let mid = memory_id.clone();
        handles.push(tokio::spawn(async move {
            let updates = MemoryUpdate::new()
                .add_fact(format!("并发事实 #{}", i))
                .revise_fact(format!("修正 #{}", i));
            storage.update_memory(&mid, updates).await.unwrap();
        }));
    }

    // 等待所有更新完成
    for handle in handles {
        handle.await.unwrap();
    }

    // 验证：所有更新都应保留（updates 字段长度 = 10）
    let memory = storage.read_memory(&memory_id).await.unwrap();
    assert_eq!(
        memory.updates.len(),
        update_count,
        "应有 {} 条更新记录（写锁串行化保证无丢失）",
        update_count
    );

    // 验证每条更新都含 added + revised
    for (idx, record) in memory.updates.iter().enumerate() {
        assert_eq!(
            record.update.added_facts.len(),
            1,
            "第 {} 条更新应有 1 个 added_fact",
            idx
        );
        assert_eq!(
            record.update.revised_facts.len(),
            1,
            "第 {} 条更新应有 1 个 revised_fact",
            idx
        );
        assert!(
            record.update.added_facts[0].starts_with("并发事实 #"),
            "第 {} 条 added_fact 内容异常: {}",
            idx,
            record.update.added_facts[0]
        );
    }

    // 验证原始 turns 未被污染
    assert_eq!(memory.turns.len(), 5);
    for turn in &memory.turns {
        let text = turn.user_message.text.as_ref().unwrap();
        assert!(
            !text.contains("并发事实"),
            "原始 turns 不应被 update 污染: {}",
            text
        );
    }
}

/// 同记忆并发更新：验证无文件损坏（序列化一致性）
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_concurrent_update_no_corruption() {
    let tmp = TempDir::new().unwrap();
    let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));

    // 预置一个较大的记忆（50 turns）
    let config = ArchiveConfig::default();
    let mut archiver = Archiver::new(config, storage.clone(), "sess-nc", None);
    for i in 0..50 {
        archiver.push_turn(make_turn(&format!("消息{:03}", i), 100 + i));
    }
    let (_, hook) = archiver.archive().await.unwrap();
    let memory_id = hook.memory_id;

    // 并发更新 20 次（每次带较大 payload）
    let mut handles = Vec::new();
    for i in 0..20 {
        let storage = storage.clone();
        let mid = memory_id.clone();
        handles.push(tokio::spawn(async move {
            let updates = MemoryUpdate::new()
                .add_fact(format!("新增事实 #{}：这是一段较长的内容用于测试序列化稳定性", i))
                .revise_fact(format!("修正事实 #{}：修正原有过时的信息", i))
                .deprecate_fact(format!("废弃事实 #{}", i));
            storage.update_memory(&mid, updates).await.unwrap();
        }));
    }

    // 等待所有更新
    for handle in handles {
        handle.await.unwrap();
    }

    // 验证：文件可正常反序列化（无损坏）
    let memory = storage.read_memory(&memory_id).await.unwrap();
    assert_eq!(memory.turns.len(), 50, "原始 turns 应保持 50 条");
    assert_eq!(memory.updates.len(), 20, "应有 20 条更新记录");

    // 验证每条更新的三类事实都存在
    for record in &memory.updates {
        assert_eq!(record.update.added_facts.len(), 1);
        assert_eq!(record.update.revised_facts.len(), 1);
        assert_eq!(record.update.deprecated_facts.len(), 1);
    }

    // 验证 total_tokens 未被破坏
    let expected_tokens: usize = (0..50).map(|i| 100 + i).sum();
    assert_eq!(memory.total_tokens, expected_tokens, "total_tokens 应未被修改");
}
