//! # OpenCode SQLite 读取模块（v2.36 新增，v2.39 重构）
//!
//! 从 OpenCode 的会话 SQLite 数据库读取压缩事件和会话消息。
//!
//! ## OpenCode SQLite Schema（来自 sst/opencode 源码）
//!
//! ### session 表
//! - `id` (text PK): Session ID
//! - `title` (text): 会话标题
//! - `time_compacting` (integer|null): 遗留字段，v2.39 确认源码未写入，不作为检测依据
//! - `time_created` / `time_updated`: 时间戳
//!
//! ### session_message 表（V2 消息系统）
//! - `id` (text PK): 消息 ID（msg_xxx）
//! - `session_id` (text FK): 所属 session
//! - `type` (text): 消息类型（user/assistant/system/synthetic/shell/compaction）
//! - `seq` (integer): 序列号（同 session 内递增）
//! - `data` (text JSON): 消息内容（不含 id 和 type）
//!   - compaction 消息的 data 含 `summary`/`recent`/`reason` 字段
//! - `time_created` (integer): 创建时间
//!
//! ### message + part 表（V1 消息系统，回退用）
//! - `message.id` / `message.session_id` / `message.data` (JSON)
//! - `part.id` / `part.message_id` / `part.session_id` / `part.data` (JSON)
//!
//! ## 压缩检测策略（v2.39 重构）
//!
//! **旧策略（v2.36，已废弃）**：监控 `session.time_compacting` 字段变化
//! **问题**：该字段在 OpenCode 源码（compaction.ts）中从未被写入，检测基础不成立
//!
//! **新策略（v2.39）**：轮询 `session_message` 表中 `type='compaction'` 的新消息
//! - 压缩完成后，OpenCode 往 session_message 表插入一条 compaction 消息
//! - sidecar 记录已处理的 compaction 消息 ID，检测新消息即触发归档
//! - 归档范围：上次 compaction（exclusive）到本次 compaction（exclusive）之间的消息（增量归档）

use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::collections::HashSet;

use crate::archive::{SidecarContent, SidecarFileChange, SidecarToolCall, SidecarTurn};

/// OpenCode SQLite 读取器
pub struct OpenCodeDb {
    conn: Connection,
}

/// V1 part 结构化内容（v2.42 新增，v2.44 加 tokens_detail，v2.45 加完整 tool/patch/step-finish 字段）
///
/// 从 part 表提取的完整信息，保留所有 part 类型的所有字段。
/// v2.45 目标：信息零损失，让归档内容与原始 part 数据一致。
#[derive(Debug, Clone)]
struct V1PartContent {
    /// part 类型: "text" / "reasoning" / "tool" / "step-start" / "step-finish" / "patch" / 其他
    part_type: String,
    /// 主要文本（text/reasoning 的 text 字段）
    text: Option<String>,
    /// 工具名称（tool 类型才有，如 "bash"/"read"/"edit"/"websearch"/"webfetch"）
    tool_name: Option<String>,
    /// 工具输入（tool 类型，JSON 字符串，含 command/filePath/query 等）
    tool_input: Option<String>,
    /// 工具输出（tool 类型，已完成时的 output 字段）
    tool_output: Option<String>,
    /// token 总数（step-finish 类型，从 tokens.total 提取，累计值含 cache read）
    tokens_total: Option<i64>,
    /// 单轮实际 token 消耗（v2.44 新增，step-finish 的 input + output + reasoning）
    tokens_consumed: Option<usize>,
    // v2.45 新增：tool part 完整字段
    /// 工具执行状态（state.status: "completed"/"error"/"running"/"pending"）
    tool_status: Option<String>,
    /// 工具错误信息（state.status="error" 时的 state.error 字符串）
    tool_error: Option<String>,
    /// 工具调用唯一标识（callID 字段）
    tool_call_id: Option<String>,
    /// 工具执行开始时间（毫秒时间戳，state.time.start）
    tool_time_start: Option<i64>,
    /// 工具执行结束时间（毫秒时间戳，state.time.end）
    tool_time_end: Option<i64>,
    // v2.45 新增：patch part 字段
    /// patch 的 git hash
    patch_hash: Option<String>,
    /// patch 影响的文件路径列表
    patch_files: Vec<String>,
    // v2.45 新增：step-finish part 完整字段
    /// LLM 停止原因（reason 字段：stop/length/tool_use/max_tokens 等）
    step_finish_reason: Option<String>,
    /// 单轮成本（cost 字段，单位：美元）
    step_finish_cost: Option<f64>,
}

/// Session 基本信息
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    pub time_compacting: Option<i64>,
}

/// compaction 消息记录（v2.39 新增）
///
/// 对应 session_message 表中 type='compaction' 的一行。
#[derive(Debug, Clone)]
pub struct CompactionRecord {
    /// 消息 ID（msg_xxx），用于去重
    pub message_id: String,
    /// 所属 session ID
    pub session_id: String,
    /// seq 序列号（用于确定归档范围）
    pub seq: i64,
    /// 创建时间戳（毫秒）
    pub time_created: i64,
    /// 压缩原因："auto" 或 "manual"
    pub reason: String,
    /// LLM 生成的压缩摘要
    pub summary: String,
    /// 保留的最近上下文
    pub recent: String,
}

/// 压缩状态变化检测结果（v2.39 重构）
#[derive(Debug)]
pub struct CompactionChange {
    pub session_id: String,
    pub session_title: String,
    /// 触发归档的 compaction 消息
    pub compaction: CompactionRecord,
}

