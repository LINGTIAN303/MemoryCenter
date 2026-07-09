//! # 序列化格式模块
//!
//! 提供 JSON / MessagePack 双格式支持。
//!
//! ## 设计
//!
//! - [`SerializationFormat`] 枚举：可选 `Json` / `MessagePack`
//! - 写入时按配置格式序列化
//! - 读取时根据文件后缀自动识别格式（`.json` / `.msgpack`）
//!
//! ## 选型
//!
//! | 格式 | 体积 | 可读性 | 适用场景 |
//! |------|------|--------|----------|
//! | JSON | 较大 | 好 | 调试、人工查看 |
//! | MessagePack | 紧凑 | 差 | 生产环境、存储密集 |

use crate::model::{IndexDocument, MemoryFile};

/// 序列化格式枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SerializationFormat {
    /// JSON 文本格式（默认，便于调试）
    Json,
    /// MessagePack 二进制格式（紧凑高效）
    MessagePack,
}

impl SerializationFormat {
    /// 获取文件后缀
    pub fn extension(&self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::MessagePack => "msgpack",
        }
    }

    /// 根据文件后缀推断格式（默认 JSON）
    pub fn detect_from_extension(ext: Option<&str>) -> Self {
        match ext {
            Some("msgpack") => Self::MessagePack,
            _ => Self::Json,
        }
    }

    /// 根据文件路径推断格式
    pub fn detect_from_path(path: &std::path::Path) -> Self {
        let ext = path.extension().and_then(|e| e.to_str());
        Self::detect_from_extension(ext)
    }

    /// 序列化 MemoryFile
    pub fn serialize_memory(&self, file: &MemoryFile) -> crate::Result<Vec<u8>> {
        match self {
            Self::Json => serde_json::to_vec_pretty(file)
                .map_err(|e| crate::Error::Serialize(format!("JSON 序列化 MemoryFile 失败: {}", e))),
            Self::MessagePack => rmp_serde::to_vec_named(file)
                .map_err(|e| crate::Error::Serialize(format!("MessagePack 序列化 MemoryFile 失败: {}", e))),
        }
    }

    /// 反序列化 MemoryFile
    pub fn deserialize_memory(&self, content: &[u8]) -> crate::Result<MemoryFile> {
        match self {
            Self::Json => serde_json::from_slice(content)
                .map_err(|e| crate::Error::Serialize(format!("JSON 反序列化 MemoryFile 失败: {}", e))),
            Self::MessagePack => rmp_serde::from_slice(content)
                .map_err(|e| crate::Error::Serialize(format!("MessagePack 反序列化 MemoryFile 失败: {}", e))),
        }
    }

    /// 序列化 IndexDocument
    pub fn serialize_index(&self, doc: &IndexDocument) -> crate::Result<Vec<u8>> {
        match self {
            Self::Json => serde_json::to_vec_pretty(doc)
                .map_err(|e| crate::Error::Serialize(format!("JSON 序列化 IndexDocument 失败: {}", e))),
            Self::MessagePack => rmp_serde::to_vec_named(doc)
                .map_err(|e| crate::Error::Serialize(format!("MessagePack 序列化 IndexDocument 失败: {}", e))),
        }
    }

    /// 反序列化 IndexDocument
    pub fn deserialize_index(&self, content: &[u8]) -> crate::Result<IndexDocument> {
        match self {
            Self::Json => serde_json::from_slice(content)
                .map_err(|e| crate::Error::Serialize(format!("JSON 反序列化 IndexDocument 失败: {}", e))),
            Self::MessagePack => rmp_serde::from_slice(content)
                .map_err(|e| crate::Error::Serialize(format!("MessagePack 反序列化 IndexDocument 失败: {}", e))),
        }
    }
}

impl Default for SerializationFormat {
    fn default() -> Self {
        Self::Json
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ArchivePeriod, MemoryFile, MessageContent, MessageTurn, Tag};
    use chrono::Utc;
    use uuid::Uuid;

    fn sample_memory_file() -> MemoryFile {
        let turn = MessageTurn {
            id: Uuid::new_v4(),
            user_message: MessageContent {
                text: Some("测试用户消息".into()),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                thinking: None,
                file_changes: Vec::new(),
            },
            llm_message: MessageContent {
                text: Some("测试 LLM 回复".into()),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                thinking: None,
                file_changes: Vec::new(),
            },
            tags: vec![Tag::Text],
            timestamp: Utc::now(),
            token_count: 50,
            stop_reason: None,
            cost: None,
        };
        MemoryFile::new(
            String::from("test-session"),
            Some(String::from("test-project")),
            vec![turn],
            ArchivePeriod::Daily,
        )
    }

