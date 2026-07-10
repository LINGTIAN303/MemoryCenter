//! # OpenCode DB 写入器（v2.47 新增）
//!
//! 向 OpenCode SQLite 数据库插入 compaction 消息对，实现"主动清空"。
//!
//! ## 设计背景
//!
//! OpenCode 的 compaction 机制采用"标记 + 跳过"：
//! - 压缩完成后插入两条消息（user 触发 + assistant 摘要）
//! - 加载 session 时 `completedCompactions()` 识别这对消息，
//!   将它们之前的所有消息加入 `hidden` 集合（不发给 LLM）
//!
//! sidecar 主动归档后，模仿 OpenCode 的 compaction 消息对结构插入两条消息，
//! 让 OpenCode 下次加载时自动跳过旧消息（无感清空），跳过 LLM 压缩步骤。
//!
//! ## 消息对结构（V1 message + part 表）
//!
//! ### 消息 A：user 触发消息
//!
//! `message.data`:
//! ```json
//! { "role": "user", "time": { "created": <timestamp> } }
//! ```
//!
//! `part.data`:
//! ```json
//! { "type": "compaction", "auto": false, "tail_start_id": "<msg_id>" }
//! ```
//!
//! ### 消息 B：assistant 摘要消息
//!
//! `message.data`:
//! ```json
//! {
//!   "role": "assistant",
//!   "mode": "compaction",
//!   "agent": "compaction",
//!   "summary": true,
//!   "cost": 0,
//!   "tokens": { "input": 0, "output": 0, "reasoning": 0, "cache": { "read": 0, "write": 0 } },
//!   "time": { "created": <ts>, "completed": <ts> },
//!   "finish": "stop"
//! }
//! ```
//!
//! `part.data`:
//! ```json
//! { "type": "text", "text": "<memory-center 归档摘要>" }
//! ```
//!
//! ## 与 OpenCode 原生 compaction 的区别
//!
//! | 方面 | OpenCode 原生 | sidecar 主动清空 |
//! |------|-------------|----------------|
//! | 触发时机 | tokens 溢出后 | tokens 达到阈值 80% 时 |
//! | summary 来源 | LLM 压缩生成 | memory-center 归档摘要 |
//! | LLM 调用 | 需要（压缩摘要） | 不需要（跳过压缩） |
//! | 上下文保留 | 完整保留在 DB | 完整保留在 DB（相同） |

use rusqlite::{Connection, OpenFlags};
use std::path::Path;

/// 写入错误
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("SQLite 错误: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("JSON 序列化错误: {0}")]
    Json(#[from] serde_json::Error),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
}

/// 向 OpenCode DB 插入 compaction 消息对（v2.47 新增）
///
/// 插入两条消息（user 触发 + assistant 摘要），让 OpenCode 下次加载时
/// 通过 `completedCompactions()` 将旧消息加入 hidden 集合（无感清空）。
///
/// ## 参数
///
/// - `db_path`：OpenCode SQLite 数据库路径
/// - `session_id`：目标 session ID
/// - `summary`：归档摘要文本（替代 LLM 压缩摘要）
/// - `tail_start_id`：保留尾部的起始消息 ID（该 ID 之前的消息被跳过）
/// - `reason`：compaction 原因（如 "memory_center_proactive"）
///
/// ## 返回
///
/// 成功返回 `(user_msg_id, assistant_msg_id)`，失败返回 WriteError。
///
/// ## 安全性
///
/// - 使用独立可写连接（不影响 OpenCodeDb 的只读连接）
/// - 只插入不删除（旧消息保留在 DB 中，不破坏数据）
/// - 使用事务确保两条消息原子插入
pub fn insert_compaction_pair(
    db_path: &Path,
    session_id: &str,
    summary: &str,
    tail_start_id: &str,
    reason: &str,
) -> Result<(String, String), WriteError> {
    // 打开可写连接（与 OpenCodeDb 的只读连接分离）
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    // 设置 busy_timeout，避免与 OpenCode 主进程写入冲突时立即返回 SQLITE_BUSY
    conn.busy_timeout(std::time::Duration::from_secs(5))?;

    // 使用事务确保原子性
    let tx = conn.unchecked_transaction()?;

    let now = now_millis();
    let user_msg_id = gen_msg_id("msg");
    let assistant_msg_id = gen_msg_id("msg");
    let user_part_id = gen_msg_id("prt");
    let assistant_part_id = gen_msg_id("prt");

    // === 消息 A：user 触发消息 ===
    let user_msg_data = serde_json::json!({
        "role": "user",
        "time": { "created": now }
    });
    let user_msg_data_str = serde_json::to_string(&user_msg_data)?;

    let user_part_data = serde_json::json!({
        "type": "compaction",
        "auto": false,
        "tail_start_id": tail_start_id,
        "reason": reason
    });
    let user_part_data_str = serde_json::to_string(&user_part_data)?;

    // 插入 message A
    tx.execute(
        "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            &user_msg_id,
            session_id,
            now,
            now,
            &user_msg_data_str
        ],
    )?;

    // 插入 part A（compaction 元数据）
    tx.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            &user_part_id,
            &user_msg_id,
            session_id,
            now,
            now,
            &user_part_data_str
        ],
    )?;

    // === 消息 B：assistant 摘要消息 ===
    // 补全 OpenCode Assistant schema 必填字段 + finish 字段（风险 1+2 修复）
    // finish:"stop" 是关键 — completedCompactions() 检查 !msg.info.finish，缺失会导致消息对被跳过
    let assistant_msg_data = serde_json::json!({
        "role": "assistant",
        "parentID": &user_msg_id,
        "sessionID": session_id,
        "mode": "compaction",
        "agent": "compaction",
        "variant": "compaction",
        "summary": true,
        "finish": "stop",
        "path": { "cwd": "", "root": "" },
        "cost": 0,
        "tokens": {
            "input": 0,
            "output": 0,
            "reasoning": 0,
            "cache": { "read": 0, "write": 0 }
        },
        "modelID": "compaction",
        "providerID": "compaction",
        "time": { "created": now, "completed": now }
    });
    let assistant_msg_data_str = serde_json::to_string(&assistant_msg_data)?;

    let assistant_part_data = serde_json::json!({
        "type": "text",
        "text": summary
    });
    let assistant_part_data_str = serde_json::to_string(&assistant_part_data)?;

    // 插入 message B
    tx.execute(
        "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            &assistant_msg_id,
            session_id,
            now,
            now,
            &assistant_msg_data_str
        ],
    )?;

    // 插入 part B（摘要文本）
    tx.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            &assistant_part_id,
            &assistant_msg_id,
            session_id,
            now,
            now,
            &assistant_part_data_str
        ],
    )?;

    tx.commit()?;

    tracing::info!(
        session_id = session_id,
        user_msg_id = %user_msg_id,
        assistant_msg_id = %assistant_msg_id,
        tail_start_id = %tail_start_id,
        summary_len = summary.len(),
        "compaction 消息对插入成功"
    );

    Ok((user_msg_id, assistant_msg_id))
}

