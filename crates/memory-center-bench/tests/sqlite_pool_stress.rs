//! # SQLite 连接池压力测试
//!
//! 验证 r2d2 连接池在高并发下的稳定性：
//! - 50+ 并发读写 SQLite
//! - 连接池不耗尽（max_size=8）
//! - WAL 模式下读写不互相阻塞
//! - 无 deadlock 或连接超时

use memory_center_core::{
    archive::Archiver,
    model::{ArchiveConfig, MessageContent, MessageTurn, Tag},
    retrieve::Retriever,
    sqlite::SqliteStorage,
    storage::Storage,
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

/// SQLite 并发归档：20 个会话同时归档，验证连接池不耗尽
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn test_sqlite_concurrent_archive() {
    let tmp = TempDir::new().unwrap();
    let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::new(tmp.path(), None).unwrap());

    let session_count = 20;
    let mut handles = Vec::new();

    for sid in 0..session_count {
        let storage = storage.clone();
        let session_id = format!("sess-{}", sid);
        let turns: Vec<MessageTurn> = (0..5)
            .map(|i| make_turn(&format!("会话{}消息{}", sid, i), 100 + i))
            .collect();

        handles.push(tokio::spawn(async move {
            let config = ArchiveConfig::default();
            let mut archiver = Archiver::new(config, storage, &session_id, None);
            for turn in turns {
                archiver.push_turn(turn);
            }
            archiver.archive().await.unwrap();
            session_id
        }));
    }

    let mut session_ids = Vec::new();
    for handle in handles {
        session_ids.push(handle.await.unwrap());
    }

    // 验证每个会话独立可检索
    for sid in &session_ids {
        let retriever = Retriever::new(storage.clone(), sid, None);
        let summaries = retriever.get_summaries().await.unwrap();
        assert_eq!(summaries.len(), 1, "会话 {} 应有 1 条摘要", sid);
    }
}

/// SQLite 高并发读写混合：50 个任务交替读写，验证连接池稳定性
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn test_sqlite_mixed_read_write_50() {
    let tmp = TempDir::new().unwrap();
    let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::new(tmp.path(), None).unwrap());

    // 预置 5 个会话的记忆
    for sid in 0..5 {
        let config = ArchiveConfig::default();
        let mut archiver = Archiver::new(config, storage.clone(), &format!("sess-{}", sid), None);
        for i in 0..3 {
            archiver.push_turn(make_turn(&format!("消息{}", i), 100));
        }
        archiver.archive().await.unwrap();
    }

    // 50 个并发任务：25 写 + 25 读
    let mut handles = Vec::new();

    // 25 个写入任务（继续归档）
    for i in 0..25 {
        let storage = storage.clone();
        let sid = format!("sess-{}", i % 5);
        handles.push(tokio::spawn(async move {
            let config = ArchiveConfig::default();
            let mut archiver = Archiver::new(config, storage, &sid, None);
            archiver.push_turn(make_turn(&format!("新消息{}", i), 100));
            archiver.archive().await.unwrap();
        }));
    }

    // 25 个读取任务
    for i in 0..25 {
        let storage = storage.clone();
        let sid = format!("sess-{}", i % 5);
        handles.push(tokio::spawn(async move {
            let retriever = Retriever::new(storage, &sid, None);
            let summaries = retriever.get_summaries().await.unwrap();
            // 应至少有 1 条摘要（预置的）
            assert!(!summaries.is_empty(), "会话 {} 应有摘要", sid);
        }));
    }

    // 所有任务应无 deadlock 完成
    for handle in handles {
        handle.await.unwrap();
    }

    // 最终验证：每个会话至少有 1 条摘要
    for sid in 0..5 {
        let retriever = Retriever::new(storage.clone(), &format!("sess-{}", sid), None);
        let summaries = retriever.get_summaries().await.unwrap();
        assert!(
            summaries.len() >= 1,
            "会话 sess-{} 最终应至少有 1 条摘要，实际: {}",
            sid,
            summaries.len()
        );
    }
}