impl OpenCodeDb {
    /// 以只读模式打开 OpenCode SQLite
    ///
    /// 只读模式避免干扰 OpenCode 的写入，WAL 模式支持并发读。
    pub fn open(db_path: &Path) -> Result<Self, DbError> {
        let conn = Connection::open_with_flags(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        Ok(Self { conn })
    }

    /// 查询所有 session 的压缩状态（v2.39：仅用于日志统计，不再用于检测）
    pub fn query_all_compaction_states(&self) -> Result<Vec<SessionInfo>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, time_compacting FROM session",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(SessionInfo {
                id: row.get::<_, String>(0)?,
                title: row.get::<_, String>(1)?,
                time_compacting: row.get::<_, Option<i64>>(2)?,
            })
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// 查询 session 标题（v2.39 新增）
    ///
    /// 用于 compaction 事件触发时获取 session 标题。
    pub fn query_session_title(&self, session_id: &str) -> Result<String, DbError> {
        let mut stmt = self.conn.prepare("SELECT title FROM session WHERE id = ?1")?;
        let title: Option<String> = stmt.query_row([session_id], |row| row.get(0)).ok();
        Ok(title.unwrap_or_else(|| session_id.to_string()))
    }

    /// 查询所有 compaction 消息（v2.39 新增，v2.40 改为 V2 优先 V1 回退）
    ///
    /// 先查 V2 `session_message` 表（CLI/TUI 版），空则回退 V1 `message` 表（桌面端）。
    /// sidecar 用 message_id 去重，发现新消息即触发归档。
    pub fn query_all_compactions(&self) -> Result<Vec<CompactionRecord>, DbError> {
        // 先试 V2 session_message 表
        let v2_result = self.query_all_compactions_v2()?;
        if !v2_result.is_empty() {
            return Ok(v2_result);
        }

        // V2 为空，回退 V1 message 表（桌面端）
        tracing::debug!("V2 session_message 表无 compaction 数据，回退 V1 message 表");
        self.query_all_compactions_v1()
    }

    /// V2 查询：从 session_message 表检测 compaction（CLI/TUI 版）
    ///
    /// 扫描 session_message 表中所有 type='compaction' 的消息，
    /// 按创建时间升序返回。利用索引 `session_message_session_type_seq_idx` 高效查询。
    fn query_all_compactions_v2(&self) -> Result<Vec<CompactionRecord>, DbError> {
        let mut stmt = match self.conn.prepare(
            "SELECT id, session_id, seq, time_created, data
             FROM session_message
             WHERE type = 'compaction'
             ORDER BY time_created ASC",
        ) {
            Ok(s) => s,
            Err(_) => {
                // session_message 表可能不存在（老版本 OpenCode 或桌面端）
                tracing::debug!("session_message 表查询失败，可能 OpenCode 版本过旧或为桌面端");
                return Ok(Vec::new());
            }
        };

        let rows = stmt.query_map([], |row| {
            let message_id: String = row.get(0)?;
            let session_id: String = row.get(1)?;
            let seq: i64 = row.get(2)?;
            let time_created: i64 = row.get(3)?;
            let data_json: String = row.get(4)?;
            let data: serde_json::Value = serde_json::from_str(&data_json).unwrap_or_default();

            let reason = data.get("reason").and_then(|v| v.as_str()).unwrap_or("auto").to_string();
            let summary = data.get("summary").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let recent = data.get("recent").and_then(|v| v.as_str()).unwrap_or("").to_string();

            Ok(CompactionRecord {
                message_id,
                session_id,
                seq,
                time_created,
                reason,
                summary,
                recent,
            })
        })?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// V1 查询：从 message 表检测 compaction（桌面端，v2.40 新增）
    ///
    /// OpenCode 桌面端不使用 V2 session_message 表，压缩信息存储在
    /// `message.data` JSON 字段中：`{"mode":"compaction","agent":"compaction","summary":true}`。
    ///
    /// 压缩摘要和推理过程在 `part` 表中（通过 message_id 关联）：
    /// - `part.data.type='text'` → summary
    /// - `part.data.type='reasoning'` → recent
    ///
    /// V1 无 seq 字段，用 `time_created`（毫秒时间戳）代替 seq 作为排序和增量范围标识。
    fn query_all_compactions_v1(&self) -> Result<Vec<CompactionRecord>, DbError> {
        let mut stmt = match self.conn.prepare(
            "SELECT id, session_id, time_created, data
             FROM message
             WHERE data LIKE '%\"mode\":\"compaction\"%'
               AND data LIKE '%\"summary\":true%'
             ORDER BY time_created ASC",
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "V1 message 表查询失败");
                return Ok(Vec::new());
            }
        };

        let rows = stmt.query_map([], |row| {
            let message_id: String = row.get(0)?;
            let session_id: String = row.get(1)?;
            let time_created: i64 = row.get(2)?;
            let data_json: String = row.get(3)?;
            Ok((message_id, session_id, time_created, data_json))
        })?;

        let mut result = Vec::new();
        for row in rows {
            let (message_id, session_id, time_created, data_json) = row?;
            let data: serde_json::Value =
                serde_json::from_str(&data_json).unwrap_or_default();

            // V1 的 reason 字段通常不存在，默认 "auto"
            let reason = data
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("auto")
                .to_string();

            // 从 part 表提取压缩摘要和推理过程
            let (summary, recent) = self.read_compaction_parts_v1(&message_id)?;

            tracing::debug!(
                message_id = %message_id,
                session_id = %session_id,
                time_created = time_created,
                summary_len = summary.len(),
                recent_len = recent.len(),
                "V1 compaction 消息解析完成"
            );

            result.push(CompactionRecord {
                message_id,
                session_id,
                seq: time_created, // V1 用 time_created 代替 seq
                time_created,
                reason,
                summary,
                recent,
            });
        }
        Ok(result)
    }

    /// 从 part 表提取 V1 压缩消息的摘要和推理过程（v2.40 新增）
    ///
    /// OpenCode 桌面端的压缩消息内容存储在 part 表中：
    /// - `type='text'`：LLM 生成的压缩摘要文本
    /// - `type='reasoning'`：压缩推理过程
    /// - `type='step-finish'`：步骤完成元数据（跳过）
    ///
    /// 返回 `(summary, recent)`，与 V2 的 CompactionRecord 字段对应。
    fn read_compaction_parts_v1(&self, message_id: &str) -> Result<(String, String), DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT data FROM part WHERE message_id = ?1 ORDER BY time_created ASC",
        )?;

        let rows = stmt.query_map([message_id], |row| row.get::<_, String>(0))?;

        let mut summary_parts = Vec::new();
        let mut reasoning_parts = Vec::new();

        for row in rows {
            let data_json = row?;
            let data: serde_json::Value = serde_json::from_str(&data_json).unwrap_or_default();
            let part_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let text = data.get("text").and_then(|v| v.as_str()).unwrap_or("");

            if text.is_empty() {
                continue;
            }

            match part_type {
                "text" => summary_parts.push(text.to_string()),
                "reasoning" => reasoning_parts.push(text.to_string()),
                _ => {} // step-finish 等跳过
            }
        }

        Ok((summary_parts.join("\n"), reasoning_parts.join("\n")))
    }

    /// 读取 session 中两个 seq 之间的消息（v2.39 新增，v2.40 改为 V2 优先 V1 回退）
    ///
    /// 归档范围：(from_seq, to_seq)，即 from_seq < seq < to_seq
    /// - from_seq：上次 compaction 的 seq（exclusive），None 表示从会话开头
    /// - to_seq：本次 compaction 的 seq（exclusive）
    ///
    /// 跳过 compaction 类型消息本身。
    /// 输出格式：`User: ...\n\nAssistant: ...`
    ///
    /// **V2 路径**：查 `session_message` 表，按 `seq` 范围过滤
    /// **V1 路径**（桌面端）：查 `message` 表，按 `time_created` 范围过滤
    /// （V1 的 CompactionRecord.seq 实际存的是 time_created，所以范围语义一致）
    pub fn read_session_context_between(
        &self,
        session_id: &str,
        from_seq: Option<i64>,
        to_seq: i64,
        max_turns: usize,
    ) -> Result<String, DbError> {
        // 先试 V2 session_message 表
        let v2_context = self.read_session_context_between_v2(session_id, from_seq, to_seq, max_turns)?;
        if !v2_context.is_empty() {
            return Ok(v2_context);
        }

        // V2 为空，回退 V1 message + part 表（桌面端）
        tracing::debug!(
            session_id = %session_id,
            from_seq = ?from_seq,
            to_seq = to_seq,
            "V2 session_message 表无范围数据，回退 V1 message 表"
        );
        self.read_session_context_between_v1(session_id, from_seq, to_seq, max_turns)
    }

    /// 读取 session 中两个 seq 之间的消息，返回结构化 turns（v2.43 新增）
    ///
    /// 与 `read_session_context_between` 相同的归档范围和跳过规则，
    /// 但返回 `Vec<SidecarTurn>` 而非字符串，保留 tool_calls/thinking 等结构化字段。
    ///
    /// sidecar 应优先使用此方法，让服务器端能自动推断 tags 和估算 token_count。
    pub fn read_session_turns_between(
        &self,
        session_id: &str,
        from_seq: Option<i64>,
        to_seq: i64,
        max_turns: usize,
    ) -> Result<Vec<SidecarTurn>, DbError> {
        // 先试 V2 session_message 表
        let v2_turns = self.read_session_turns_between_v2(session_id, from_seq, to_seq, max_turns)?;
        if !v2_turns.is_empty() {
            return Ok(v2_turns);
        }

        // V2 为空，回退 V1 message + part 表（桌面端）
        tracing::debug!(
            session_id = %session_id,
            from_seq = ?from_seq,
            to_seq = to_seq,
            "V2 session_message 表无范围数据，回退 V1 message 表（turns 路径）"
        );
        self.read_session_turns_between_v1(session_id, from_seq, to_seq, max_turns)
    }

    /// V2 版本：从 session_message 表读取 (from_seq, to_seq) 范围内的消息
    fn read_session_context_between_v2(
        &self,
        session_id: &str,
        from_seq: Option<i64>,
        to_seq: i64,
        max_turns: usize,
    ) -> Result<String, DbError> {
        let mut stmt = match self.conn.prepare(
            "SELECT type, data FROM session_message
             WHERE session_id = ?1
               AND seq > ?2
               AND seq < ?3
               AND type != 'compaction'
             ORDER BY seq ASC",
        ) {
            Ok(s) => s,
            Err(_) => {
                // session_message 表可能不存在（桌面端）
                return Ok(String::new());
            }
        };

        let from_seq_val = from_seq.unwrap_or(-1);

        let rows = stmt.query_map(rusqlite::params![session_id, from_seq_val, to_seq], |row| {
            Ok((
                row.get::<_, String>(0)?, // type
                row.get::<_, String>(1)?, // data (JSON)
            ))
        })?;

        let mut parts = Vec::new();
        let mut turn_count = 0usize;

        for row in rows {
            let (msg_type, data_json) = row?;
            let data: serde_json::Value = serde_json::from_str(&data_json).unwrap_or_default();

            let serialized = serialize_v2_message(&msg_type, &data);
            if serialized.is_empty() {
                continue;
            }

            parts.push(serialized);
            turn_count += 1;

            if turn_count >= max_turns {
                parts.push("[... truncated by sidecar max_turns ...]".to_string());
                break;
            }
        }

        Ok(parts.join("\n\n"))
    }

    /// V2 版本：从 session_message 表读取 turns（v2.43 新增）
    ///
    /// 与 `read_session_context_between_v2` 相同的查询范围，但返回结构化 turns。
    /// 正确做 turn 配对（user + 后续 assistant），保留 reasoning/tool 信息。
    fn read_session_turns_between_v2(
        &self,
        session_id: &str,
        from_seq: Option<i64>,
        to_seq: i64,
        max_turns: usize,
    ) -> Result<Vec<SidecarTurn>, DbError> {
        let mut stmt = match self.conn.prepare(
            "SELECT type, data FROM session_message
             WHERE session_id = ?1
               AND seq > ?2
               AND seq < ?3
               AND type != 'compaction'
             ORDER BY seq ASC",
        ) {
            Ok(s) => s,
            Err(_) => {
                return Ok(Vec::new());
            }
        };

        let from_seq_val = from_seq.unwrap_or(-1);

        let rows = stmt.query_map(rusqlite::params![session_id, from_seq_val, to_seq], |row| {
            Ok((
                row.get::<_, String>(0)?, // type
                row.get::<_, String>(1)?, // data (JSON)
            ))
        })?;

        // 先收集所有消息的结构化内容（v2.44：含 tokens，v2.45：含 stop_reason/cost）
        #[derive(Debug)]
        struct V2Msg {
            msg_type: String,
            content: SidecarContent,
            tokens: Option<usize>,
            // v2.45 新增：step-finish 提取的停止原因和成本
            stop_reason: Option<String>,
            cost: Option<f64>,
        }

        let mut messages: Vec<V2Msg> = Vec::new();
        for row in rows {
            let (msg_type, data_json) = row?;
            let data: serde_json::Value = serde_json::from_str(&data_json).unwrap_or_default();

            // v2.45：parse_v2_message_to_content 返回 4-tuple
            let (content, tokens, stop_reason, cost) = parse_v2_message_to_content(&msg_type, &data);
            if content.text.is_none() && content.thinking.is_none() && content.tool_calls.is_empty()
                && content.file_changes.is_empty()
            {
                continue;
            }
            messages.push(V2Msg {
                msg_type,
                content,
                tokens,
                stop_reason,
                cost,
            });
        }

        if messages.is_empty() {
            return Ok(Vec::new());
        }

        // 按 turn 分组（v2.44：追踪 tokens，v2.45：追踪 stop_reason/cost）
        let mut turns: Vec<SidecarTurn> = Vec::new();
        let mut current_user: Option<SidecarContent> = None;
        let mut current_assistant_parts: Vec<SidecarContent> = Vec::new();
        let mut current_turn_tokens: usize = 0;
        let mut has_tokens = false;
        // v2.45：turn 内最后一条 assistant 消息的 stop_reason/cost
        // （同一 turn 内可能有多条 assistant 消息，取最后一条非 None 值）
        let mut current_turn_stop_reason: Option<String> = None;
        let mut current_turn_cost: Option<f64> = None;

        let flush_turn = |user: &Option<SidecarContent>,
                          assistant_parts: &[SidecarContent],
                          turn_tokens: usize,
                          has_tokens: bool,
                          turn_stop_reason: &Option<String>,
                          turn_cost: &Option<f64>,
                          turns: &mut Vec<SidecarTurn>| {
            if user.is_none() && assistant_parts.is_empty() {
                return;
            }
            let user_content = user.clone().unwrap_or_else(|| SidecarContent {
                text: None,
                thinking: None,
                tool_calls: Vec::new(),
                file_changes: Vec::new(),
            });
            let llm_content = merge_sidecar_contents(assistant_parts);
            turns.push(SidecarTurn {
                user_message: user_content,
                llm_message: llm_content,
                token_count: if has_tokens { Some(turn_tokens) } else { None },
                stop_reason: turn_stop_reason.clone(),
                cost: *turn_cost,
            });
        };

        for msg in &messages {
            match msg.msg_type.as_str() {
                "user" => {
                    if current_user.is_some() || !current_assistant_parts.is_empty() {
                        flush_turn(
                            &current_user,
                            &current_assistant_parts,
                            current_turn_tokens,
                            has_tokens,
                            &current_turn_stop_reason,
                            &current_turn_cost,
                            &mut turns,
                        );
                        if turns.len() >= max_turns {
                            return Ok(turns);
                        }
                        current_user = None;
                        current_assistant_parts.clear();
                        current_turn_tokens = 0;
                        has_tokens = false;
                        current_turn_stop_reason = None;
                        current_turn_cost = None;
                    }
                    current_user = Some(msg.content.clone());
                }
                "assistant" => {
                    if let Some(t) = msg.tokens {
                        current_turn_tokens += t;
                        has_tokens = true;
                    }
                    // v2.45：取 turn 内最后一条非 None 的 stop_reason/cost
                    if msg.stop_reason.is_some() {
                        current_turn_stop_reason = msg.stop_reason.clone();
                    }
                    if msg.cost.is_some() {
                        current_turn_cost = msg.cost;
                    }
                    current_assistant_parts.push(msg.content.clone());
                }
                "system" | "synthetic" | "shell" | "compaction" => {
                    // 跳过
                }
                _ => {
                    // 未知类型当 user 处理
                    if current_user.is_some() || !current_assistant_parts.is_empty() {
                        flush_turn(
                            &current_user,
                            &current_assistant_parts,
                            current_turn_tokens,
                            has_tokens,
                            &current_turn_stop_reason,
                            &current_turn_cost,
                            &mut turns,
                        );
                        if turns.len() >= max_turns {
                            return Ok(turns);
                        }
                        current_user = None;
                        current_assistant_parts.clear();
                        current_turn_tokens = 0;
                        has_tokens = false;
                        current_turn_stop_reason = None;
                        current_turn_cost = None;
                    }
                    current_user = Some(msg.content.clone());
                }
            }
        }

        flush_turn(
            &current_user,
            &current_assistant_parts,
            current_turn_tokens,
            has_tokens,
            &current_turn_stop_reason,
            &current_turn_cost,
            &mut turns,
        );
        Ok(turns)
    }

    /// V1 版本：从 message + part 表读取 (from_seq, to_seq) 范围内的消息（桌面端，v2.40 新增，v2.42 重构）
    ///
    /// V1 无 seq 字段，用 `time_created`（毫秒时间戳）代替。
    /// 由于 `query_all_compactions_v1` 已将 `time_created` 存入 `CompactionRecord.seq`，
    /// 所以 `(from_seq, to_seq)` 实际上是 `time_created` 范围，语义一致。
    ///
    /// 跳过 `data.mode = 'compaction'` 的压缩产物消息。
    /// 跳过 restore checkpoint 消息（text 以 `[restore checkpointed` 开头的 user 消息）。
    ///
    /// ## v2.42 重构：完整信息提取
    ///
    /// 旧版（v2.40）只提取 `text` 类型的 part，丢失了 reasoning/tool/step-finish，
    /// 导致归档后信息不完整（tool_calls 和 thinking 全部丢失）。
    ///
    /// 新版（v2.42）使用 `read_v1_parts_structured` 提取所有 part 类型，
    /// 将 reasoning 放入 `<thinking>` 标记，tool 放入 `<tool_call>` 标记，
    /// 生成包含完整信息的 `User:`/`Assistant:` 格式。
    fn read_session_context_between_v1(
        &self,
        session_id: &str,
        from_seq: Option<i64>,
        to_seq: i64,
        max_turns: usize,
    ) -> Result<String, DbError> {
        let from_seq_val = from_seq.unwrap_or(0);

        let mut stmt = match self.conn.prepare(
            "SELECT id, data, time_created FROM message
             WHERE session_id = ?1
               AND time_created > ?2
               AND time_created < ?3
             ORDER BY time_created ASC, id ASC",
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "V1 message 表查询失败");
                return Ok(String::new());
            }
        };

        let rows = stmt.query_map(rusqlite::params![session_id, from_seq_val, to_seq], |row| {
            Ok((
                row.get::<_, String>(0)?, // id
                row.get::<_, String>(1)?, // data (JSON)
                row.get::<_, i64>(2)?,    // time_created
            ))
        })?;

        // 先收集所有消息的结构化内容
        #[derive(Debug)]
        struct MsgEntry {
            role: String,
            time_created: i64,
            parts: Vec<V1PartContent>,
        }

        let mut messages: Vec<MsgEntry> = Vec::new();

        for row in rows {
            let (msg_id, msg_data_json, time_created) = row?;
            let msg_data: serde_json::Value =
                serde_json::from_str(&msg_data_json).unwrap_or_default();

            // 跳过压缩产物消息（mode=compaction）
            let mode = msg_data.get("mode").and_then(|v| v.as_str()).unwrap_or("");
            if mode == "compaction" {
                continue;
            }

            // 提取结构化 parts
            let parts = self.read_v1_parts_structured(&msg_id)?;
            if parts.is_empty() {
                continue;
            }

            // 跳过 restore checkpoint 消息（user 消息且 text 以 [restore checkpointed 开头）
            let role = msg_data
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            if role == "user" {
                let first_text = parts
                    .iter()
                    .find_map(|p| p.text.as_deref())
                    .unwrap_or("");
                if first_text.starts_with("[restore checkpointed") {
                    tracing::debug!(
                        msg_id = %msg_id,
                        "跳过 restore checkpoint 消息"
                    );
                    continue;
                }
            }

            messages.push(MsgEntry {
                role,
                time_created,
                parts,
            });
        }

        if messages.is_empty() {
            return Ok(String::new());
        }

        // 将消息按 turn 分组：user 消息开始一个新 turn，后续的 assistant 消息归入同 turn
        // 格式: User: ...\n\nAssistant: ...
        // 一个 turn = 一个 user 消息 + 后续所有 assistant 消息（直到下一个 user 消息）
        let mut output_parts: Vec<String> = Vec::new();
        let mut turn_count = 0usize;
        let mut current_user_text = String::new();
        let mut current_assistant_parts: Vec<String> = Vec::new();
        let mut has_user = false;

        let flush_turn = |user_text: &str, assistant_parts: &[String], output: &mut Vec<String>, turn_count: &mut usize| {
            if user_text.is_empty() && assistant_parts.is_empty() {
                return;
            }
            let mut turn_str = String::new();
            if !user_text.is_empty() {
                turn_str.push_str("User: ");
                turn_str.push_str(user_text);
            }
            if !assistant_parts.is_empty() {
                if !turn_str.is_empty() {
                    turn_str.push_str("\n\n");
                }
                turn_str.push_str("Assistant: ");
                turn_str.push_str(&assistant_parts.join("\n\n"));
            }
            output.push(turn_str);
            *turn_count += 1;
        };

        for msg in &messages {
            match msg.role.as_str() {
                "user" => {
                    // 遇到新 user 消息，先 flush 上一个 turn
                    if has_user {
                        flush_turn(
                            &current_user_text,
                            &current_assistant_parts,
                            &mut output_parts,
                            &mut turn_count,
                        );
                        if turn_count >= max_turns {
                            output_parts.push(
                                "[... truncated by sidecar max_turns ...]".to_string(),
                            );
                            return Ok(output_parts.join("\n\n"));
                        }
                        current_user_text.clear();
                        current_assistant_parts.clear();
                    }
                    // 提取 user 消息的 text
                    current_user_text = msg
                        .parts
                        .iter()
                        .filter_map(|p| p.text.as_deref())
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n");
                    has_user = true;
                }
                "assistant" => {
                    // 将 assistant 消息的所有 part 格式化后加入当前 turn
                    let formatted = format_v1_assistant_parts(&msg.parts);
                    if !formatted.is_empty() {
                        current_assistant_parts.push(formatted);
                    }
                }
                "system" => {
                    // system 消息跳过（不归档系统指令）
                }
                _ => {
                    // 其他 role 当 user 处理
                    if has_user {
                        flush_turn(
                            &current_user_text,
                            &current_assistant_parts,
                            &mut output_parts,
                            &mut turn_count,
                        );
                        if turn_count >= max_turns {
                            output_parts.push(
                                "[... truncated by sidecar max_turns ...]".to_string(),
                            );
                            return Ok(output_parts.join("\n\n"));
                        }
                        current_user_text.clear();
                        current_assistant_parts.clear();
                    }
                    current_user_text = msg
                        .parts
                        .iter()
                        .filter_map(|p| p.text.as_deref())
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n");
                    has_user = true;
                }
            }
        }

        // flush 最后一个 turn
        flush_turn(
            &current_user_text,
            &current_assistant_parts,
            &mut output_parts,
            &mut turn_count,
        );

        Ok(output_parts.join("\n\n"))
    }

    /// V1 版本：从 message + part 表读取 turns（v2.43 新增）
    ///
    /// 与 `read_session_context_between_v1` 相同的查询范围和跳过规则，
    /// 但返回 `Vec<SidecarTurn>`，保留 tool_calls/thinking 结构化字段。
    fn read_session_turns_between_v1(
        &self,
        session_id: &str,
        from_seq: Option<i64>,
        to_seq: i64,
        max_turns: usize,
    ) -> Result<Vec<SidecarTurn>, DbError> {
        let from_seq_val = from_seq.unwrap_or(0);

        let mut stmt = match self.conn.prepare(
            "SELECT id, data, time_created FROM message
             WHERE session_id = ?1
               AND time_created > ?2
               AND time_created < ?3
             ORDER BY time_created ASC, id ASC",
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "V1 message 表查询失败（turns 路径）");
                return Ok(Vec::new());
            }
        };

        let rows = stmt.query_map(rusqlite::params![session_id, from_seq_val, to_seq], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;

        // 先收集所有消息的结构化内容
        #[derive(Debug)]
        struct MsgEntry {
            role: String,
            parts: Vec<V1PartContent>,
        }

        let mut messages: Vec<MsgEntry> = Vec::new();
        for row in rows {
            let (msg_id, msg_data_json, _time_created) = row?;
            let msg_data: serde_json::Value =
                serde_json::from_str(&msg_data_json).unwrap_or_default();

            // 跳过压缩产物消息
            let mode = msg_data.get("mode").and_then(|v| v.as_str()).unwrap_or("");
            if mode == "compaction" {
                continue;
            }

            let parts = self.read_v1_parts_structured(&msg_id)?;
            if parts.is_empty() {
                continue;
            }

            let role = msg_data
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            // 跳过 restore checkpoint
            if role == "user" {
                let first_text = parts.iter().find_map(|p| p.text.as_deref()).unwrap_or("");
                if first_text.starts_with("[restore checkpointed") {
                    continue;
                }
            }

            messages.push(MsgEntry { role, parts });
        }

        if messages.is_empty() {
            return Ok(Vec::new());
        }

        // 按 turn 分组（v2.44：同时追踪 tokens，v2.45：追踪 stop_reason/cost）
        let mut turns: Vec<SidecarTurn> = Vec::new();
        let mut current_user: Option<SidecarContent> = None;
        let mut current_assistant_parts: Vec<SidecarContent> = Vec::new();
        let mut current_turn_tokens: usize = 0;
        let mut has_tokens = false;
        let mut current_stop_reason: Option<String> = None;
        let mut current_cost: Option<f64> = None;

        let flush_turn = |user: &Option<SidecarContent>,
                          assistant_parts: &[SidecarContent],
                          turn_tokens: usize,
                          has_tokens: bool,
                          turn_stop_reason: &Option<String>,
                          turn_cost: &Option<f64>,
                          turns: &mut Vec<SidecarTurn>| {
            if user.is_none() && assistant_parts.is_empty() {
                return;
            }
            let user_content = user.clone().unwrap_or_else(|| SidecarContent {
                text: None,
                thinking: None,
                tool_calls: Vec::new(),
                file_changes: Vec::new(),
            });
            let llm_content = merge_sidecar_contents(assistant_parts);
            turns.push(SidecarTurn {
                user_message: user_content,
                llm_message: llm_content,
                token_count: if has_tokens { Some(turn_tokens) } else { None },
                stop_reason: turn_stop_reason.clone(),
                cost: *turn_cost,
            });
        };

        for msg in &messages {
            match msg.role.as_str() {
                "user" => {
                    if current_user.is_some() || !current_assistant_parts.is_empty() {
                        flush_turn(&current_user, &current_assistant_parts, current_turn_tokens, has_tokens, &current_stop_reason, &current_cost, &mut turns);
                        if turns.len() >= max_turns {
                            return Ok(turns);
                        }
                        current_user = None;
                        current_assistant_parts.clear();
                        current_turn_tokens = 0;
                        has_tokens = false;
                        current_stop_reason = None;
                        current_cost = None;
                    }
                    // user 消息只提取 text
                    let user_text: String = msg
                        .parts
                        .iter()
                        .filter(|p| p.part_type == "text")
                        .filter_map(|p| p.text.as_deref())
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n");
                    current_user = Some(SidecarContent::text_only(user_text));
                }
                "assistant" => {
                    // assistant 消息提取 text + thinking + tool_calls + file_changes + tokens + stop_reason + cost
                    let (content, tokens, stop_reason, cost) = v1_parts_to_assistant_content(&msg.parts);
                    if let Some(t) = tokens {
                        current_turn_tokens += t;
                        has_tokens = true;
                    }
                    // v2.45：追踪 stop_reason 和 cost（取最后一个非 None 值）
                    if let Some(r) = stop_reason {
                        current_stop_reason = Some(r);
                    }
                    if let Some(c) = cost {
                        current_cost = Some(c);
                    }
                    current_assistant_parts.push(content);
                }
                "system" => {
                    // 跳过
                }
                _ => {
                    // 未知角色当 user 处理
                    if current_user.is_some() || !current_assistant_parts.is_empty() {
                        flush_turn(&current_user, &current_assistant_parts, current_turn_tokens, has_tokens, &current_stop_reason, &current_cost, &mut turns);
                        if turns.len() >= max_turns {
                            return Ok(turns);
                        }
                        current_user = None;
                        current_assistant_parts.clear();
                        current_turn_tokens = 0;
                        has_tokens = false;
                        current_stop_reason = None;
                        current_cost = None;
                    }
                    let user_text: String = msg
                        .parts
                        .iter()
                        .filter(|p| p.part_type == "text")
                        .filter_map(|p| p.text.as_deref())
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n");
                    current_user = Some(SidecarContent::text_only(user_text));
                }
            }
        }

        flush_turn(&current_user, &current_assistant_parts, current_turn_tokens, has_tokens, &current_stop_reason, &current_cost, &mut turns);
        Ok(turns)
    }

    /// 读取 session 的完整消息并序列化为 full_context 字符串
    ///
    /// 优先从 V2 `session_message` 表读取，回退到 V1 `message`+`part` 表。
    /// 输出格式：`User: ...\n\nAssistant: ...\n\nUser: ...\n...`
    /// （MemoryCenter context_parser 支持 `User:`/`Assistant:` 分隔符格式）
    pub fn read_session_context(
        &self,
        session_id: &str,
        max_turns: usize,
    ) -> Result<String, DbError> {
        // 优先 V2 session_message 表
        let v2_context = self.read_v2_messages(session_id, max_turns)?;
        if !v2_context.is_empty() {
            return Ok(v2_context);
        }

        // 回退 V1 message + part 表
        let v1_context = self.read_v1_messages(session_id, max_turns)?;
        Ok(v1_context)
    }

    /// 从 V2 `session_message` 表读取消息
    ///
    /// 消息类型：user / assistant / system / synthetic / shell / compaction
    /// 按 seq 排序，跳过 compaction 类型（压缩产物不归档）。
    fn read_v2_messages(
        &self,
        session_id: &str,
        max_turns: usize,
    ) -> Result<String, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT type, data FROM session_message
             WHERE session_id = ?1
             ORDER BY seq ASC",
        )?;

        let rows = stmt.query_map([session_id], |row| {
            Ok((
                row.get::<_, String>(0)?, // type
                row.get::<_, String>(1)?, // data (JSON)
            ))
        })?;

        let mut parts = Vec::new();
        let mut turn_count = 0usize;

        for row in rows {
            let (msg_type, data_json) = row?;
            let data: serde_json::Value = serde_json::from_str(&data_json).unwrap_or_default();

            let serialized = serialize_v2_message(&msg_type, &data);
            if serialized.is_empty() {
                continue;
            }

            // 跳过压缩产物（不归档压缩摘要本身）
            if msg_type == "compaction" {
                continue;
            }

            parts.push(serialized);
            turn_count += 1;

            if turn_count >= max_turns {
                parts.push("[... truncated by sidecar max_turns ...]".to_string());
                break;
            }
        }

        Ok(parts.join("\n\n"))
    }

    /// 从 V1 `message` + `part` 表读取消息
    ///
    /// V1 结构：message 表存消息元数据，part 表存消息内容片段。
    fn read_v1_messages(
        &self,
        session_id: &str,
        max_turns: usize,
    ) -> Result<String, DbError> {
        // 查询 message 表
        let mut stmt = self.conn.prepare(
            "SELECT id, data FROM message
             WHERE session_id = ?1
             ORDER BY time_created ASC, id ASC",
        )?;

        let message_ids: Vec<(String, String)> = stmt
            .query_map([session_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                ))
            })?
            .filter_map(|r| r.ok())
            .collect();

        if message_ids.is_empty() {
            return Ok(String::new());
        }

        let mut parts = Vec::new();
        let mut turn_count = 0usize;

        for (msg_id, msg_data_json) in message_ids {
            let msg_data: serde_json::Value =
                serde_json::from_str(&msg_data_json).unwrap_or_default();

            // 查询该消息的 parts
            let part_texts = self.read_v1_parts(&msg_id)?;
            if part_texts.is_empty() {
                continue;
            }

            // V1 message 的 role 通常在 data.role 中
            let role = msg_data
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");

            let role_label = match role {
                "user" => "User",
                "assistant" => "Assistant",
                "system" => "System",
                _ => "User", // 默认按用户处理
            };

            for text in part_texts {
                if !text.is_empty() {
                    parts.push(format!("{}: {}", role_label, text));
                    turn_count += 1;
                    if turn_count >= max_turns {
                        parts.push("[... truncated by sidecar max_turns ...]".to_string());
                        return Ok(parts.join("\n\n"));
                    }
                }
            }
        }

        Ok(parts.join("\n\n"))
    }

    /// 读取 V1 part 表中某消息的所有片段
    fn read_v1_parts(&self, message_id: &str) -> Result<Vec<String>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT data FROM part
             WHERE message_id = ?1
             ORDER BY id ASC",
        )?;

        let rows = stmt.query_map([message_id], |row| {
            row.get::<_, String>(0)
        })?;

        let mut parts = Vec::new();
        for row in rows {
            let data_json = row?;
            let data: serde_json::Value = serde_json::from_str(&data_json).unwrap_or_default();
            if let Some(text) = data.get("text").and_then(|v| v.as_str()) {
                if !text.is_empty() {
                    parts.push(text.to_string());
                }
            }
        }
        Ok(parts)
    }

    /// 读取 V1 part 表的所有 part（结构化，v2.42 新增）
    ///
    /// 与 `read_v1_parts` 的区别：返回所有 part 类型（text/reasoning/tool/step-finish），
    /// 保留完整的工具调用输入输出、思考过程、token 统计等信息。
    ///
    /// part.data JSON 结构（按 type 不同）：
    /// - `{"type":"text","text":"..."}` — 文本回复
    /// - `{"type":"reasoning","text":"..."}` — Agent 思考过程
    /// - `{"type":"tool","tool":"bash","callID":"...","state":{"status":"completed","input":{...},"output":"..."}}` — 工具调用
    /// - `{"type":"step-start"}` — 步骤开始（无内容）
    /// - `{"type":"step-finish","reason":"stop","tokens":{"total":88584,...},"cost":0}` — 步骤结束（含 token 统计）
    fn read_v1_parts_structured(&self, message_id: &str) -> Result<Vec<V1PartContent>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT data FROM part
             WHERE message_id = ?1
             ORDER BY id ASC",
        )?;

        let rows = stmt.query_map([message_id], |row| {
            row.get::<_, String>(0)
        })?;

        let mut parts = Vec::new();
        for row in rows {
            let data_json = row?;
            let data: serde_json::Value =
                serde_json::from_str(&data_json).unwrap_or_default();

            let part_type = data
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let text = data
                .get("text")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            // v2.45：tool part 提取完整字段（status/error/call_id/time/input/output）
            let (tool_name, tool_input, tool_output, tool_status, tool_error, tool_call_id, tool_time_start, tool_time_end) =
                if part_type == "tool" {
                    let name = data.get("tool").and_then(|v| v.as_str()).map(|s| s.to_string());
                    let state = data.get("state");

                    let input = state
                        .and_then(|s| s.get("input"))
                        .map(|v| v.to_string());

                    let output = state
                        .and_then(|s| s.get("output"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    // v2.45 新增：提取工具执行状态
                    let status = state
                        .and_then(|s| s.get("status"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    // v2.45 新增：提取错误信息
                    // state.error 可能是字符串或对象（{message: "..."}）
                    let error = state.and_then(|s| {
                        let err = s.get("error")?;
                        if let Some(msg) = err.as_str() {
                            Some(msg.to_string())
                        } else if let Some(msg) = err.get("message").and_then(|v| v.as_str()) {
                            Some(msg.to_string())
                        } else {
                            Some(err.to_string())
                        }
                    });

                    // v2.45 新增：提取 callID
                    let call_id = data
                        .get("callID")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    // v2.45 新增：提取执行时间
                    let time_start = state
                        .and_then(|s| s.get("time"))
                        .and_then(|t| t.get("start"))
                        .and_then(|v| v.as_i64());
                    let time_end = state
                        .and_then(|s| s.get("time"))
                        .and_then(|t| t.get("end"))
                        .and_then(|v| v.as_i64());

                    (name, input, output, status, error, call_id, time_start, time_end)
                } else {
                    (None, None, None, None, None, None, None, None)
                };

            // v2.45：patch part 提取 hash 和 files
            let (patch_hash, patch_files) = if part_type == "patch" {
                let hash = data
                    .get("hash")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let files = data
                    .get("files")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                (hash, files)
            } else {
                (None, Vec::new())
            };

            // step-finish 类型提取 token 统计 + v2.45 新增 reason/cost
            let (tokens_total, tokens_consumed, step_finish_reason, step_finish_cost) =
                if part_type == "step-finish" {
                    let tokens = data.get("tokens");
                    let total = tokens
                        .and_then(|t| t.get("total"))
                        .and_then(|v| v.as_i64());
                    // v2.44：单轮实际消耗 = input + output + reasoning
                    let consumed = tokens.and_then(|t| {
                        let input = t.get("input").and_then(|v| v.as_i64()).unwrap_or(0);
                        let output = t.get("output").and_then(|v| v.as_i64()).unwrap_or(0);
                        let reasoning = t.get("reasoning").and_then(|v| v.as_i64()).unwrap_or(0);
                        Some((input + output + reasoning) as usize)
                    });
                    // v2.45 新增：停止原因
                    let reason = data
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    // v2.45 新增：单轮成本
                    let cost = data.get("cost").and_then(|v| v.as_f64());
                    (total, consumed, reason, cost)
                } else {
                    (None, None, None, None)
                };

            parts.push(V1PartContent {
                part_type,
                text,
                tool_name,
                tool_input,
                tool_output,
                tokens_total,
                tokens_consumed,
                tool_status,
                tool_error,
                tool_call_id,
                tool_time_start,
                tool_time_end,
                patch_hash,
                patch_files,
                step_finish_reason,
                step_finish_cost,
            });
        }
        Ok(parts)
    }

    /// 获取已归档过的 session 集合（v2.39 新增，v2.40 加 V1 回退）
    ///
    /// 用于 backfill 模式：启动时找出所有曾经压缩过的 session。
    ///
    /// **V2 路径**：`SELECT DISTINCT session_id FROM session_message WHERE type='compaction'`
    /// **V1 路径**（桌面端）：`SELECT DISTINCT session_id FROM message WHERE data LIKE '%"mode":"compaction"%'`
    pub fn query_ever_compacted_sessions(&self) -> Result<HashSet<String>, DbError> {
        // 先试 V2 session_message 表
        if let Ok(mut stmt) = self.conn.prepare(
            "SELECT DISTINCT session_id FROM session_message WHERE type = 'compaction'",
        ) {
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            let mut result = HashSet::new();
            for row in rows {
                if let Ok(sid) = row {
                    result.insert(sid);
                }
            }
            if !result.is_empty() {
                return Ok(result);
            }
            tracing::debug!("V2 session_message 表无 compaction 数据，回退 V1 message 表");
        }

        // 回退 V1 message 表（桌面端）
        let mut stmt = match self.conn.prepare(
            "SELECT DISTINCT session_id FROM message
             WHERE data LIKE '%\"mode\":\"compaction\"%'
               AND data LIKE '%\"summary\":true%'",
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "V1 message 表查询失败");
                return Ok(HashSet::new());
            }
        };

        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut result = HashSet::new();
        for row in rows {
            if let Ok(sid) = row {
                result.insert(sid);
            }
        }
        Ok(result)
    }
}

