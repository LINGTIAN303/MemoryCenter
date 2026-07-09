//! # OpenCode 压缩事件监听 sidecar（v2.36 新增，v2.39 重构）
//!
//! 监听 OpenCode SQLite 会话库的压缩事件，自动触发 MemoryCenter 归档。
//!
//! ## 架构（v2.39 重构）
//!
//! ```text
//! ┌─────────────────┐      ┌──────────────────┐      ┌─────────────────┐
//! │   OpenCode      │      │  mc-sidecar      │      │  MemoryCenter   │
//! │                 │      │                  │      │                 │
//! │  session.db     │◄────│  SQLite 轮询     │      │  HTTP Server    │
//! │  (WAL mode)     │      │  (5s interval)   │      │                 │
//! │                 │      │                  │      │                 │
//! │  session_message│      │  检测 compaction │      │  /pre-compress  │
//! │  type=compaction│────►│  新消息 → 读增量 │────►│  归档 + 摘要     │
//! │                 │      │  → 序列化        │      │                 │
//! └─────────────────┘      └──────────────────┘      └─────────────────┘
//! ```
//!
//! ## 检测原理（v2.39 重构）
//!
//! **旧策略（v2.36，已废弃）**：监控 `session.time_compacting` 字段变化
//! **问题**：该字段在 OpenCode 源码（compaction.ts）中从未被写入
//!
//! **新策略（v2.39）**：轮询 `session_message` 表中 `type='compaction'` 的新消息
//! - 压缩完成后，OpenCode 往 session_message 表插入 compaction 消息
//! - sidecar 用 message_id 去重，发现新消息即触发归档
//! - 增量归档：只归档上次 compaction 到本次 compaction 之间的消息
//!
//! ## 使用方式
//!
//! ```bash
//! # 1. 启动 MemoryCenter HTTP 服务
//! mc-server
//!
//! # 2. 启动 sidecar（默认 5 秒轮询）
//! mc-sidecar --memorycenter-url http://127.0.0.1:8765
//!
//! # 3. backfill 模式（归档历史压缩会话）
//! mc-sidecar --backfill
//! ```

mod config;
mod opencode_db;
mod archive;
mod state;
mod watcher;

use clap::Parser;
use config::SidecarConfig;
use opencode_db::OpenCodeDb;
use archive::ArchiveClient;
use state::{resolve_state_path, SidecarState};
use watcher::{CompactionChangeEvent, CompactionWatcher};

#[tokio::main]
async fn main() {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mc_sidecar=info".into()),
        )
        .init();

    let config = SidecarConfig::parse();

    // 解析 OpenCode SQLite 路径
    let db_path = match config.resolve_db_path() {
        Ok(path) => path,
        Err(e) => {
            tracing::error!(error = %e, "无法解析 OpenCode SQLite 路径，请通过 --opencode-db 指定");
            std::process::exit(1);
        }
    };

    // 解析状态文件路径（v2.41 新增）
    let state_path = match resolve_state_path(config.state_file.as_ref()) {
        Ok(path) => path,
        Err(e) => {
            tracing::error!(error = %e, "无法解析状态文件路径，请通过 --state-file 指定");
            std::process::exit(1);
        }
    };

    tracing::info!(
        db_path = %db_path.display(),
        state_path = %state_path.display(),
        memorycenter_url = %config.memorycenter_url,
        poll_interval_secs = config.poll_interval,
        project_id = %config.project_id,
        backfill = config.backfill,
        "mc-sidecar 启动（v2.41 compaction 消息检测模式 + 状态持久化）"
    );

    // 检查 db 文件是否存在
    if !db_path.exists() {
        tracing::error!(db_path = %db_path.display(), "OpenCode SQLite 文件不存在");
        tracing::error!("请确认 OpenCode 已安装并至少运行过一次");
        std::process::exit(1);
    }

    // 加载持久化状态（v2.41 新增）
    let sidecar_state = match SidecarState::load(&state_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "状态文件加载失败，使用空状态继续");
            SidecarState::default()
        }
    };

    // 打开数据库
    let db = match OpenCodeDb::open(&db_path) {
        Ok(db) => db,
        Err(e) => {
            tracing::error!(error = %e, "打开 OpenCode SQLite 失败");
            std::process::exit(1);
        }
    };

    // 创建归档客户端
    let archive_client = ArchiveClient::new(&config);

    // 健康检查
    let healthy = archive_client.health_check().await.unwrap_or(false);
    if !healthy {
        tracing::warn!(
            url = %config.memorycenter_url,
            "MemoryCenter 服务不可达，sidecar 将继续运行并在检测到压缩时重试"
        );
    } else {
        tracing::info!(url = %config.memorycenter_url, "MemoryCenter 服务连接正常");
    }

    // 创建压缩事件检测器（v2.41：传入持久化状态）
    let mut watcher = CompactionWatcher::new(sidecar_state);

    // backfill 模式：归档历史压缩会话
    if config.backfill {
        tracing::info!("backfill 模式：扫描历史 compaction 事件...");
        match watcher.backfill_events(&db) {
            Ok(events) => {
                tracing::info!(count = events.len(), "发现未归档的历史 compaction 事件");
                for event in events {
                    archive_compaction_event(&db, &archive_client, &event, &config).await;
                    // 归档成功后标记为已处理（避免主循环 poll 重复归档）
                    watcher.mark_archived(&event);
                    // v2.41：每次归档后保存状态（防止 sidecar 异常退出丢失进度）
                    if let Err(e) = watcher.state().save(&state_path) {
                        tracing::warn!(error = %e, "状态保存失败（不影响本次归档）");
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "backfill 扫描失败");
            }
        }
        tracing::info!("backfill 完成，进入持续监听模式");
    } else {
        // 首次轮询：建立基线状态（不触发归档）
        // 将现有 compaction 消息标记为已处理，只归档后续新增的
        tracing::info!("首次轮询：建立基线状态...");
        match watcher.poll(&db) {
            Ok(result) => {
                // 把现有 compaction 全部标记为已处理（建立基线，不归档历史）
                for event in &result.new_compactions {
                    watcher.mark_archived(event);
                }
                // 保存基线状态
                if let Err(e) = watcher.state().save(&state_path) {
                    tracing::warn!(error = %e, "基线状态保存失败");
                }
                tracing::info!(
                    baseline_count = result.new_compactions.len(),
                    total_compactions = result.total_compactions,
                    "基线状态已建立（历史 compaction 不归档，只监听新增）"
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, "首次轮询失败");
            }
        }
    }

    // 主循环
    let poll_interval = std::time::Duration::from_secs(config.poll_interval);
    loop {
        tokio::time::sleep(poll_interval).await;

        // 轮询检测新的 compaction 事件
        let poll_result = match watcher.poll(&db) {
            Ok(result) => result,
            Err(e) => {
                tracing::warn!(error = %e, "轮询失败，等待下次重试");
                continue;
            }
        };

        if poll_result.total_compactions > 0 {
            tracing::debug!(
                total = poll_result.total_compactions,
                processed = poll_result.processed_count,
                new = poll_result.new_compactions.len(),
                "compaction 消息统计"
            );
        }

        // 处理新检测到的 compaction 事件
        let mut had_new = false;
        for event in poll_result.new_compactions {
            archive_compaction_event(&db, &archive_client, &event, &config).await;
            // 归档成功后标记为已处理
            watcher.mark_archived(&event);
            had_new = true;
        }

        // v2.41：有新归档时保存状态
        if had_new {
            if let Err(e) = watcher.state().save(&state_path) {
                tracing::warn!(error = %e, "状态保存失败（不影响本次归档）");
            }
        }
    }
}

