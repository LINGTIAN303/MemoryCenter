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

use crate::opencode_db::{CompactionRecord, OpenCodeDb};
use std::collections::{HashMap, HashSet};

/// 压缩事件检测器（v2.39 重构）
pub struct CompactionWatcher {
    /// 已处理的 compaction 消息 ID 集合（msg_xxx），避免重复归档
    processed_message_ids: HashSet<String>,
    /// 每个 session 上次归档的 compaction seq（用于增量归档）
    ///
    /// key: session_id, value: 上次 compaction 的 seq
    /// None 表示该 session 尚未归档过任何 compaction
    last_archived_seq: HashMap<String, i64>,
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
    /// 创建新的检测器
    pub fn new() -> Self {
        Self {
            processed_message_ids: HashSet::new(),
            last_archived_seq: HashMap::new(),
        }
    }

    /// 执行一次轮询（v2.39 重构）
    ///
    /// 查询所有 compaction 消息，对比已处理集合，
    /// 返回新检测到的 compaction 事件列表。
    pub fn poll(&mut self, db: &OpenCodeDb) -> Result<PollResult, crate::opencode_db::DbError> {
        let all_compactions = db.query_all_compactions()?;
        let total = all_compactions.len();

        let mut new_compactions = Vec::new();

        for compaction in all_compactions {
            // 跳过已处理的消息
            if self.processed_message_ids.contains(&compaction.message_id) {
                continue;
            }

            // 获取上次归档的 seq（增量归档范围起点）
            let from_seq = self.last_archived_seq.get(&compaction.session_id).copied();

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

    /// 标记 compaction 事件已归档（v2.39 新增）
    ///
    /// 归档成功后调用，更新内部状态：
    /// - 将 message_id 加入已处理集合
    /// - 更新该 session 的 last_archived_seq
    pub fn mark_archived(&mut self, event: &CompactionChangeEvent) {
        self.processed_message_ids
            .insert(event.compaction.message_id.clone());
        self.last_archived_seq
            .insert(event.compaction.session_id.clone(), event.compaction.seq);
    }

    /// backfill 模式：获取所有历史 compaction 事件（v2.39 重构）
    ///
    /// 查询所有 compaction 消息，对每个 session 按 seq 排序，
    /// 返回所有未处理的 compaction 事件（含 from_seq 用于增量归档）。
    pub fn backfill_events(
        &mut self,
        db: &OpenCodeDb,
    ) -> Result<Vec<CompactionChangeEvent>, crate::opencode_db::DbError> {
        let all_compactions = db.query_all_compactions()?;

        // 按 session_id 分组，每个 session 内按 seq 排序
        let mut by_session: HashMap<String, Vec<CompactionRecord>> = HashMap::new();
        for c in all_compactions {
            by_session.entry(c.session_id.clone()).or_default().push(c);
        }
        for vec in by_session.values_mut() {
            vec.sort_by_key(|c| c.seq);
        }

        let mut result = Vec::new();
        for (_session_id, compactions) in &by_session {
            let mut prev_seq: Option<i64> = None;
            for compaction in compactions {
                // 跳过已处理的
                if self.processed_message_ids.contains(&compaction.message_id) {
                    prev_seq = Some(compaction.seq);
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
            session_count = by_session.len(),
            "backfill 扫描完成"
        );

        Ok(result)
    }
}

impl Default for CompactionWatcher {
    fn default() -> Self {
        Self::new()
    }
}