/// 序列化 V2 消息为 `Role: text` 格式
///
/// 对应 OpenCode compaction.ts 中的 `serialize()` 函数（简化版）。
fn serialize_v2_message(msg_type: &str, data: &serde_json::Value) -> String {
    match msg_type {
        "user" => {
            let text = data.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if text.is_empty() {
                return String::new();
            }
            format!("User: {}", text)
        }
        "assistant" => {
            // assistant 消息的 content 是数组，每个 part 有 type
            if let Some(content) = data.get("content").and_then(|v| v.as_array()) {
                let mut parts = Vec::new();
                for part in content {
                    let part_type = part.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match part_type {
                        "text" => {
                            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                if !text.is_empty() {
                                    parts.push(text.to_string());
                                }
                            }
                        }
                        "reasoning" => {
                            // 跳过推理过程（不归档 thinking）
                        }
                        "tool" => {
                            // 工具调用：提取名称和结果
                            let name = part.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
                            if let Some(state) = part.get("state") {
                                let status = state.get("status").and_then(|v| v.as_str()).unwrap_or("");
                                if status == "completed" {
                                    if let Some(content_arr) = state.get("content").and_then(|v| v.as_array()) {
                                        let result_text: Vec<String> = content_arr.iter()
                                            .filter_map(|c| c.get("text").and_then(|v| v.as_str()).map(String::from))
                                            .collect();
                                        if !result_text.is_empty() {
                                            let truncated = truncate_str(&result_text.join("\n"), 2000);
                                            parts.push(format!("[Tool: {}] {}", name, truncated));
                                        }
                                    }
                                } else if status == "error" {
                                    if let Some(err) = state.get("error").and_then(|v| v.get("message")).and_then(|v| v.as_str()) {
                                        parts.push(format!("[Tool: {} ERROR] {}", name, err));
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if parts.is_empty() {
                    return String::new();
                }
                format!("Assistant: {}", parts.join("\n"))
            } else {
                String::new()
            }
        }
        "system" => {
            let text = data.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if text.is_empty() {
                return String::new();
            }
            format!("System: {}", text)
        }
        "synthetic" | "shell" => {
            // 跳过 synthetic 和 shell 消息（不归档）
            String::new()
        }
        "compaction" => {
            // 压缩产物不归档（跳过）
            String::new()
        }
        _ => String::new(),
    }
}

/// 截断字符串到指定字符数
fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}\n[truncated]", truncated)
    }
}

/// 格式化 V1 assistant 消息的 parts 为完整内容字符串（v2.42 新增）
///
/// 将不同类型的 part 按顺序格式化，保留完整信息：
/// - `text` → 直接输出文本
/// - `reasoning` → `<thinking>...</thinking>` 标记
/// - `tool` → `<tool_call name="bash">输入 → 输出</tool_call>` 标记
/// - `step-start` / `step-finish` → 跳过（无内容或仅 token 统计）
///
/// 生成的字符串会被拼接到 `Assistant: ` 后面，作为 full_context 的一部分。
/// context_parser 的 `parse_separators` 会将其作为 llm_message.text 保留。
fn format_v1_assistant_parts(parts: &[V1PartContent]) -> String {
    let mut segments = Vec::new();

    for part in parts {
        match part.part_type.as_str() {
            "text" => {
                if let Some(text) = &part.text {
                    if !text.is_empty() {
                        segments.push(text.clone());
                    }
                }
            }
            "reasoning" => {
                if let Some(text) = &part.text {
                    if !text.is_empty() {
                        // 截断过长的 reasoning（防止上下文膨胀，保留前 2000 字符）
                        let truncated = truncate_str(text, 2000);
                        segments.push(format!("<thinking>\n{}\n</thinking>", truncated));
                    }
                }
            }
            "tool" => {
                let name = part.tool_name.as_deref().unwrap_or("unknown");
                let input = part.tool_input.as_deref().unwrap_or("");
                let output = part.tool_output.as_deref().unwrap_or("");

                // 截断过长的工具输出（保留前 3000 字符，防止上下文膨胀）
                let input_display = if !input.is_empty() {
                    truncate_str(input, 1000)
                } else {
                    String::new()
                };
                let output_display = if !output.is_empty() {
                    truncate_str(output, 3000)
                } else {
                    String::new()
                };

                let mut tool_segment = format!("<tool_call name=\"{}\">", name);
                if !input_display.is_empty() {
                    tool_segment.push_str(&format!("\n输入: {}", input_display));
                }
                if !output_display.is_empty() {
                    tool_segment.push_str(&format!("\n输出: {}", output_display));
                }
                tool_segment.push_str("\n</tool_call>");
                segments.push(tool_segment);
            }
            "step-start" => {
                // 跳过（无内容）
            }
            "step-finish" => {
                // 跳过（token 统计不放入 full_context，后续可从 part 单独提取）
                // 如需保留可添加: <token_stats total="88584" />
            }
            _ => {
                // 未知 part 类型，尝试提取 text
                if let Some(text) = &part.text {
                    if !text.is_empty() {
                        segments.push(text.clone());
                    }
                }
            }
        }
    }

    segments.join("\n\n")
}

/// V2 消息解析为 SidecarContent（v2.43 新增，v2.45 完整提取）
///
/// V2 的 assistant 消息 data.content 是 part 数组，每个 part 有 type：
/// - text → 加入 text 字段
/// - reasoning → 加入 thinking 字段（v2.45：移除截断，保留完整）
/// - tool → 转换为 SidecarToolCall（v2.45：含 status/error/call_id/duration_ms）
/// - step-finish → 提取 tokens + v2.45 新增 reason/cost
///
/// v2.45 返回 4-tuple：(SidecarContent, Option<usize>, Option<String>, Option<f64>)
fn parse_v2_message_to_content(
    msg_type: &str,
    data: &serde_json::Value,
) -> (SidecarContent, Option<usize>, Option<String>, Option<f64>) {
    match msg_type {
        "user" => {
            let text = data.get("text").and_then(|v| v.as_str()).unwrap_or("");
            (SidecarContent::text_only(text.to_string()), None, None, None)
        }
        "assistant" => {
            let mut text_parts: Vec<String> = Vec::new();
            let mut thinking_parts: Vec<String> = Vec::new();
            let mut tool_calls: Vec<SidecarToolCall> = Vec::new();
            let mut tokens_consumed: Option<usize> = None;
            let mut stop_reason: Option<String> = None;
            let mut cost: Option<f64> = None;

            if let Some(content) = data.get("content").and_then(|v| v.as_array()) {
                for part in content {
                    let part_type = part.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match part_type {
                        "text" => {
                            if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                                if !t.is_empty() {
                                    text_parts.push(t.to_string());
                                }
                            }
                        }
                        "reasoning" => {
                            if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                                if !t.is_empty() {
                                    // v2.45：移除截断，保留完整推理过程
                                    thinking_parts.push(t.to_string());
                                }
                            }
                        }
                        "tool" => {
                            let name = part
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown")
                                .to_string();
                            // v2.45：提取完整工具信息
                            let (arguments, result, status, error, duration_ms) =
                                extract_v2_tool_state(part);
                            let call_id = part
                                .get("id")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            tool_calls.push(SidecarToolCall {
                                name,
                                arguments,
                                result,
                                status,
                                error,
                                call_id,
                                duration_ms,
                            });
                        }
                        "step-finish" => {
                            // v2.44：提取单轮实际 token 消耗 = input + output + reasoning
                            if let Some(tokens) = part.get("tokens") {
                                let input = tokens.get("input").and_then(|v| v.as_i64()).unwrap_or(0);
                                let output = tokens.get("output").and_then(|v| v.as_i64()).unwrap_or(0);
                                let reasoning = tokens.get("reasoning").and_then(|v| v.as_i64()).unwrap_or(0);
                                tokens_consumed = Some((input + output + reasoning) as usize);
                            }
                            // v2.45：提取停止原因和成本
                            if let Some(reason) = part.get("reason").and_then(|v| v.as_str()) {
                                stop_reason = Some(reason.to_string());
                            }
                            if let Some(c) = part.get("cost").and_then(|v| v.as_f64()) {
                                cost = Some(c);
                            }
                        }
                        _ => {}
                    }
                }
            }

            let text = if text_parts.is_empty() {
                None
            } else {
                Some(text_parts.join("\n"))
            };
            let thinking = if thinking_parts.is_empty() {
                None
            } else {
                Some(thinking_parts.join("\n\n"))
            };

            (
                SidecarContent {
                    text,
                    thinking,
                    tool_calls,
                    file_changes: Vec::new(), // V2 路径暂不提取 file_changes
                },
                tokens_consumed,
                stop_reason,
                cost,
            )
        }
        // system/synthetic/shell/compaction 返回空 content（调用方会跳过）
        _ => (
            SidecarContent {
                text: None,
                thinking: None,
                tool_calls: Vec::new(),
                file_changes: Vec::new(),
            },
            None,
            None,
            None,
        ),
    }
}

