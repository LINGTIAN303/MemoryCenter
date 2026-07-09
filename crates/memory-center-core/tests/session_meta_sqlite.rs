//! SqliteStorage session_meta 读写测试（v2.33）

use memory_center_core::serialization::SerializationFormat;
use memory_center_core::sqlite::SqliteStorage;
use memory_center_core::storage::{SessionMeta, Storage};
use chrono::Utc;
use tempfile::TempDir;

fn make_meta(scenario: &str, confidence: f32, method: &str) -> SessionMeta {
    SessionMeta {
        scenario: scenario.to_string(),
        confidence,
        method: method.to_string(),
        detected_at: Utc::now(),
        agent_family: "Trae".to_string(),
        hook_mode: "pseudo".to_string(),
    }
}

fn make_storage(tmp: &TempDir) -> SqliteStorage {
    SqliteStorage::with_format(
        tmp.path().to_path_buf(),
        None,
        SerializationFormat::Json,
    )
    .unwrap()
}

#[tokio::test]
async fn test_sqlite_write_then_read_session_meta() {
    let tmp = TempDir::new().unwrap();
    let storage = make_storage(&tmp);
    let sid = "sqlite-session-1";

    let meta = make_meta("coding", 0.85, "keyword");
    storage.write_session_meta(sid, &meta).await.unwrap();

    let read = storage.read_session_meta(sid).await.unwrap();
    assert!(read.is_some());
    let read = read.unwrap();
    assert_eq!(read.scenario, "coding");
    assert!((read.confidence - 0.85).abs() < 1e-6);
    assert_eq!(read.method, "keyword");
}

#[tokio::test]
async fn test_sqlite_read_returns_none_when_absent() {
    let tmp = TempDir::new().unwrap();
    let storage = make_storage(&tmp);

    let read = storage.read_session_meta("never-existed").await.unwrap();
    assert!(read.is_none());
}

#[tokio::test]
async fn test_sqlite_write_overwrites_existing() {
    let tmp = TempDir::new().unwrap();
    let storage = make_storage(&tmp);
    let sid = "sqlite-session-2";

    let meta1 = make_meta("coding", 0.7, "keyword");
    storage.write_session_meta(sid, &meta1).await.unwrap();

    let meta2 = make_meta("writing", 0.9, "llm");
    storage.write_session_meta(sid, &meta2).await.unwrap();

    let read = storage.read_session_meta(sid).await.unwrap().unwrap();
    assert_eq!(read.scenario, "writing");
    assert_eq!(read.method, "llm");
}
