//! 上下文字符串解析器(v2.34)
//!
//! 将 pre_compress_hook 接收的 full_context 字符串解析为 `Vec<MessageTurn>`。
//! 支持两种格式:
//! 1. JSON 数组(`[{"user_message": "...", "llm_message": "..."}]`)
//! 2. 分隔符识别(`User:` / `Assistant:`)
//!
//! 解析失败返回 `None`,不阻塞 pre_compress_hook(仅存 raw_context)。

use chrono::Utc;
use uuid::Uuid;

use crate::model::{MessageContent, MessageTurn, Tag};

/// 解析结果
#[derive(Debug)]
pub struct ParseResult {
    /// 解析得到的轮次列表
    pub turns: Vec<MessageTurn>,
    /// 实际使用的解析方法:"json" / "separator"
    pub method: &'static str,
}

/// 将 full_context 解析为 turns
///
/// 策略(按优先级):
/// 1. 若以 `[` 开头,尝试 JSON 数组解析
/// 2. 尝试 `User:` / `Assistant:` 分隔符识别
/// 3. 兜底返回 `None`
///
/// 空字符串与无法识别的格式均返回 `None`,调用方应仅存储 raw_context。
pub fn parse_context(full_context: &str) -> Option<ParseResult> {
    let trimmed = full_context.trim();
    if trimmed.is_empty() {
        return None;
    }

    // 策略 1: JSON 数组
    if trimmed.starts_with('[') {
        if let Some(turns) = parse_json_array(trimmed) {
            return Some(ParseResult {
                turns,
                method: "json",
            });
        }
    }

    // 策略 2: 分隔符识别
    if let Some(turns) = parse_separators(trimmed) {
        return Some(ParseResult {
            turns,
            method: "separator",
        });
    }

    None
}

/// JSON 数组解析
///
/// 期望格式:`[{"user_message": "...", "llm_message": "..."}]`
/// - 多余字段(如 id/timestamp)忽略
/// - user_message / llm_message 缺省视为空字符串
/// - 全空轮次跳过
/// - 解析失败或结果为空返回 `None`
fn parse_json_array(s: &str) -> Option<Vec<MessageTurn>> {
    #[derive(serde::Deserialize)]
    struct JsonTurn {
        #[serde(default)]
        user_message: Option<String>,
        #[serde(default)]
        llm_message: Option<String>,
    }

    let parsed: Vec<JsonTurn> = serde_json::from_str(s).ok()?;
    if parsed.is_empty() {
        return None;
    }

    let mut turns = Vec::new();
    for jt in parsed {
        let user_text = jt.user_message.unwrap_or_default();
        let llm_text = jt.llm_message.unwrap_or_default();
        if user_text.is_empty() && llm_text.is_empty() {
            continue;
        }
        turns.push(make_turn(user_text, llm_text));
    }

    if turns.is_empty() {
        None
    } else {
        Some(turns)
    }
}

/// 分隔符识别
///
/// 简单策略:按 `User:` / `Assistant:` 配对分割。
/// - 文本中无这两个标记 → 返回 `None`
/// - 后续行追加到当前所在段(user 或 assistant)
/// - 遇到新 `User:` 触发上一组收尾
/// - 末尾剩余内容作为最后一组收尾
fn parse_separators(s: &str) -> Option<Vec<MessageTurn>> {
    if !s.contains("User:") && !s.contains("Assistant:") {
        return None;
    }

    let mut turns = Vec::new();
    let mut current_user = String::new();
    let mut current_llm = String::new();
    let mut in_user = false;
    let mut in_assistant = false;

    for line in s.lines() {
        let trimmed_line = line.trim();
        if trimmed_line.starts_with("User:") {
            // 遇到新 User: 触发上一组收尾
            if !current_user.is_empty() || !current_llm.is_empty() {
                turns.push(make_turn(
                    current_user.clone(),
                    current_llm.clone(),
                ));
                current_user.clear();
                current_llm.clear();
            }
            current_user = trimmed_line
                .strip_prefix("User:")
                .unwrap_or("")
                .trim()
                .to_string();
            in_user = true;
            in_assistant = false;
        } else if trimmed_line.starts_with("Assistant:") {
            current_llm = trimmed_line
                .strip_prefix("Assistant:")
                .unwrap_or("")
                .trim()
                .to_string();
            in_user = false;
            in_assistant = true;
        } else if in_user {
            current_user.push('\n');
            current_user.push_str(line);
        } else if in_assistant {
            current_llm.push('\n');
            current_llm.push_str(line);
        }
    }

    // 末尾剩余内容作为最后一组
    if !current_user.is_empty() || !current_llm.is_empty() {
        turns.push(make_turn(current_user, current_llm));
    }

    if turns.is_empty() {
        None
    } else {
        Some(turns)
    }
}

