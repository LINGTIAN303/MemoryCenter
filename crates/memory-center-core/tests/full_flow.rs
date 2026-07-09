//! # P2.5 跨模块集成测试
//!
//! 模拟 Agent 完整会话场景，验证「归档 → 索引 → 检索」全链路。
//!
//! ## 测试场景
//!
//! 1. 模拟多轮对话（含不同类型标签）
//! 2. 达到阈值触发归档
//! 3. 用 Retriever 渲染下一轮 system prompt（模拟 LLM 起点）
//! 4. 通过 hook_id 检索详细记忆（模拟 tool 调用）
//! 5. 验证多次归档后索引与存储的一致性
//! 6. 验证 project_id 隔离

use memory_center_core::archive::Archiver;
use memory_center_core::model::{ArchiveConfig, MessageContent, MessageTurn, Tag, ToolInvocation};
use memory_center_core::retrieve::Retriever;
use memory_center_core::storage::{LocalStorage, Storage};
use chrono::Utc;
use std::sync::Arc;
use tempfile::TempDir;
use uuid::Uuid;

/// 构造带可配置标签的 MessageTurn
fn make_turn_with_tags(
    user_text: &str,
    llm_text: &str,
    token_count: usize,
    tags: Vec<Tag>,
) -> MessageTurn {
    MessageTurn {
        id: Uuid::new_v4(),
        user_message: MessageContent {
            text: Some(user_text.into()),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            thinking: None,
            file_changes: Vec::new(),
        },
        llm_message: MessageContent {
            text: Some(llm_text.into()),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            thinking: None,
            file_changes: Vec::new(),
        },
        tags,
        timestamp: Utc::now(),
        token_count,
        stop_reason: None,
        cost: None,
    }
}

/// 默认构造的 turn（带 Text + CodeBlock 标签）
fn make_turn(user_text: &str, token_count: usize) -> MessageTurn {
    make_turn_with_tags(
        user_text,
        "LLM 回复内容",
        token_count,
        vec![Tag::Text, Tag::CodeBlock],
    )
}

/// 带工具调用的 turn
fn make_turn_with_tool_call(user_text: &str, token_count: usize) -> MessageTurn {
    let mut turn = make_turn_with_tags(
        user_text,
        "LLM 调用了工具",
        token_count,
        vec![Tag::Text, Tag::ToolCall, Tag::AgentTool],
    );
    turn.llm_message.tool_calls = vec![ToolInvocation {
        name: "search_web".into(),
        arguments: r#"{"query":"Rust memory库"}"#.into(),
        result: r#"{"results":[]}"#.into(),
        duration_ms: Some(120),
        status: None,
        error: None,
        call_id: None,
    }];
    turn
}

/// 带思考过程的 turn
fn make_turn_with_thinking(user_text: &str, token_count: usize) -> MessageTurn {
    let mut turn = make_turn_with_tags(
        user_text,
        "LLM 经过思考后回复",
        token_count,
        vec![Tag::Text, Tag::Thinking],
    );
    turn.llm_message.thinking = Some("用户问的是记忆库设计，需要从架构层面分析...".into());
    turn
}

/// 默认归档配置
fn default_config() -> ArchiveConfig {
    ArchiveConfig {
        token_threshold: 100,
        force_truncate_limit: 150,
        wait_for_turn_completion: true,
    }
}

// ============================================================================
// 场景 1：基础全链路（单次归档 → 渲染 → 检索）
// ============================================================================