/// 从 V2 tool part 提取完整状态（v2.43 新增，v2.45 扩展为 5-tuple）
///
/// V2 tool part 结构：`{ "type": "tool", "name": "bash", "state": { "status": "completed", "input": {...}, "content": [...], "time": { "start": ms, "end": ms }, "error": { "message": "..." } } }`
/// - arguments：state.input 序列化为 JSON 字符串（v2.45：完整保留，不截断）
/// - result：state.content 数组中所有 text 拼接（v2.45：完整保留，不截断）
/// - status：state.status（completed/error/running/pending）
/// - error：status="error" 时的 error.message
/// - duration_ms：state.time.end - state.time.start
///
/// v2.45 通用性：返回 5-tuple，不同 Agent adapter 按能力填充，缺失为 None。
fn extract_v2_tool_state(
    part: &serde_json::Value,
) -> (String, String, Option<String>, Option<String>, Option<u64>) {
    let mut arguments = String::new();
    let mut result = String::new();
    let mut status: Option<String> = None;
    let mut error: Option<String> = None;
    let mut duration_ms: Option<u64> = None;

    if let Some(state) = part.get("state") {
        // 提取状态字符串（用于分支判断和返回）
        let status_str = state
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !status_str.is_empty() {
            status = Some(status_str.to_string());
        }

        // input：完整保留（v2.45 移除截断）
        if let Some(input) = state.get("input") {
            arguments = serde_json::to_string(input).unwrap_or_default();
        }

        // output：按 status 分支提取
        if status_str == "completed" {
            if let Some(content_arr) = state.get("content").and_then(|v| v.as_array()) {
                let texts: Vec<String> = content_arr
                    .iter()
                    .filter_map(|c| c.get("text").and_then(|v| v.as_str()).map(String::from))
                    .collect();
                if !texts.is_empty() {
                    // v2.45：移除 3000 字符截断，保留完整工具输出
                    result = texts.join("\n");
                }
            }
        } else if status_str == "error" {
            if let Some(err) = state
                .get("error")
                .and_then(|v| v.get("message"))
                .and_then(|v| v.as_str())
            {
                result = format!("[ERROR] {}", err);
                error = Some(err.to_string());
            }
        }
        // running/pending 状态无 output，result 保持空字符串

        // v2.45：提取耗时（time.end - time.start，毫秒）
        if let Some(time) = state.get("time") {
            let t_start = time.get("start").and_then(|v| v.as_i64());
            let t_end = time.get("end").and_then(|v| v.as_i64());
            if let (Some(start), Some(end)) = (t_start, t_end) {
                if end >= start {
                    duration_ms = Some((end - start) as u64);
                }
            }
        }
    }

    (arguments, result, status, error, duration_ms)
}

