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
mod opencode_writer;
mod archive;
mod state;
mod watcher;

use clap::Parser;
use config::SidecarConfig;
use opencode_db::OpenCodeDb;
use archive::{ArchiveClient, SidecarContent, SidecarTurn};
use state::{resolve_state_path, SidecarState};
use watcher::{CompactionChangeEvent, CompactionWatcher, TokenThresholdEvent};
// v2.46：AgentAdapter trait 用于动态分发
use memory_center_adapter::AgentAdapter;

#[tokio::main]
async fn main() {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mc_sidecar=info".into()),
        )
        .init();

    let mut config = SidecarConfig::parse();

    // 解析状态文件路径（v2.41 新增）
    let state_path = match resolve_state_path(config.state_file.as_ref()) {
        Ok(path) => path,
        Err(e) => {
            tracing::error!(error = %e, "无法解析状态文件路径，请通过 --state-file 指定");
            std::process::exit(1);
        }
    };

    // v2.47 修复：提前解析 DB 路径并回填到 config.opencode_db
    // 主循环的 compaction 消息对插入依赖 config.opencode_db，
    // 但用户通常不传 --opencode-db（走默认路径），导致 None 而跳过插入
    if config.agent == "opencode" && config.opencode_db.is_none() {
        match config.resolve_db_path() {
            Ok(path) => {
                config.opencode_db = Some(path);
            }
            Err(e) => {
                tracing::error!(error = %e, "无法解析 OpenCode SQLite 路径，请通过 --opencode-db 指定");
                std::process::exit(1);
            }
        }
    }

    // v2.46：按 --agent 选择 adapter（动态分发）
    // 当前仅支持 "opencode"，未来加 "claude-code" 等
    let db: Box<dyn AgentAdapter> = match config.agent.as_str() {
        "opencode" => {
            // v2.47：opencode_db 已在 match 之前回填，这里直接 unwrap
            let db_path = config.opencode_db.as_ref().expect("opencode_db 已在上方回填");

            tracing::info!(
                db_path = %db_path.display(),
                state_path = %state_path.display(),
                memorycenter_url = %config.memorycenter_url,
                poll_interval_secs = config.poll_interval,
                project_id = %config.project_id,
                backfill = config.backfill,
                agent = %config.agent,
                "mc-sidecar 启动（v2.46 多 Agent adapter + 状态持久化）"
            );

            // 检查 db 文件是否存在
            if !db_path.exists() {
                tracing::error!(db_path = %db_path.display(), "OpenCode SQLite 文件不存在");
                tracing::error!("请确认 OpenCode 已安装并至少运行过一次");
                std::process::exit(1);
            }

            // 打开数据库
            match OpenCodeDb::open(&db_path) {
                Ok(db) => Box::new(db),
                Err(e) => {
                    tracing::error!(error = %e, "打开 OpenCode SQLite 失败");
                    std::process::exit(1);
                }
            }
        }
        other => {
            tracing::error!(agent = other, "不支持的 agent 类型，当前仅支持: opencode");
            std::process::exit(1);
        }
    };

    // 加载持久化状态（v2.41 新增）
    let sidecar_state = match SidecarState::load(&state_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "状态文件加载失败，使用空状态继续");
            SidecarState::default()
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
        match watcher.backfill_events(db.as_ref()) {
            Ok(events) => {
                tracing::info!(count = events.len(), "发现未归档的历史 compaction 事件");
                for event in events {
                    match archive_compaction_event(db.as_ref(), &archive_client, &event, &config).await {
                        Ok(()) => {
                            // 归档成功后标记为已处理（避免主循环 poll 重复归档）
                            watcher.mark_archived(&event);
                            // v2.41：每次归档后保存状态（防止 sidecar 异常退出丢失进度）
                            if let Err(e) = watcher.state().save(&state_path) {
                                tracing::warn!(error = %e, "状态保存失败（不影响本次归档）");
                            }
                        }
                        Err(e) => {
                            // 归档失败不标记为已处理，下次启动会重试
                            tracing::warn!(
                                session_id = %event.compaction.session_id,
                                error = %e,
                                "backfill 归档失败，跳过（下次启动会重试）"
                            );
                        }
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
        match watcher.poll(db.as_ref()) {
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
        let poll_result = match watcher.poll(db.as_ref()) {
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
            match archive_compaction_event(db.as_ref(), &archive_client, &event, &config).await {
                Ok(()) => {
                    // 归档成功后标记为已处理
                    watcher.mark_archived(&event);
                    had_new = true;
                }
                Err(e) => {
                    // 归档失败不标记为已处理，下次 poll 会自动重试
                    tracing::warn!(
                        session_id = %event.compaction.session_id,
                        error = %e,
                        "归档失败，下次 poll 会重试（不标记为已处理）"
                    );
                }
            }
        }

        // v2.41：有新归档时保存状态
        if had_new {
            if let Err(e) = watcher.state().save(&state_path) {
                tracing::warn!(error = %e, "状态保存失败（不影响本次归档）");
            }
        }

        // v2.47：tokens 阈值监控（主动归档 + 清空）
        // 检查每个活跃 session 的 token 累积值，达到阈值时主动归档
        let token_events = match watcher.poll_tokens(
            db.as_ref(),
            config.token_threshold,
            config.token_trigger_ratio,
        ) {
            Ok(events) => events,
            Err(e) => {
                tracing::warn!(error = %e, "tokens 阈值监控轮询失败，等待下次重试");
                continue;
            }
        };

        let mut had_proactive = false;
        for event in token_events {
            match archive_token_event(db.as_ref(), &archive_client, &event, &config).await {
                Ok(()) => {
                    // 主动归档成功，更新 last_archived_seq
                    watcher.mark_proactive_archived(&event.session_id, event.last_seq);
                    had_proactive = true;

                    // v2.47 阶段 3：插入 compaction 消息对（主动清空）
                    // 让 OpenCode 下次加载时跳过旧消息，无需 LLM 压缩
                    if let Some(db_path) = &config.opencode_db {
                        match opencode_writer::query_tail_start_id(
                            db_path,
                            &event.session_id,
                            config.tail_turns,
                        ) {
                            Ok(tail_start_id) => {
                                // 用 memory-center 归档摘要作为 compaction summary
                                let summary = format!(
                                    "MemoryCenter 主动归档（tokens={}/threshold={}）\n\n\
                                    上下文已归档到 MemoryCenter，请调用 \
                                    mcp_memory-center.prompt(session_id) 获取历史记忆。",
                                    event.accumulated_tokens, event.threshold
                                );

                                match opencode_writer::insert_compaction_pair(
                                    db_path,
                                    &event.session_id,
                                    &summary,
                                    &tail_start_id,
                                    "memory_center_proactive",
                                ) {
                                    Ok((user_msg_id, assistant_msg_id)) => {
                                        tracing::info!(
                                            session_id = %event.session_id,
                                            last_seq = event.last_seq,
                                            accumulated_tokens = event.accumulated_tokens,
                                            tail_start_id = %tail_start_id,
                                            user_msg_id = %user_msg_id,
                                            assistant_msg_id = %assistant_msg_id,
                                            "主动归档 + compaction 消息对插入完成（上下文已清空）"
                                        );
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            session_id = %event.session_id,
                                            error = %e,
                                            "compaction 消息对插入失败（归档已成功，但上下文未清空，下次 OpenCode compaction 时会兜底）"
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    session_id = %event.session_id,
                                    error = %e,
                                    "查询 tail_start_id 失败，跳过 compaction 消息对插入"
                                );
                            }
                        }
                    } else {
                        tracing::warn!(
                            "未配置 --opencode-db，跳过 compaction 消息对插入"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        session_id = %event.session_id,
                        error = %e,
                        "主动归档失败，下次 poll_tokens 会重试"
                    );
                }
            }
        }

        if had_proactive {
            if let Err(e) = watcher.state().save(&state_path) {
                tracing::warn!(error = %e, "状态保存失败（不影响本次主动归档）");
            }
        }
    }
}

/// 归档单个 compaction 事件（v2.39 新增，v2.43 改为结构化 turns）
///
/// 增量归档：读取 (from_seq, compaction.seq) 之间的消息，
/// 附加 compaction summary 作为合成 turn，调用 MemoryCenter pre-compress 端点。
///
/// v2.43 改动：
/// - 读取结构化 turns（保留 tool_calls/thinking），替代拼接字符串
/// - compaction summary 作为额外的合成 SidecarTurn 追加，让服务端 apply_turn_defaults 推断 tags
/// - token 估算基于各 turn 的文本/tool 内容长度总和
async fn archive_compaction_event(
    db: &dyn AgentAdapter,
    archive_client: &ArchiveClient,
    event: &CompactionChangeEvent,
    config: &SidecarConfig,
) -> Result<(), String> {
    let compaction = &event.compaction;

    tracing::info!(
        session_id = %compaction.session_id,
        message_id = %compaction.message_id,
        seq = compaction.seq,
        reason = %compaction.reason,
        from_seq = ?event.from_seq,
        "开始增量归档 compaction 事件（v2.43 结构化 turns）"
    );

    // v2.43：读取结构化 turns（保留 tool_calls/thinking）
    // v2.46：通过 trait 方法调用（原 read_session_turns_between → read_turns_between）
    let mut turns = match db.read_turns_between(
        &compaction.session_id,
        event.from_seq,
        compaction.seq,
        config.max_turns,
    ) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(
                session_id = %compaction.session_id,
                error = %e,
                "读取增量 turns 失败"
            );
            return Err(format!("读取增量 turns 失败: {e}"));
        }
    };

    // 空检查（风险 4 修复）：竞态场景下主动归档可能已更新 last_archived_seq，
    // 导致增量范围为空。此时跳过，避免归档只含 summary_turn 的无价值内容。
    if turns.is_empty() {
        tracing::info!(
            session_id = %compaction.session_id,
            "增量范围内无 turns，跳过（可能已被主动归档）"
        );
        return Ok(());
    }

    // 附加 compaction summary 作为高价值标签（决策点2）
    // v2.43：作为额外的合成 SidecarTurn 追加，让服务器 apply_turn_defaults 推断 tags
    // - user_message：标记这是压缩摘要
    // - llm_message：压缩摘要内容 + recent 保留上下文
    let summary_turn = SidecarTurn {
        user_message: SidecarContent::text_only(format!(
            "System: 压缩摘要（reason={}）",
            compaction.reason
        )),
        llm_message: SidecarContent::text_only(format!(
            "{}\n\n--- Recent Context ---\n{}",
            compaction.summary, compaction.recent
        )),
        token_count: None,      // 压缩摘要无真实 token 数
        stop_reason: None,      // 压缩摘要无停止原因
        cost: None,             // 压缩摘要无成本
    };
    turns.push(summary_turn);

    // v2.44：优先用真实 token_count，缺失时回退到长度估算
    // 真实值来源：opencode step-finish part 的 input + output + reasoning
    let (estimated_tokens, real_count, fallback_count) = {
        let mut real_total: usize = 0;
        let mut fallback_total: usize = 0;
        let mut has_real = false;
        for t in &turns {
            if let Some(tc) = t.token_count {
                real_total += tc;
                has_real = true;
            } else {
                // 对缺失真实值的 turn，用长度估算
                let user_len = t.user_message.text.as_ref().map(|s| s.len()).unwrap_or(0);
                let llm_len = t.llm_message.text.as_ref().map(|s| s.len()).unwrap_or(0);
                let thinking_len = t.llm_message.thinking.as_ref().map(|s| s.len()).unwrap_or(0);
                let tool_len: usize = t
                    .llm_message
                    .tool_calls
                    .iter()
                    .map(|c| c.arguments.len() + c.result.len())
                    .sum();
                let estimated = (user_len + llm_len + thinking_len + tool_len) / 3;
                real_total += estimated;
                fallback_total += estimated;
            }
        }
        (real_total, if has_real { real_total } else { 0 }, fallback_total)
    };

    tracing::info!(
        session_id = %compaction.session_id,
        turns_count = turns.len(),
        estimated_tokens,
        real_tokens = real_count,
        fallback_tokens = fallback_count,
        from_seq = ?event.from_seq,
        to_seq = compaction.seq,
        "读取增量 turns 完成，调用 MemoryCenter pre-compress"
    );

    // 调用 MemoryCenter 归档（v2.43 传结构化 turns）
    match archive_client
        .pre_compress(&compaction.session_id, turns, estimated_tokens, &config.project_id)
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
            Ok(())
        }
        Err(e) => {
            tracing::error!(
                session_id = %compaction.session_id,
                compaction_seq = compaction.seq,
                error = %e,
                "归档失败（不标记为已处理，下次 poll 会重试）"
            );
            Err(format!("归档失败: {e}"))
        }
    }
}

/// 主动归档 token 阈值事件（v2.47 新增）
///
/// 与 `archive_compaction_event` 类似，但触发源是 tokens 阈值监控（而非 compaction 消息）。
/// 归档范围：`(from_seq, last_seq)` 之间的结构化 turns。
///
/// v2.47 阶段 2：只做归档，不插入 compaction 消息对（阶段 3 实现）。
async fn archive_token_event(
    db: &dyn AgentAdapter,
    archive_client: &ArchiveClient,
    event: &TokenThresholdEvent,
    config: &SidecarConfig,
) -> Result<(), String> {
    tracing::info!(
        session_id = %event.session_id,
        accumulated_tokens = event.accumulated_tokens,
        threshold = event.threshold,
        ratio_percent = event.ratio_percent,
        from_seq = ?event.from_seq,
        last_seq = event.last_seq,
        "开始主动归档（tokens 阈值触发）"
    );

    // 读取结构化 turns（与 archive_compaction_event 相同的读取逻辑）
    let turns = match db.read_turns_between(
        &event.session_id,
        event.from_seq,
        event.last_seq,
        config.max_turns,
    ) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(
                session_id = %event.session_id,
                error = %e,
                "主动归档：读取增量 turns 失败"
            );
            return Err(format!("读取增量 turns 失败: {e}"));
        }
    };

    if turns.is_empty() {
        tracing::info!(
            session_id = %event.session_id,
            "主动归档：增量范围内无 turns，跳过（可能已被 compaction 归档）"
        );
        return Ok(());
    }

    tracing::info!(
        session_id = %event.session_id,
        turns_count = turns.len(),
        accumulated_tokens = event.accumulated_tokens,
        "主动归档：读取增量 turns 完成，调用 MemoryCenter pre-compress"
    );

    // 调用 MemoryCenter 归档
    match archive_client
        .pre_compress(
            &event.session_id,
            turns,
            event.accumulated_tokens,
            &config.project_id,
        )
        .await
    {
        Ok(resp) => {
            tracing::info!(
                session_id = %event.session_id,
                last_seq = event.last_seq,
                hook_id = %resp.hook_id,
                parse_success = resp.parse_success,
                parsed_turns = resp.parsed_turns_count,
                archived_tokens = resp.archived_tokens,
                threshold = resp.threshold,
                ratio_percent = resp.threshold_ratio_percent,
                suggestion = %resp.suggestion,
                "主动归档成功"
            );
            Ok(())
        }
        Err(e) => {
            tracing::error!(
                session_id = %event.session_id,
                last_seq = event.last_seq,
                error = %e,
                "主动归档失败（不更新 last_archived_seq，下次 poll_tokens 会重试）"
            );
            Err(format!("主动归档失败: {e}"))
        }
    }
}
