//! # 压缩事件检测器（v2.36 新增，v2.39 重构）
//!
//! 检测 OpenCode 压缩完成事件，触发归档。
//!
//! ## 检测原理（v2.39 重构）
//!
//! **旧策略（v2.36，已废弃）**：监控 `session.time_compacting` 字段变化
//! **问题**：该字段在 OpenCode 源码中从未被写入
//!
//! **新策略（v2.39）**：轮询 `session_message` 表中 `type='compaction'` 的新消息
//!
//! OpenCode 压缩流程（compaction.ts）：
//! 1. `compactAfterOverflow` 触发，发布 `Compaction.Started` 事件
//! 2. LLM 生成摘要
//! 3. 发布 `Compaction.Ended` 事件，**往 session_message 表插入 type='compaction' 消息**
//!
//! sidecar 维护 `已处理的 compaction 消息 ID 集合`，
//! 每次轮询查询所有 compaction 消息，发现新消息 ID 即触发归档。
//!
//! ## 增量归档
//!
//! 对每个 session，记录上次归档的 compaction seq，
//! 归档范围 = (上次 seq, 本次 seq) 之间的消息，天然不重复。
//!
//! ## backfill 模式
//!
//! 启动时若 `--backfill` 为 true，查询所有有 compaction 消息的 session，
//! 对每条 compaction 消息执行增量归档。

// v2.46：从 adapter crate 导入 AgentAdapter trait + CompactionRecord + AdapterError
// watcher 不再直接依赖 OpenCodeDb 类型，通过 trait 交互
use memory_center_adapter::{AgentAdapter, AdapterError, CompactionRecord};
use crate::state::SidecarState;
use std::collections::HashMap;

/// 压缩事件检测器（v2.39 重构，v2.41 改为持有 SidecarState 引用）
pub struct CompactionWatcher {
    /// 持久化状态（v2.41：从内部维护改为外部传入，支持重启后恢复）
    state: SidecarState,
}

/// 单次轮询检测结果
#[derive(Debug)]
pub struct PollResult {
    /// 新检测到的 compaction 事件（需要归档）
    pub new_compactions: Vec<CompactionChangeEvent>,
    /// 已处理的 compaction 消息总数
    pub processed_count: usize,
    /// 总 compaction 消息数（含已处理）
    pub total_compactions: usize,
}

/// 单个 compaction 事件（v2.39 新增）
#[derive(Debug, Clone)]
pub struct CompactionChangeEvent {
    /// 触发归档的 compaction 消息
    pub compaction: CompactionRecord,
    /// 上次 compaction 的 seq（增量归档用，None 表示从会话开头）
    pub from_seq: Option<i64>,
}

impl CompactionWatcher {
    /// 创建新的检测器（v2.41：接收外部 SidecarState）
    pub fn new(state: SidecarState) -> Self {
        Self { state }
    }

    /// 获取状态的不可变引用（用于保存）
    pub fn state(&self) -> &SidecarState {
        &self.state
    }

    /// 执行一次轮询（v2.39 重构，v2.41 改用 SidecarState）
    ///
    /// 查询所有 compaction 消息，对比已处理集合，
    /// 返回新检测到的 compaction 事件列表。
    pub fn poll(&mut self, db: &dyn AgentAdapter) -> Result<PollResult, AdapterError> {
        let all_compactions = db.query_compactions()?;
        let total = all_compactions.len();

        let mut new_compactions = Vec::new();

        for compaction in all_compactions {
            // 跳过已处理的消息
            if self.state.is_processed(&compaction.message_id) {
                continue;
            }

            // 获取上次归档的 seq（增量归档范围起点）
            let from_seq = self.state.get_last_seq(&compaction.session_id);

            tracing::info!(
                session_id = %compaction.session_id,
                message_id = %compaction.message_id,
                seq = compaction.seq,
                reason = %compaction.reason,
                from_seq = ?from_seq,
                "检测到新的压缩事件"
            );

            new_compactions.push(CompactionChangeEvent {
                compaction,
                from_seq,
            });
        }

        let processed_count = total - new_compactions.len();

        Ok(PollResult {
            new_compactions,
            processed_count,
            total_compactions: total,
        })
    }