    #[test]
    fn test_extension() {
        assert_eq!(SerializationFormat::Json.extension(), "json");
        assert_eq!(SerializationFormat::MessagePack.extension(), "msgpack");
    }

    #[test]
    fn test_detect_from_extension() {
        assert_eq!(
            SerializationFormat::detect_from_extension(Some("json")),
            SerializationFormat::Json
        );
        assert_eq!(
            SerializationFormat::detect_from_extension(Some("msgpack")),
            SerializationFormat::MessagePack
        );
        assert_eq!(
            SerializationFormat::detect_from_extension(None),
            SerializationFormat::Json
        );
        assert_eq!(
            SerializationFormat::detect_from_extension(Some("unknown")),
            SerializationFormat::Json
        );
    }

    #[test]
    fn test_detect_from_path() {
        let json_path = std::path::Path::new("memory/2026-07-02_143052.json");
        assert_eq!(
            SerializationFormat::detect_from_path(json_path),
            SerializationFormat::Json
        );

        let msgpack_path = std::path::Path::new("memory/2026-07-02_143052.msgpack");
        assert_eq!(
            SerializationFormat::detect_from_path(msgpack_path),
            SerializationFormat::MessagePack
        );

        let no_ext = std::path::Path::new("memory/no_ext");
        assert_eq!(
            SerializationFormat::detect_from_path(no_ext),
            SerializationFormat::Json
        );
    }

    #[test]
    fn test_default_is_json() {
        assert_eq!(SerializationFormat::default(), SerializationFormat::Json);
    }

    #[test]
    fn test_json_memory_roundtrip() {
        let original = sample_memory_file();
        let content = SerializationFormat::Json
            .serialize_memory(&original)
            .unwrap();
        let restored = SerializationFormat::Json
            .deserialize_memory(&content)
            .unwrap();
        assert_eq!(original.id, restored.id);
        assert_eq!(original.session_id, restored.session_id);
        assert_eq!(original.turns.len(), restored.turns.len());
        assert_eq!(original.total_tokens, restored.total_tokens);
    }

    #[test]
    fn test_msgpack_memory_roundtrip() {
        let original = sample_memory_file();
        let content = SerializationFormat::MessagePack
            .serialize_memory(&original)
            .unwrap();
        let restored = SerializationFormat::MessagePack
            .deserialize_memory(&content)
            .unwrap();
        assert_eq!(original.id, restored.id);
        assert_eq!(original.session_id, restored.session_id);
        assert_eq!(original.turns.len(), restored.turns.len());
    }

    #[test]
    fn test_msgpack_more_compact_than_json() {
        let file = sample_memory_file();
        let json_size = SerializationFormat::Json
            .serialize_memory(&file)
            .unwrap()
            .len();
        let msgpack_size = SerializationFormat::MessagePack
            .serialize_memory(&file)
            .unwrap()
            .len();
        // MessagePack 应该比 JSON 更紧凑（通常 < JSON 大小）
        assert!(
            msgpack_size < json_size,
            "MessagePack ({}) 应小于 JSON ({})",
            msgpack_size,
            json_size
        );
    }

    #[test]
    fn test_invalid_json_deserialize() {
        let bad_content = b"not a valid json";
        let result = SerializationFormat::Json.deserialize_memory(bad_content);
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_msgpack_deserialize() {
        let bad_content = b"not a valid msgpack";
        let result = SerializationFormat::MessagePack.deserialize_memory(bad_content);
        assert!(result.is_err());
    }

    #[test]
    fn test_json_index_roundtrip() {
        let doc = IndexDocument::new(
            String::from("test-session"),
            Some(String::from("test-project")),
            ArchivePeriod::Daily,
        );
        let content = SerializationFormat::Json
            .serialize_index(&doc)
            .unwrap();
        let restored = SerializationFormat::Json
            .deserialize_index(&content)
            .unwrap();
        assert_eq!(doc.session_id, restored.session_id);
        assert_eq!(doc.period, restored.period);
    }

    #[test]
    fn test_msgpack_index_roundtrip() {
        let doc = IndexDocument::new(
            String::from("test-session"),
            Some(String::from("test-project")),
            ArchivePeriod::Daily,
        );
        let content = SerializationFormat::MessagePack
            .serialize_index(&doc)
            .unwrap();
        let restored = SerializationFormat::MessagePack
            .deserialize_index(&content)
            .unwrap();
        assert_eq!(doc.session_id, restored.session_id);
    }
}
