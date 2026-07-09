//! # 同会话读写并发测试
//!
//! 验证同一会话并发 update + read 时 RwLock 读写隔离的正确性：
//! - 读操作可并发（多个 reader 同时读）
//! - 写操作串行化（同一 session 的写锁互斥）
//! - 读操作不会被写操作阻塞（RwLock 读优先）

use memory_center_core::{
    archive::Archiver,
    model::{ArchiveConfig, MemoryUpdate, MessageContent, MessageTurn, Tag},
    retrieve::Retriever,
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

/// 同会话并发读取：多个 reader 同时读取同一记忆
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_concurrent_read_same_session() {
    let tmp = TempDir::new().unwrap();
    let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));

    // 预置一个记忆
    let config = ArchiveConfig::default();
    let mut archiver = Archiver::new(config, storage.clone(), "sess-rw", None);
    for i in 0..10 {
        archiver.push_turn(make_turn(&format!("消息{}", i), 100));
    }
    let (_, hook) = archiver.archive().await.unwrap();
    let hook_id = hook.id.to_string();

    // 并发 20 个读取任务
    let mut handles = Vec::new();
    for _ in 0..20 {
        let storage = storage.clone();
        let hook_id = hook_id.clone();
        handles.push(tokio::spawn(async move {
            let retriever = Retriever::new(storage, "sess-rw", None);
            let memory = retriever.retrieve_memory(&hook_id).await.unwrap();
            assert_eq!(memory.turns.len(), 10);
            memory.turns.len()
        }));
    }

    // 所有读取应成功
    for handle in handles {
        let count = handle.await.unwrap();
        assert_eq!(count, 10);
    }
}

/// 同会话并发更新 + 读取：验证写锁串行化，读不被阻塞
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_concurrent_update_and_read_same_session() {
    let tmp = TempDir::new().unwrap();
    let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));

    // 预置一个记忆
    let config = ArchiveConfig::default();
    let mut archiver = Archiver::new(config, storage.clone(), "sess-ur", None);
    for i in 0..5 {
        archiver.push_turn(make_turn(&format!("消息{}", i), 100));
    }
    let (_, hook) = archiver.archive().await.unwrap();
    let memory_id = hook.memory_id;

    // 并发：5 个 update + 5 个 read
    let mut handles = Vec::new();

    // 5 个更新任务
    for i in 0..5 {
        let storage = storage.clone();
        let mid = memory_id.clone();
        handles.push(tokio::spawn(async move {
            let updates = MemoryUpdate::new().add_fact(format!("并发更新事实 #{}", i));
            storage.update_memory(&mid, updates).await.unwrap();
        }));
    }

    // 5 个读取任务
    for _ in 0..5 {
        let storage = storage.clone();
        let mid = memory_id.clone();
        handles.push(tokio::spawn(async move {
            let memory = storage.read_memory(&mid).await.unwrap();
            // 读取应始终成功，updates 数量在 0-5 之间
            assert!(memory.updates.len() <= 5);
        }));
    }

    // 所有任务应无 deadlock 完成
    for handle in handles {
        handle.await.unwrap();
    }

    // 最终验证：5 次更新全部生效
    let memory = storage.read_memory(&memory_id).await.unwrap();
    assert_eq!(
        memory.updates.len(),
        5,
        "应有 5 条更新记录（写锁串行化保证）"
    );
}

/// 同会话并发归档 + 读取摘要：验证读写隔离
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_concurrent_archive_and_summaries() {
    let tmp = TempDir::new().unwrap();
    let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));

    // 预置 3 个记忆
    for _ in 0..3 {
        let config = ArchiveConfig::default();
        let mut archiver = Archiver::new(config, storage.clone(), "sess-as", None);
        for i in 0..2 {
            archiver.push_turn(make_turn(&format!("消息{}", i), 100));
        }
        archiver.archive().await.unwrap();
    }

    // 并发：继续归档 + 读取摘要
    let mut handles = Vec::new();

    // 3 个归档任务
    for i in 0..3 {
        let storage = storage.clone();
        handles.push(tokio::spawn(async move {
            let config = ArchiveConfig::default();
            let mut archiver = Archiver::new(config, storage, "sess-as", None);
            for j in 0..2 {
                archiver.push_turn(make_turn(&format!("新消息{}-{}", i, j), 100));
            }
            archiver.archive().await.unwrap();
        }));
    }

    // 3 个摘要读取任务
    for _ in 0..3 {
        let storage = storage.clone();
        handles.push(tokio::spawn(async move {
            let retriever = Retriever::new(storage, "sess-as", None);
            let summaries = retriever.get_summaries().await.unwrap();
            // 摘要数应在 3-6 之间（取决于归档进度）
            assert!(
                summaries.len() >= 3 && summaries.len() <= 6,
                "摘要数应在新归档过程中递增，当前: {}",
                summaries.len()
            );
        }));
    }

    // 无 deadlock 完成
    for handle in handles {
        handle.await.unwrap();
    }

    // 最终验证：3 预置 + 3 新增 = 6 个钩子
    let retriever = Retriever::new(storage.clone(), "sess-as", None);
    let summaries = retriever.get_summaries().await.unwrap();
    assert_eq!(summaries.len(), 6, "最终应有 6 条摘要");
}