    /// 标记 compaction 事件已归档（v2.39 新增，v2.41 改用 SidecarState）
    ///
    /// 归档成功后调用，更新状态：
    /// - 将 message_id 加入已处理集合
    /// - 更新该 session 的 last_archived_seq
    pub fn mark_archived(&mut self, event: &CompactionChangeEvent) {
        self.state.mark_archived(
            &event.compaction.message_id,
            &event.compaction.session_id,
            event.compaction.seq,
        );
    }

    /// backfill 模式：获取所有历史 compaction 事件（v2.39 重构，v2.41 改用 SidecarState）
    ///
    /// 查询所有 compaction 消息，对每个 session 按 seq 排序，
    /// 返回所有未处理的 compaction 事件（含 from_seq 用于增量归档）。
    pub fn backfill_events(
        &mut self,
        db: &dyn AgentAdapter,
    ) -> Result<Vec<CompactionChangeEvent>, AdapterError> {
        let all_compactions = db.query_compactions()?;

        // 按 session_id 分组，每个 session 内按 seq 排序
        let mut by_session: HashMap<String, Vec<CompactionRecord>> = HashMap::new();
        for c in all_compactions {
            by_session.entry(c.session_id.clone()).or_default().push(c);
        }
        for vec in by_session.values_mut() {
            vec.sort_by_key(|c| c.seq);
        }

        let mut result = Vec::new();
        let mut skipped = 0;
        for (_session_id, compactions) in &by_session {
            let mut prev_seq: Option<i64> = None;
            for compaction in compactions {
                // 跳过已处理的（v2.41：从持久化状态恢复）
                if self.state.is_processed(&compaction.message_id) {
                    prev_seq = Some(compaction.seq);
                    skipped += 1;
                    continue;
                }

                result.push(CompactionChangeEvent {
                    compaction: compaction.clone(),
                    from_seq: prev_seq,
                });
                prev_seq = Some(compaction.seq);
            }
        }

        tracing::info!(
            backfill_count = result.len(),
            skipped_count = skipped,
            session_count = by_session.len(),
            "backfill 扫描完成（已跳过 {} 条已归档的 compaction）",
            skipped
        );

        Ok(result)
    }
}

impl Default for CompactionWatcher {
    fn default() -> Self {
        Self::new(SidecarState::default())
    }
}

// ============================================================================
// v2.47 新增：tokens 阈值监控（主动归档）
// ============================================================================

/// Token 阈值触发事件（v2.47 新增）
///
/// 当某 session 的累积 tokens 达到阈值 * 触发比例时产生此事件，
/// 主循环据此执行主动归档 + 插入 compaction 消息对。
///
/// v2.48：主动清空逻辑已回退，此结构体暂时未使用，保留供未来方案探索复用。
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct TokenThresholdEvent {
    /// 触发的 session ID
    pub session_id: String,
    /// 当前累积 tokens（从 last_archived_seq 到 last_seq）
    pub accumulated_tokens: usize,
    /// 使用的阈值（已解析：CLI > 服务器缓存 > 默认 120000）
    pub threshold: usize,
    /// 触发比例（如 80 表示 80%）
    pub ratio_percent: u64,
    /// 当前最新 seq（作为本次主动归档的范围终点）
    pub last_seq: i64,
    /// 上次归档的 seq（作为本次主动归档的范围起点，None 表示从会话开头）
    pub from_seq: Option<i64>,
}