/// 合并多个 SidecarContent 为一个（v2.43 新增，v2.45 合并 file_changes）
///
/// 用于把一个 turn 内的多个 assistant 消息合并为单个 llm_message：
/// - text 用 `\n\n` 连接
/// - thinking 用 `\n\n` 连接
/// - tool_calls 直接 extend
/// - file_changes 直接 extend（v2.45 新增）
fn merge_sidecar_contents(contents: &[SidecarContent]) -> SidecarContent {
    if contents.is_empty() {
        return SidecarContent {
            text: None,
            thinking: None,
            tool_calls: Vec::new(),
            file_changes: Vec::new(),
        };
    }
    if contents.len() == 1 {
        return contents[0].clone();
    }

    let mut text_parts: Vec<String> = Vec::new();
    let mut thinking_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<SidecarToolCall> = Vec::new();
    let mut file_changes: Vec<crate::archive::SidecarFileChange> = Vec::new();

    for c in contents {
        if let Some(t) = &c.text {
            if !t.is_empty() {
                text_parts.push(t.clone());
            }
        }
        if let Some(t) = &c.thinking {
            if !t.is_empty() {
                thinking_parts.push(t.clone());
            }
        }
        tool_calls.extend(c.tool_calls.iter().cloned());
        file_changes.extend(c.file_changes.iter().cloned());
    }

    SidecarContent {
        text: if text_parts.is_empty() {
            None
        } else {
            Some(text_parts.join("\n\n"))
        },
        thinking: if thinking_parts.is_empty() {
            None
        } else {
            Some(thinking_parts.join("\n\n"))
        },
        tool_calls,
        file_changes,
    }
}

