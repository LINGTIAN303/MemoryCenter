//! # 并发归档正确性测试
//!
//! 验证多会话并发归档时 DashMap 细粒度锁的正确性：
//! - 10 个会话同时归档，互不干扰
//! - 每个会话归档后能独立检索到自己的记忆
//! - 无数据混杂或丢失

use memory_center_core::{
    archive::Archiver,
    model::{ArchiveConfig, MessageContent, MessageTurn, Tag},
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

/// 多会话并发归档：10 个会话同时归档，验证互不干扰
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_concurrent_multi_session_archive() {
    let tmp = TempDir::new().unwrap();
    let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));
    let session_count = 10;
    let turns_per_session = 5;

    // 并发归档 10 个会话
    let mut handles = Vec::new();
    for sid in 0..session_count {
        let storage = storage.clone();
        let session_id = format!("sess-{}", sid);
        let turns: Vec<MessageTurn> = (0..turns_per_session)
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

    // 等待所有归档完成
    let mut session_ids = Vec::new();
    for handle in handles {
        session_ids.push(handle.await.unwrap());
    }

    // 验证每个会话独立可检索
    for sid in &session_ids {
        let retriever = Retriever::new(storage.clone(), sid, None);
        let summaries = retriever.get_summaries().await.unwrap();
        assert_eq!(
            summaries.len(),
            1,
            "会话 {} 应有 1 条摘要",
            sid
        );
        // 验证内容属于该会话
        let memory = retriever
            .retrieve_memory(&summaries[0].hook_id)
            .await
            .unwrap();
        assert_eq!(memory.session_id, *sid);
        assert_eq!(memory.turns.len(), turns_per_session);
    }

    // 验证会话间无混杂：检查每个会话的 turn 文本只含自己的标识
    for (idx, sid) in session_ids.iter().enumerate() {
        let retriever = Retriever::new(storage.clone(), sid, None);
        let summaries = retriever.get_summaries().await.unwrap();
        let memory = retriever
            .retrieve_memory(&summaries[0].hook_id)
            .await
            .unwrap();
        for turn in &memory.turns {
            let text = turn.user_message.text.as_ref().unwrap();
            assert!(
                text.contains(&format!("会话{}", idx)),
                "会话 {} 的文本不应混杂：{}",
                sid,
                text
            );
        }
    }
}

/// 多会话并发归档 + 并发读取：验证读写不互相阻塞
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_concurrent_archive_and_read() {
    let tmp = TempDir::new().unwrap();
    let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));

    // 先预置一个会话的记忆
    let config = ArchiveConfig::default();
    let mut archiver = Archiver::new(config, storage.clone(), "sess-pre", None);
    for i in 0..3 {
        archiver.push_turn(make_turn(&format!("预置消息{}", i), 100));
    }
    archiver.archive().await.unwrap();

    // 并发：一边归档新会话，一边读取已有会话
    let mut handles = Vec::new();

    // 写入任务：归档 5 个新会话
    for sid in 0..5 {
        let storage = storage.clone();
        let session_id = format!("sess-write-{}", sid);
        handles.push(tokio::spawn(async move {
            let config = ArchiveConfig::default();
            let mut archiver = Archiver::new(config, storage, &session_id, None);
            for i in 0..3 {
                archiver.push_turn(make_turn(&format!("新消息{}-{}", sid, i), 100));
            }
            archiver.archive().await.unwrap();
        }));
    }

    // 读取任务：并发读取预置会话
    for _ in 0..5 {
        let storage = storage.clone();
        handles.push(tokio::spawn(async move {
            let retriever = Retriever::new(storage, "sess-pre", None);
            let summaries = retriever.get_summaries().await.unwrap();
            assert_eq!(summaries.len(), 1);
            let memory = retriever
                .retrieve_memory(&summaries[0].hook_id)
                .await
                .unwrap();
            assert_eq!(memory.turns.len(), 3);
        }));
    }

    // 所有任务应无 deadlock 完成
    for handle in handles {
        handle.await.unwrap();
    }

    // 最终验证：预置会话 + 5 个新会话 = 6 个独立会话
    let retriever = Retriever::new(storage.clone(), "sess-pre", None);
    let summaries = retriever.get_summaries().await.unwrap();
    assert_eq!(summaries.len(), 1, "预置会话记忆应保持不变");
}
