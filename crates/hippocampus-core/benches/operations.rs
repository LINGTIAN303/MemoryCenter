//! # 性能基准测试
//!
//! 测量 Hippocampus 核心操作的吞吐与延迟：
//!
//! - **归档**：不同 turn 数量下 archive() 的耗时
//! - **检索**：get_summaries / retrieve_memory 的耗时
//! - **周期任务**：weekly_merge / monthly_evict 的耗时
//!
//! 运行：
//!     cargo bench -p hippocampus-core
//!
//! 报告位置：
//!     target/criterion/report/index.html

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use hippocampus_core::{
    archive::Archiver,
    compact::Compactor,
    model::{ArchiveConfig, MessageContent, MessageTurn, Tag},
    retrieve::Retriever,
    score::DefaultScorer,
    storage::{LocalStorage, Storage},
};
use std::sync::Arc;
use tempfile::TempDir;
use uuid::Uuid;

/// 构造测试用 MessageTurn
fn make_turn(idx: usize, token_count: usize, with_tool: bool) -> MessageTurn {
    let mut tags = vec![Tag::Text];
    if with_tool {
        tags.push(Tag::ToolCall);
        tags.push(Tag::CodeBlock);
    }
    MessageTurn {
        id: Uuid::new_v4(),
        user_message: MessageContent {
            text: Some(format!("用户消息 {}: 测试内容用于基准测试", idx)),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            thinking: None,
        },
        llm_message: MessageContent {
            text: Some(format!("LLM 回复 {}: 这是 LLM 对第 {} 条消息的回复", idx, idx)),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            thinking: if with_tool {
                Some(format!("思考过程 {}: 分析用户意图并制定方案", idx))
            } else {
                None
            },
        },
        tags,
        timestamp: chrono::Utc::now(),
        token_count,
    }
}

/// 异步基准测试辅助：用 tokio runtime 执行 future
fn run_async<F, T>(future: F) -> T
where
    F: std::future::Future<Output = T>,
{
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(future)
}

/// 准备归档器并 push 指定数量的 turn
fn prepare_archiver(
    storage: Arc<dyn Storage>,
    session_id: &str,
    turn_count: usize,
    tokens_per_turn: usize,
) -> Archiver {
    let config = ArchiveConfig {
        token_threshold: turn_count * tokens_per_turn + 1, // 不触发自动归档
        force_truncate_limit: turn_count * tokens_per_turn * 2,
        wait_for_turn_completion: true,
    };
    let mut archiver = Archiver::new(config, storage, session_id, None);
    for i in 0..turn_count {
        archiver.push_turn(make_turn(i, tokens_per_turn, i % 5 == 0));
    }
    archiver
}

// ============================================================================
// 基准测试用例
// ============================================================================

fn bench_archive(c: &mut Criterion) {
    let mut group = c.benchmark_group("archive");

    for turn_count in [10, 50, 100, 500].iter() {
        group.bench_with_input(
            format!("archive_{}_turns", turn_count),
            turn_count,
            |b, &n| {
                b.iter(|| {
                    let tmp = TempDir::new().unwrap();
                    let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));
                    let mut archiver =
                        prepare_archiver(storage.clone(), "bench-archive", n, 100);
                    run_async(async move {
                        archiver.archive().await.unwrap();
                    });
                });
            },
        );
    }

    group.finish();
}