/// 查询 session 中保留尾部（tail）的起始消息 ID（v2.47 新增）
///
/// 从最新消息往前数 `tail_turns` 轮（user+assistant 为一轮），
/// 返回该轮 user 消息的 ID 作为 `tail_start_id`。
///
/// ## V1 路径（message + part 表）
///
/// 按 time_created DESC 排序，找第 `tail_turns * 2` 条 user 消息的 ID。
/// 如果消息不足，返回第一条消息的 ID。
pub fn query_tail_start_id(
    db_path: &Path,
    session_id: &str,
    tail_turns: usize,
) -> Result<String, WriteError> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;

    // 查询该 session 的所有 user 消息（按时间倒序）
    // tail_turns 轮 = tail_turns 条 user 消息（每轮一条 user）
    // 使用 json_extract 结构化查询，避免 LIKE 文本匹配的脆弱性（风险 7 修复）
    let mut stmt = conn.prepare(
        "SELECT id FROM message
         WHERE session_id = ?1 AND json_extract(data, '$.role') = 'user'
         ORDER BY time_created DESC",
    )?;

    let rows = stmt.query_map([session_id], |row| row.get::<_, String>(0))?;

    let mut user_msg_ids: Vec<String> = Vec::new();
    for row in rows {
        user_msg_ids.push(row?);
    }

    if user_msg_ids.is_empty() {
        // 无 user 消息，返回 session 中最早的消息 ID
        let earliest = conn
            .query_row(
                "SELECT id FROM message WHERE session_id = ?1 ORDER BY time_created ASC LIMIT 1",
                [session_id],
                |row| row.get::<_, String>(0),
            )
            .map_err(|e| WriteError::Sqlite(e))?;
        return Ok(earliest);
    }

    // 取倒数第 tail_turns 条 user 消息的 ID
    // 例如 tail_turns=2，则取倒数第 2 条 user 消息
    let idx = tail_turns.saturating_sub(1).min(user_msg_ids.len() - 1);
    Ok(user_msg_ids[idx].clone())
}

/// 生成当前毫秒时间戳
fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 生成 ULID 风格的消息 ID（v2.47 新增）
///
/// 格式：`<prefix>_<timestamp_hex><random>`
///
/// 与 OpenCode 的 ID 风格一致（如 `msg_f4a8216d30015KauUfu5yJHoq5`），
/// 但不保证与 OpenCode 的 ULID 算法完全一致——只需要唯一性即可。
fn gen_msg_id(prefix: &str) -> String {
    let now = now_millis();
    // 时间戳的 36 进制编码（紧凑）
    let ts_part = format!("{:x}", now as u64);

    // 随机后缀（12 位字母数字）
    let random_part: String = (0..12)
        .map(|_| {
            let c = rand_char();
            c
        })
        .collect();

    format!("{}_{}{}", prefix, ts_part, random_part)
}

/// 生成随机字母数字字符
fn rand_char() -> char {
    // 简单的伪随机（基于时间戳），不保证密码学安全
    // 用于生成唯一 ID，不需要高强度随机
    let now = now_millis() as u64;
    let chars = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    // 使用系统时间的纳秒部分作为随机源
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let idx = ((now ^ nanos.wrapping_mul(2654435761)) % chars.len() as u64) as usize;
    chars.chars().nth(idx).unwrap_or('x')
}