impl CompactionWatcher {
    /// Token 阈值监控轮询（v2.47 新增）
    ///
    /// 查询所有活跃 session 的 token 累积值，检查是否达到触发阈值。
    /// 达到阈值的 session 返回 `TokenThresholdEvent`，由主循环执行主动归档。
    ///
    /// ## 阈值解析优先级
    ///
    /// 1. CLI 参数 `--token-threshold` 非 0 → 直接使用
    /// 2. 服务器缓存的 `cached_threshold`（从归档响应获取）非 0 → 使用缓存
    /// 3. 最终降级到默认 120000
    ///
    /// ## 触发条件
    ///
    /// `accumulated_tokens >= threshold * ratio_percent / 100`
    ///
    /// ## 参数
    ///
    /// - `db`：Agent 数据源适配器
    /// - `cli_threshold`：CLI 参数 `--token-threshold`（0 表示未配置）
    /// - `ratio_percent`：触发比例（默认 80）
    ///
    /// v2.48：主动清空逻辑已回退，此方法暂时未使用，保留供未来方案探索复用。
    #[allow(dead_code)]
    pub fn poll_tokens(
        &self,
        db: &dyn AgentAdapter,
        cli_threshold: usize,
        ratio_percent: u64,
    ) -> Result<Vec<TokenThresholdEvent>, AdapterError> {
        // 解析阈值：CLI > 服务器缓存 > 默认 120000
        let threshold = if cli_threshold > 0 {
            cli_threshold
        } else if self.state.cached_threshold > 0 {
            self.state.cached_threshold
        } else {
            120_000
        };

        // 触发线 = threshold * ratio / 100
        let trigger_line = threshold * (ratio_percent as usize) / 100;

        // 查询所有活跃 session 的 token 累积值
        let sessions = db.query_active_sessions_tokens(&self.state.last_archived_seq)?;

        let mut events = Vec::new();
        for info in sessions {
            if info.accumulated_tokens >= trigger_line {
                let from_seq = self.state.get_last_seq(&info.session_id);

                tracing::info!(
                    session_id = %info.session_id,
                    accumulated_tokens = info.accumulated_tokens,
                    threshold,
                    trigger_line,
                    ratio_percent,
                    last_seq = info.last_seq,
                    from_seq = ?from_seq,
                    "检测到 token 阈值触发（主动归档）"
                );

                events.push(TokenThresholdEvent {
                    session_id: info.session_id,
                    accumulated_tokens: info.accumulated_tokens,
                    threshold,
                    ratio_percent,
                    last_seq: info.last_seq,
                    from_seq,
                });
            } else {
                tracing::debug!(
                    session_id = %info.session_id,
                    accumulated_tokens = info.accumulated_tokens,
                    trigger_line,
                    ratio_percent,
                    "session token 未达阈值，跳过"
                );
            }
        }

        Ok(events)
    }

    /// 主动归档完成后更新状态（v2.47 新增）
    ///
    /// 与 `mark_archived` 类似，但不依赖 CompactionChangeEvent（主动归档无 compaction 消息）。
    /// 更新 `last_archived_seq` 为 `last_seq`，让下次 poll_tokens 不重复计算已归档部分。
    ///
    /// 注意：不更新 `processed_message_ids`（那是 compaction 消息 ID 去重用的）。
    ///
    /// v2.48：主动清空逻辑已回退，此方法暂时未使用，保留供未来方案探索复用。
    #[allow(dead_code)]
    pub fn mark_proactive_archived(&mut self, session_id: &str, last_seq: i64) {
        self.state
            .last_archived_seq
            .insert(session_id.to_string(), last_seq);
    }

    /// 更新服务器返回的阈值缓存（v2.47 新增）
    ///
    /// 归档响应中含 `threshold` 字段时，缓存到 state，
    /// 后续 poll_tokens 在 CLI 未配置阈值时使用此缓存。
    ///
    /// v2.48：主动清空逻辑已回退，此方法暂时未使用，保留供未来方案探索复用。
    #[allow(dead_code)]
    pub fn update_cached_threshold(&mut self, threshold: usize) {
        if threshold > 0 && self.state.cached_threshold != threshold {
            tracing::debug!(
                old = self.state.cached_threshold,
                new = threshold,
                "更新服务器阈值缓存"
            );
            self.state.cached_threshold = threshold;
        }
    }
}
