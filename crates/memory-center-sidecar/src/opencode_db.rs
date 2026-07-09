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

/// OpenCode SQLite 读取器
pub struct OpenCodeDb {
    conn: Connection,
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

    /// V1 版本：从 message + part 表读取 (from_seq, to_seq) 范围内的消息（桌面端，v2.40 新增）
    ///
    /// V1 无 seq 字段，用 `time_created`（毫秒时间戳）代替。
    /// 由于 `query_all_compactions_v1` 已将 `time_created` 存入 `CompactionRecord.seq`，
    /// 所以 `(from_seq, to_seq)` 实际上是 `time_created` 范围，语义一致。
    ///
    /// 跳过 `data.mode = 'compaction'` 的压缩产物消息。
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
            ))
        })?;

        let mut parts = Vec::new();
        let mut turn_count = 0usize;

        for row in rows {
            let (msg_id, msg_data_json) = row?;
            let msg_data: serde_json::Value =
                serde_json::from_str(&msg_data_json).unwrap_or_default();

            // 跳过压缩产物消息（mode=compaction）
            let mode = msg_data.get("mode").and_then(|v| v.as_str()).unwrap_or("");
            if mode == "compaction" {
                continue;
            }

            // 从 part 表提取消息内容
            let part_texts = self.read_v1_parts(&msg_id)?;
            if part_texts.is_empty() {
                continue;
            }

            // V1 message 的 role 在 data.role 中
            let role = msg_data
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let role_label = match role {
                "user" => "User",
                "assistant" => "Assistant",
                "system" => "System",
                _ => "User",
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

/// 数据库错误
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("SQLite 错误: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("JSON 解析错误: {0}")]
    Json(#[from] serde_json::Error),
}