/// V1 parts 转换为 assistant 的 SidecarContent（v2.43 新增，v2.44 返回 tokens，v2.45 完整提取）
///
/// v2.45 改动：
/// - 移除所有截断（arguments/result/thinking 均保留完整内容）
/// - 填充 tool 的 status/error/call_id/duration_ms
/// - 处理 patch part → file_changes
/// - 提取 step-finish 的 reason/cost
///
/// 返回 `(SidecarContent, Option<usize>, Option<String>, Option<f64>)`：
/// - 第一个：SidecarContent（含 text/thinking/tool_calls/file_changes）
/// - 第二个：单轮 token 消耗
/// - 第三个：LLM 停止原因
/// - 第四个：单轮成本
fn v1_parts_to_assistant_content(
    parts: &[V1PartContent],
) -> (SidecarContent, Option<usize>, Option<String>, Option<f64>) {
    let mut text_parts: Vec<String> = Vec::new();
    let mut thinking_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<SidecarToolCall> = Vec::new();
    let mut file_changes: Vec<SidecarFileChange> = Vec::new();
    let mut tokens_consumed: Option<usize> = None;
    let mut stop_reason: Option<String> = None;
    let mut cost: Option<f64> = None;

    for part in parts {
        match part.part_type.as_str() {
            "text" => {
                if let Some(text) = &part.text {
                    if !text.is_empty() {
                        text_parts.push(text.clone());
                    }
                }
            }
            "reasoning" => {
                if let Some(text) = &part.text {
                    if !text.is_empty() {
                        // v2.45：移除截断，保留完整推理过程
                        thinking_parts.push(text.clone());
                    }
                }
            }
            "tool" => {
                let name = part.tool_name.clone().unwrap_or_else(|| "unknown".to_string());
                // v2.45：移除截断，保留完整参数和结果
                let arguments = part.tool_input.clone().unwrap_or_default();
                let result = part.tool_output.clone().unwrap_or_default();

                // v2.45：计算耗时（毫秒）
                let duration_ms = match (part.tool_time_start, part.tool_time_end) {
                    (Some(start), Some(end)) => Some((end - start) as u64),
                    _ => None,
                };

                tool_calls.push(SidecarToolCall {
                    name,
                    arguments,
                    result,
                    status: part.tool_status.clone(),
                    error: part.tool_error.clone(),
                    call_id: part.tool_call_id.clone(),
                    duration_ms,
                });
            }
            "patch" => {
                // v2.45：patch part → file_changes
                // patch part 只有 hash 和 files 列表，无 diff 内容
                // diff 内容在 user 消息的 summary.diffs 中（由 read_session_turns_between_v1 单独提取）
                for file_path in &part.patch_files {
                    file_changes.push(SidecarFileChange {
                        file_path: file_path.clone(),
                        status: None,
                        additions: None,
                        deletions: None,
                        patch: None,
                        hash: part.patch_hash.clone(),
                    });
                }
            }
            "step-finish" => {
                // v2.44：提取单轮实际 token 消耗
                if let Some(consumed) = part.tokens_consumed {
                    tokens_consumed = Some(consumed);
                }
                // v2.45：提取停止原因和成本
                if let Some(reason) = &part.step_finish_reason {
                    stop_reason = Some(reason.clone());
                }
                if let Some(c) = part.step_finish_cost {
                    cost = Some(c);
                }
            }
            "step-start" => {
                // 跳过
            }
            _ => {
                // 未知 part 类型，尝试提取 text
                if let Some(text) = &part.text {
                    if !text.is_empty() {
                        text_parts.push(text.clone());
                    }
                }
            }
        }
    }

    let content = SidecarContent {
        text: if text_parts.is_empty() {
            None
        } else {
            Some(text_parts.join("\n\n"))
        },
        thinking: if thinking_parts.is_empty() {
            None
        } else {
            Some(thinking_parts.join("\n\n"))
        },
        tool_calls,
        file_changes,
    };
    (content, tokens_consumed, stop_reason, cost)
}

/// 数据库错误
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("SQLite 错误: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("JSON 解析错误: {0}")]
    Json(#[from] serde_json::Error),
}