fn bench_retrieve(c: &mut Criterion) {
    // 预置：归档 50 个记忆文件（每个含 10 个 turn）
    let tmp = TempDir::new().unwrap();
    let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));
    let config = ArchiveConfig {
        token_threshold: 1000,
        force_truncate_limit: 2000,
        wait_for_turn_completion: true,
    };
    let mut archiver = Archiver::new(config, storage.clone(), "bench-retrieve", None);
    let mut hook_ids = Vec::new();
    for i in 0..50 {
        for j in 0..10 {
            archiver.push_turn(make_turn(i * 10 + j, 100, j == 0));
        }
        let (_, hook) = run_async(async { archiver.archive().await.unwrap() });
        hook_ids.push(hook.id.to_string());
    }

    let mut group = c.benchmark_group("retrieve");

    // get_summaries：读取所有周期的索引
    group.bench_function("get_summaries_50_files", |b| {
        b.iter(|| {
            let retriever = Retriever::new(storage.clone(), "bench-retrieve", None);
            run_async(async move {
                black_box(retriever.get_summaries().await.unwrap());
            });
        });
    });

    // render_to_system_prompt：渲染所有钩子为 prompt
    group.bench_function("render_prompt_50_files", |b| {
        b.iter(|| {
            let retriever = Retriever::new(storage.clone(), "bench-retrieve", None);
            run_async(async move {
                black_box(retriever.render_to_system_prompt().await.unwrap());
            });
        });
    });

    // retrieve_memory：按 hook_id 检索单个记忆文件
    let target_hook = hook_ids[25].clone();
    group.bench_function("retrieve_memory_single", |b| {
        b.iter(|| {
            let retriever = Retriever::new(storage.clone(), "bench-retrieve", None);
            let hook_id = target_hook.clone();
            run_async(async move {
                black_box(retriever.retrieve_memory(&hook_id).await.unwrap());
            });
        });
    });

    group.finish();
}

fn bench_compaction(c: &mut Criterion) {
    let mut group = c.benchmark_group("compaction");

    // 周级合并：预置 7 个 daily 文件，每个含 10 个 turn
    group.bench_function("weekly_merge_7_files", |b| {
        b.iter(|| {
            let tmp = TempDir::new().unwrap();
            let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));
            let config = ArchiveConfig {
                token_threshold: 1000,
                force_truncate_limit: 2000,
                wait_for_turn_completion: true,
            };

            // 预置 7 个 daily 文件
            let mut archiver = Archiver::new(config, storage.clone(), "bench-weekly", None);
            for i in 0..7 {
                for j in 0..10 {
                    archiver.push_turn(make_turn(i * 10 + j, 100, j == 0));
                }
                run_async(async {
                    archiver.archive().await.unwrap();
                });
            }

            // 执行 weekly_merge
            let compactor = Compactor::new(
                storage.clone(),
                Box::new(DefaultScorer::new()),
                "bench-weekly",
                None,
            );
            run_async(async move {
                black_box(compactor.weekly_merge().await.unwrap());
            });
        });
    });

    // 月级淘汰：预置 4 个 weekly 文件
    group.bench_function("monthly_evict_4_weekly", |b| {
        b.iter(|| {
            let tmp = TempDir::new().unwrap();
            let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));

            // 预置 4 个 weekly 文件（先归档 daily，再合并为 weekly）
            for w in 0..4 {
                let mut archiver = Archiver::new(
                    ArchiveConfig {
                        token_threshold: 1000,
                        force_truncate_limit: 2000,
                        wait_for_turn_completion: true,
                    },
                    storage.clone(),
                    "bench-monthly",
                    None,
                );
                for i in 0..7 {
                    for j in 0..10 {
                        archiver.push_turn(make_turn(w * 70 + i * 10 + j, 100, j == 0));
                    }
                    run_async(async {
                        archiver.archive().await.unwrap();
                    });
                }
                // 合并这 7 个 daily 为 1 个 weekly
                let compactor = Compactor::new(
                    storage.clone(),
                    Box::new(DefaultScorer::new()),
                    "bench-monthly",
                    None,
                );
                run_async(async {
                    compactor.weekly_merge().await.unwrap();
                });
            }

            // 执行 monthly_evict
            let compactor = Compactor::new(
                storage.clone(),
                Box::new(DefaultScorer::new()),
                "bench-monthly",
                None,
            );
            run_async(async move {
                black_box(compactor.monthly_evict().await.unwrap());
            });
        });
    });

    group.finish();
}

criterion_group!(benches, bench_archive, bench_retrieve, bench_compaction);
criterion_main!(benches);
