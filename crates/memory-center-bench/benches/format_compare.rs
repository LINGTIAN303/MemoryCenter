//! # 序列化格式对比基准
//!
//! 对比 JSON vs MessagePack 在 LocalStorage 下的性能：
//! - archive（归档）：序列化 + 写入
//! - retrieve（检索）：读取 + 反序列化
//!
//! 运行方式：`cargo bench -p MemoryCenter-bench --bench format_compare`

use criterion::{criterion_group, criterion_main, Criterion};
use memory_center_core::{
    archive::Archiver,
    model::{ArchiveConfig, MessageContent, MessageTurn, Tag},
    retrieve::Retriever,
    serialization::SerializationFormat,
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
        tags: vec![Tag::Text, Tag::CodeBlock],
        timestamp: chrono::Utc::now(),
        token_count,
        stop_reason: None,
        cost: None,
    }
}

/// 归档格式对比：JSON vs MessagePack
fn bench_archive_format(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let turns: Vec<MessageTurn> = (0..100)
        .map(|i| make_turn(&format!("消息 #{}", i), 100 + i))
        .collect();

    let mut group = c.benchmark_group("archive_format");

    group.bench_function("json", |b| {
        b.iter(|| {
            rt.block_on(async {
                let tmp = TempDir::new().unwrap();
                let storage: Arc<dyn Storage> = Arc::new(LocalStorage::with_format(
                    tmp.path(),
                    SerializationFormat::Json,
                ));
                let config = ArchiveConfig::default();
                let mut archiver = Archiver::new(config, storage, "bench-json", None);
                for turn in turns.clone() {
                    archiver.push_turn(turn);
                }
                archiver.archive().await.unwrap();
            });
        });
    });

    group.bench_function("msgpack", |b| {
        b.iter(|| {
            rt.block_on(async {
                let tmp = TempDir::new().unwrap();
                let storage: Arc<dyn Storage> = Arc::new(LocalStorage::with_format(
                    tmp.path(),
                    SerializationFormat::MessagePack,
                ));
                let config = ArchiveConfig::default();
                let mut archiver = Archiver::new(config, storage, "bench-msgpack", None);
                for turn in turns.clone() {
                    archiver.push_turn(turn);
                }
                archiver.archive().await.unwrap();
            });
        });
    });

    group.finish();
}

/// 检索格式对比：JSON vs MessagePack
fn bench_retrieve_format(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    // JSON 预置
    let json_tmp = TempDir::new().unwrap();
    let json_storage: Arc<dyn Storage> = Arc::new(LocalStorage::with_format(
        json_tmp.path(),
        SerializationFormat::Json,
    ));
    let json_hook = rt.block_on(async {
        let config = ArchiveConfig::default();
        let mut archiver = Archiver::new(config, json_storage.clone(), "bench-ret", None);
        for i in 0..50 {
            archiver.push_turn(make_turn(&format!("消息 #{}", i), 100 + i));
        }
        let (_, hook) = archiver.archive().await.unwrap();
        hook.id.to_string()
    });

    // MessagePack 预置
    let msgpack_tmp = TempDir::new().unwrap();
    let msgpack_storage: Arc<dyn Storage> = Arc::new(LocalStorage::with_format(
        msgpack_tmp.path(),
        SerializationFormat::MessagePack,
    ));
    let msgpack_hook = rt.block_on(async {
        let config = ArchiveConfig::default();
        let mut archiver = Archiver::new(config, msgpack_storage.clone(), "bench-ret", None);
        for i in 0..50 {
            archiver.push_turn(make_turn(&format!("消息 #{}", i), 100 + i));
        }
        let (_, hook) = archiver.archive().await.unwrap();
        hook.id.to_string()
    });

    let mut group = c.benchmark_group("retrieve_format");

    group.bench_function("json", |b| {
        b.iter(|| {
            rt.block_on(async {
                let retriever = Retriever::new(json_storage.clone(), "bench-ret", None);
                retriever.retrieve_memory(&json_hook).await.unwrap();
            });
        });
    });

    group.bench_function("msgpack", |b| {
        b.iter(|| {
            rt.block_on(async {
                let retriever = Retriever::new(msgpack_storage.clone(), "bench-ret", None);
                retriever.retrieve_memory(&msgpack_hook).await.unwrap();
            });
        });
    });

    group.finish();
}

criterion_group!(benches, bench_archive_format, bench_retrieve_format);
criterion_main!(benches);