#[tokio::test]
async fn test_full_flow_single_archive() {
    let tmp = TempDir::new().unwrap();
    let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));

    // 阶段 1：模拟 Agent 多轮对话并归档
    let mut archiver = Archiver::new(
        default_config(),
        storage.clone(),
        "session-full-1",
        None,
    );

    // 推入 2 轮对话达到阈值
    archiver.push_turn(make_turn("用户问：如何设计记忆库？", 60));
    archiver.push_turn(make_turn("用户追问：索引怎么建？", 50));

    assert!(archiver.should_archive());
    let (memory, hook) = archiver.archive().await.unwrap();

    // 验证归档后状态清零（模拟 LLM 上下文已丢弃）
    assert_eq!(archiver.current_tokens(), 0);
    assert_eq!(archiver.pending_turns_count(), 0);

    // 阶段 2：用 Retriever 渲染下一轮 system prompt
    let retriever = Retriever::new(storage.clone(), "session-full-1", None);
    let prompt = retriever.render_to_system_prompt().await.unwrap();

    // 验证 system prompt 包含摘要信息
    assert!(prompt.contains("# 可用记忆索引"));
    assert!(prompt.contains("## 近期记忆（daily）"));
    assert!(prompt.contains("如何设计记忆库？"));
    assert!(prompt.contains("文本消息")); // Tag Display 中文输出
    assert!(prompt.contains(&hook.id.to_string())); // 钩子 ID

    // 阶段 3：模拟 LLM tool 调用，按 hook_id 检索详细记忆
    let retrieved = retriever
        .retrieve_memory(&hook.id.to_string())
        .await
        .unwrap();

    assert_eq!(retrieved.id, memory.id);
    assert_eq!(retrieved.turns.len(), 2);
    assert_eq!(retrieved.total_tokens, 110);
    assert!(!retrieved.truncated);
}

// ============================================================================
// 场景 2：多次归档 + 多钩子检索
// ============================================================================

#[tokio::test]
async fn test_full_flow_multiple_archives_and_retrieval() {
    let tmp = TempDir::new().unwrap();
    let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));

    let mut archiver = Archiver::new(
        default_config(),
        storage.clone(),
        "session-full-2",
        None,
    );

    // 归档 3 次（模拟 3 个完整的上下文窗口周期）
    let mut hooks = Vec::new();
    let mut memories = Vec::new();
    let topics = ["Rust 基础", "Tokio 异步运行时", "Serde 序列化"];

    for (i, topic) in topics.iter().enumerate() {
        archiver.push_turn(make_turn(
            &format!("第 {} 次对话：{}", i + 1, topic),
            60,
        ));
        archiver.push_turn(make_turn(&format!("{} 续接", topic), 50));

        let (m, h) = archiver.archive().await.unwrap();
        memories.push(m);
        hooks.push(h);
    }

    // 验证 Storage 中有 3 个记忆文件
    let files = storage
        .list_memories("session-full-2", None, memory_center_core::model::ArchivePeriod::Daily)
        .await
        .unwrap();
    assert_eq!(files.len(), 3);

    // 验证索引文档有 3 个钩子
    let retriever = Retriever::new(storage.clone(), "session-full-2", None);
    let summaries = retriever.get_summaries().await.unwrap();
    assert_eq!(summaries.len(), 3);

    // 验证摘要按时间排序（旧 → 新）
    for i in 0..2 {
        assert!(summaries[i].archived_at <= summaries[i + 1].archived_at);
    }

    // 验证每个钩子都能正确检索
    for (i, hook) in hooks.iter().enumerate() {
        let retrieved = retriever
            .retrieve_memory(&hook.id.to_string())
            .await
            .unwrap();
        assert_eq!(retrieved.id, memories[i].id);
        assert!(retrieved.turns[0]
            .user_message
            .text
            .as_ref()
            .unwrap()
            .contains(topics[i]));
    }

    // 验证 system prompt 包含全部 3 个钩子
    let prompt = retriever.render_to_system_prompt().await.unwrap();
    for topic in topics.iter() {
        assert!(prompt.contains(*topic));
    }
}

// ============================================================================
// 场景 3：混合标签类型（文本/工具调用/思考过程）
// ============================================================================