/// 归档单个 compaction 事件（v2.39 新增）
///
/// 增量归档：读取 (from_seq, compaction.seq) 之间的消息，
/// 附加 compaction summary 作为标签，调用 MemoryCenter pre-compress 端点。
async fn archive_compaction_event(
    db: &OpenCodeDb,
    archive_client: &ArchiveClient,
    event: &CompactionChangeEvent,
    config: &SidecarConfig,
) {
    let compaction = &event.compaction;

    tracing::info!(
        session_id = %compaction.session_id,
        message_id = %compaction.message_id,
        seq = compaction.seq,
        reason = %compaction.reason,
        from_seq = ?event.from_seq,
        "开始增量归档 compaction 事件"
    );

    // 读取增量上下文：(from_seq, compaction.seq) 之间的消息
    let mut full_context = match db.read_session_context_between(
        &compaction.session_id,
        event.from_seq,
        compaction.seq,
        config.max_turns,
    ) {
        Ok(ctx) => ctx,
        Err(e) => {
            tracing::error!(
                session_id = %compaction.session_id,
                error = %e,
                "读取增量上下文失败"
            );
            return;
        }
    };

    // 若增量上下文为空（可能首次压缩前无消息），附加 compaction summary 保证非空
    if full_context.trim().is_empty() {
        tracing::warn!(
            session_id = %compaction.session_id,
            seq = compaction.seq,
            "增量上下文为空，使用 compaction summary 作为兜底"
        );
        full_context = format!(
            "System: 压缩摘要（reason={})\n\n{}\n\n{}",
            compaction.reason,
            compaction.summary,
            compaction.recent
        );
    } else {
        // 附加 compaction summary 作为高价值标签（决策点2）
        // 让 MemoryCenter 知道这批上下文对应的压缩摘要
        full_context.push_str(&format!(
            "\n\n--- Compaction Summary (reason={}) ---\n{}\n--- End Summary ---",
            compaction.reason, compaction.summary
        ));
    }

    // 估算 token 数（字符数 / 3，与 MemoryCenter 默认估算一致）
    let estimated_tokens = full_context.len() / 3;

    tracing::info!(
        session_id = %compaction.session_id,
        context_chars = full_context.len(),
        estimated_tokens,
        from_seq = ?event.from_seq,
        to_seq = compaction.seq,
        "读取增量上下文完成，调用 MemoryCenter pre-compress"
    );

    // 调用 MemoryCenter 归档
    match archive_client
        .pre_compress(&compaction.session_id, full_context, estimated_tokens, &config.project_id)
        .await
    {
        Ok(resp) => {
            tracing::info!(
                session_id = %compaction.session_id,
                compaction_seq = compaction.seq,
                hook_id = %resp.hook_id,
                parse_success = resp.parse_success,
                parsed_turns = resp.parsed_turns_count,
                archived_tokens = resp.archived_tokens,
                threshold = resp.threshold,
                ratio_percent = resp.threshold_ratio_percent,
                suggestion = %resp.suggestion,
                "归档成功"
            );
        }
        Err(e) => {
            tracing::error!(
                session_id = %compaction.session_id,
                compaction_seq = compaction.seq,
                error = %e,
                "归档失败"
            );
        }
    }
}