/// SQLite 连接池边界：并发数 > max_size(8)，验证排队不失败
#[tokio::test(flavor = "multi_thread", worker_threads = 16)]
async fn test_sqlite_pool_exceeds_max_size() {
    let tmp = TempDir::new().unwrap();
    let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::new(tmp.path(), None).unwrap());

    // 预置一个记忆
    let config = ArchiveConfig::default();
    let mut archiver = Archiver::new(config, storage.clone(), "sess-pool", None);
    for i in 0..5 {
        archiver.push_turn(make_turn(&format!("消息{}", i), 100));
    }
    let (_, hook) = archiver.archive().await.unwrap();
    let hook_id = hook.id.to_string();

    // 并发 16 个读取任务（> max_size=8）
    let mut handles = Vec::new();
    for _ in 0..16 {
        let storage = storage.clone();
        let hid = hook_id.clone();
        handles.push(tokio::spawn(async move {
            let retriever = Retriever::new(storage, "sess-pool", None);
            let memory = retriever.retrieve_memory(&hid).await.unwrap();
            assert_eq!(memory.turns.len(), 5);
        }));
    }

    // 所有任务应排队完成，无连接池耗尽错误
    for handle in handles {
        handle.await.expect("连接池排队读取应成功");
    }
}

/// SQLite 并发更新：同一记忆并发 PATCH，验证写串行化
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_sqlite_concurrent_update() {
    let tmp = TempDir::new().unwrap();
    let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::new(tmp.path(), None).unwrap());

    // 预置一个记忆
    let config = ArchiveConfig::default();
    let mut archiver = Archiver::new(config, storage.clone(), "sess-upd", None);
    for i in 0..3 {
        archiver.push_turn(make_turn(&format!("消息{}", i), 100));
    }
    let (_, hook) = archiver.archive().await.unwrap();
    let memory_id = hook.memory_id;

    // 并发更新 10 次
    let update_count = 10;
    let mut handles = Vec::new();
    for i in 0..update_count {
        let storage = storage.clone();
        let mid = memory_id.clone();
        handles.push(tokio::spawn(async move {
            let updates = memory_center_core::model::MemoryUpdate::new()
                .add_fact(format!("SQLite 并发事实 #{}", i));
            storage.update_memory(&mid, updates).await.unwrap();
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    // 验证：所有更新都应保留
    let memory = storage.read_memory(&memory_id).await.unwrap();
    assert_eq!(
        memory.updates.len(),
        update_count,
        "SQLite 应有 {} 条更新记录",
        update_count
    );
}

/// SQLite 高竞争并发更新：16 个并发 update（> max_size=8），验证 BEGIN IMMEDIATE 事务
///
/// 风险点验证：BEGIN IMMEDIATE 立即获取写锁，16 个并发会串行化执行。
/// 若每个事务耗时 <300ms，16 个串行总耗时 <5s（busy_timeout），不会超时。
/// 此测试验证：
/// 1. 高竞争下无 SQLITE_BUSY 超时
/// 2. 所有更新无丢失（事务串行化正确）
/// 3. 无死锁或连接池耗尽
#[tokio::test(flavor = "multi_thread", worker_threads = 16)]
async fn test_sqlite_concurrent_update_high_contention() {
    let tmp = TempDir::new().unwrap();
    let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::new(tmp.path(), None).unwrap());

    // 预置一个记忆
    let config = ArchiveConfig::default();
    let mut archiver = Archiver::new(config, storage.clone(), "sess-hc", None);
    for i in 0..3 {
        archiver.push_turn(make_turn(&format!("消息{}", i), 100));
    }
    let (_, hook) = archiver.archive().await.unwrap();
    let memory_id = hook.memory_id;

    // 16 个并发 update（超过连接池 max_size=8，触发排队 + 写锁串行化）
    let update_count = 16;
    let mut handles = Vec::new();
    for i in 0..update_count {
        let storage = storage.clone();
        let mid = memory_id.clone();
        handles.push(tokio::spawn(async move {
            let updates = memory_center_core::model::MemoryUpdate::new()
                .add_fact(format!("高竞争事实 #{}", i));
            storage.update_memory(&mid, updates).await.unwrap();
        }));
    }

    // 所有任务应在 busy_timeout(5s) 内完成
    for handle in handles {
        handle
            .await
            .expect("高竞争 update 不应超时或 panic");
    }

    // 验证：16 条更新全部保留（事务串行化无丢失）
    let memory = storage.read_memory(&memory_id).await.unwrap();
    assert_eq!(
        memory.updates.len(),
        update_count,
        "高竞争下应有 {} 条更新记录（实际 {}），BEGIN IMMEDIATE 事务串行化失效",
        update_count,
        memory.updates.len()
    );

    // 验证：原始上下文未被污染（updates 独立字段，不影响 turns）
    let original_text = memory.turns[0]
        .user_message
        .text
        .as_ref()
        .expect("原始消息应有文本");
    assert!(
        !original_text.contains("高竞争事实"),
        "原始上下文不应被 update 污染"
    );
}