#[tokio::test]
async fn test_full_flow_mixed_tag_types() {
    let tmp = TempDir::new().unwrap();
    let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));

    let mut archiver = Archiver::new(
        default_config(),
        storage.clone(),
        "session-mixed",
        None,
    );

    // 推入不同类型的 turn
    archiver.push_turn(make_turn("普通文本对话", 30));
    archiver.push_turn(make_turn_with_tool_call("需要调用工具", 40));
    archiver.push_turn(make_turn_with_thinking("需要深度思考", 40));

    let (memory, hook) = archiver.archive().await.unwrap();

    // 验证 MemoryFile 自动合并了所有标签（并集去重）
    assert!(memory.tags.contains(&Tag::Text));
    assert!(memory.tags.contains(&Tag::CodeBlock));
    assert!(memory.tags.contains(&Tag::ToolCall));
    assert!(memory.tags.contains(&Tag::AgentTool));
    assert!(memory.tags.contains(&Tag::Thinking));

    // 验证钩子标签与 MemoryFile 一致
    let retriever = Retriever::new(storage.clone(), "session-mixed", None);
    let summaries = retriever.get_summaries().await.unwrap();
    assert_eq!(summaries.len(), 1);

    let s = &summaries[0];
    assert!(s.tags.contains(&"文本消息".to_string()));
    assert!(s.tags.contains(&"代码块".to_string()));
    assert!(s.tags.contains(&"工具调用".to_string()));
    assert!(s.tags.contains(&"思考过程".to_string()));

    // 验证检索到的记忆文件保留完整结构（含 tool_calls 和 thinking）
    let retrieved = retriever
        .retrieve_memory(&hook.id.to_string())
        .await
        .unwrap();
    assert_eq!(retrieved.turns.len(), 3);
    assert!(!retrieved.turns[1].llm_message.tool_calls.is_empty());
    assert!(retrieved.turns[2].llm_message.thinking.is_some());
}

// ============================================================================
// 场景 4：强制截断（超过硬上限）
// ============================================================================

#[tokio::test]
async fn test_full_flow_force_truncate() {
    let tmp = TempDir::new().unwrap();
    let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));

    let mut archiver = Archiver::new(
        default_config(),
        storage.clone(),
        "session-trunc",
        None,
    );

    // 推入一个超大 turn，超过硬上限
    archiver.push_turn(make_turn("超长上下文", 160));
    assert!(archiver.should_force_truncate());

    let (memory, _) = archiver.archive().await.unwrap();
    assert!(memory.truncated); // 应标记截断

    // 验证摘要视图也反映截断状态（通过 token_count > force_truncate_limit）
    let retriever = Retriever::new(storage.clone(), "session-trunc", None);
    let summaries = retriever.get_summaries().await.unwrap();
    assert_eq!(summaries[0].token_count, 160);
}

// ============================================================================
// 场景 5：project_id 隔离
// ============================================================================

#[tokio::test]
async fn test_full_flow_project_isolation() {
    let tmp = TempDir::new().unwrap();
    let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));

    // 项目 A 的会话
    let mut archiver_a = Archiver::new(
        default_config(),
        storage.clone(),
        "session-a",
        Some("project-A".into()),
    );
    archiver_a.push_turn(make_turn("项目 A 的对话", 110));
    let (memory_a, hook_a) = archiver_a.archive().await.unwrap();

    // 项目 B 的会话
    let mut archiver_b = Archiver::new(
        default_config(),
        storage.clone(),
        "session-b",
        Some("project-B".into()),
    );
    archiver_b.push_turn(make_turn("项目 B 的对话", 110));
    let (memory_b, hook_b) = archiver_b.archive().await.unwrap();

    // v2.4: 记忆文件始终存到 sessions/{session_id}/（session 隔离）
    assert!(hook_a.memory_id.starts_with("sessions/session-a/daily/"));
    assert!(hook_b.memory_id.starts_with("sessions/session-b/daily/"));
    assert_ne!(memory_a.id, memory_b.id);

    // 验证用项目 A 的 Retriever 只能看到项目 A 的记忆
    let retriever_a = Retriever::new(
        storage.clone(),
        "session-a",
        Some("project-A".into()),
    );
    let summaries_a = retriever_a.get_summaries().await.unwrap();
    assert_eq!(summaries_a.len(), 1);
    assert!(summaries_a[0].summary_title.contains("项目 A"));

    // 项目 A 的 Retriever 不能检索项目 B 的钩子
    let result = retriever_a
        .retrieve_memory(&hook_b.id.to_string())
        .await;
    assert!(result.is_err());

    // 项目 B 的 Retriever 同理
    let retriever_b = Retriever::new(
        storage.clone(),
        "session-b",
        Some("project-B".into()),
    );
    let summaries_b = retriever_b.get_summaries().await.unwrap();
    assert_eq!(summaries_b.len(), 1);
    assert!(summaries_b[0].summary_title.contains("项目 B"));
}