/// 构造一个最小可用的 MessageTurn(仅填 user/llm 文本,其余字段取缺省)
fn make_turn(user_text: String, llm_text: String) -> MessageTurn {
    MessageTurn {
        id: Uuid::new_v4(),
        user_message: MessageContent {
            text: Some(user_text),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            thinking: None,
            file_changes: Vec::new(),
        },
        llm_message: MessageContent {
            text: Some(llm_text),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            thinking: None,
            file_changes: Vec::new(),
        },
        tags: vec![Tag::Text],
        timestamp: Utc::now(),
        token_count: 0,
        stop_reason: None,
        cost: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 辅助:从 MessageTurn 取 user_message.text
    fn user_text(t: &MessageTurn) -> &str {
        t.user_message.text.as_deref().unwrap_or("")
    }

    /// 辅助:从 MessageTurn 取 llm_message.text
    fn llm_text(t: &MessageTurn) -> &str {
        t.llm_message.text.as_deref().unwrap_or("")
    }

    #[test]
    fn test_parse_json_array_of_turns() {
        let json = r#"[{"user_message":"你好","llm_message":"你好!"}]"#;
        let result = parse_context(json).unwrap();
        assert_eq!(result.method, "json");
        assert_eq!(result.turns.len(), 1);
        assert_eq!(user_text(&result.turns[0]), "你好");
        assert_eq!(llm_text(&result.turns[0]), "你好!");
    }

    #[test]
    fn test_parse_json_with_extra_fields() {
        // 多余字段(id/timestamp)应被忽略,不阻塞解析
        let json = r#"[{"id":"1","timestamp":"2026-07-07","user_message":"问题","llm_message":"回答"}]"#;
        let result = parse_context(json).unwrap();
        assert_eq!(result.method, "json");
        assert_eq!(result.turns.len(), 1);
        assert_eq!(user_text(&result.turns[0]), "问题");
        assert_eq!(llm_text(&result.turns[0]), "回答");
    }

    #[test]
    fn test_parse_json_invalid_returns_none() {
        let json = r#"not a json"#;
        assert!(parse_context(json).is_none());
    }

    #[test]
    fn test_parse_user_assistant_markers() {
        let text = "User: 你好\nAssistant: 你好!\nUser: 第二个问题\nAssistant: 第二个回答";
        let result = parse_context(text).unwrap();
        assert_eq!(result.method, "separator");
        assert_eq!(result.turns.len(), 2);
        assert_eq!(user_text(&result.turns[0]), "你好");
        assert_eq!(llm_text(&result.turns[0]), "你好!");
        assert_eq!(user_text(&result.turns[1]), "第二个问题");
        assert_eq!(llm_text(&result.turns[1]), "第二个回答");
    }

    #[test]
    fn test_parse_dash_separator_not_supported_returns_none() {
        // `---` 分隔符暂不支持,返回 None
        let text = "第一段\n---\n第二段\n---\n第三段";
        assert!(parse_context(text).is_none());
    }

    #[test]
    fn test_parse_unrecognized_format_returns_none() {
        let text = "这是一段纯文本,没有 User: 或 Assistant: 标记,也不是 JSON";
        assert!(parse_context(text).is_none());
    }

    #[test]
    fn test_parse_empty_string_returns_none() {
        assert!(parse_context("").is_none());
        assert!(parse_context("   ").is_none());
    }

    #[test]
    fn test_parse_json_empty_array_returns_none() {
        assert!(parse_context("[]").is_none());
    }
}