// ============================================================================
// 场景 6：完整 Agent 工作流模拟
// ============================================================================

#[tokio::test]
async fn test_full_flow_agent_workflow_simulation() {
    // 模拟一个真实的 Agent 工作流：
    // 1. 第一轮对话窗口 → 归档 → 下一轮以 system prompt + 摘要为起点
    // 2. 第二轮对话窗口 → 归档 → 下一轮继续
    // 3. 验证所有历史记忆可被检索

    let tmp = TempDir::new().unwrap();
    let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));
    let session_id = "agent-workflow";

    // 模拟第一轮对话窗口
    let mut archiver = Archiver::new(
        default_config(),
        storage.clone(),
        session_id,
        None,
    );

    archiver.push_turn(make_turn_with_thinking(
        "用户：帮我设计一个 Rust 记忆库",
        60,
    ));
    archiver.push_turn(make_turn_with_tool_call(
        "用户：查一下现有的方案",
        50,
    ));
    let (memory1, hook1) = archiver.archive().await.unwrap();

    // 模拟第二轮对话窗口：以 system prompt + 摘要为新起点
    let retriever = Retriever::new(storage.clone(), session_id, None);
    let system_prompt = retriever.render_to_system_prompt().await.unwrap();
    assert!(system_prompt.contains("帮我设计一个 Rust 记忆库"));
    assert!(system_prompt.contains(&hook1.id.to_string()));

    // 推入第二轮对话（archiver 已重置，可继续使用）
    archiver.push_turn(make_turn(
        "用户：基于上次的设计，开始实现",
        60,
    ));
    archiver.push_turn(make_turn(
        "用户：先实现 Storage trait",
        50,
    ));
    let (memory2, hook2) = archiver.archive().await.unwrap();

    // 验证两轮记忆都存在且独立
    assert_ne!(memory1.id, memory2.id);
    assert_ne!(hook1.id, hook2.id);

    // 验证 system prompt 现在包含两个钩子
    let prompt = retriever.render_to_system_prompt().await.unwrap();
    assert!(prompt.contains(&hook1.id.to_string()));
    assert!(prompt.contains(&hook2.id.to_string()));

    // 验证可以通过两个钩子分别检索完整记忆
    let m1 = retriever.retrieve_memory(&hook1.id.to_string()).await.unwrap();
    let m2 = retriever.retrieve_memory(&hook2.id.to_string()).await.unwrap();

    assert_eq!(m1.id, memory1.id);
    assert_eq!(m2.id, memory2.id);
    assert!(m1.turns[0].user_message.text.as_ref().unwrap().contains("设计一个 Rust 记忆库"));
    assert!(m2.turns[0].user_message.text.as_ref().unwrap().contains("基于上次的设计"));

    // 验证 Storage 中有 2 个记忆文件
    let files = storage
        .list_memories(session_id, None, memory_center_core::model::ArchivePeriod::Daily)
        .await
        .unwrap();
    assert_eq!(files.len(), 2);
}
