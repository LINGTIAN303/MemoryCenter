//! LocalStorage session_meta 读写测试（v2.33）

use memory_center_core::storage::{LocalStorage, SessionMeta, Storage};
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

#[tokio::test]
async fn test_write_then_read_session_meta() {
    let tmp = TempDir::new().unwrap();
    let storage = LocalStorage::new(tmp.path().to_path_buf());
    let sid = "test-session-1";

    let meta = make_meta("coding", 0.85, "keyword");
    storage.write_session_meta(sid, &meta).await.unwrap();

    let read = storage.read_session_meta(sid).await.unwrap();
    assert!(read.is_some(), "读取应命中已写入的 meta");
    let read = read.unwrap();
    assert_eq!(read.scenario, "coding");
    assert!((read.confidence - 0.85).abs() < 1e-6);
    assert_eq!(read.method, "keyword");
}

#[tokio::test]
async fn test_read_session_meta_returns_none_when_absent() {
    let tmp = TempDir::new().unwrap();
    let storage = LocalStorage::new(tmp.path().to_path_buf());

    let read = storage.read_session_meta("never-archived-session").await.unwrap();
    assert!(read.is_none(), "未写入的 session 应返回 None");
}

#[tokio::test]
async fn test_write_session_meta_overwrites_existing() {
    let tmp = TempDir::new().unwrap();
    let storage = LocalStorage::new(tmp.path().to_path_buf());
    let sid = "test-session-2";

    let meta1 = make_meta("coding", 0.7, "keyword");
    storage.write_session_meta(sid, &meta1).await.unwrap();

    let meta2 = make_meta("writing", 0.9, "llm");
    storage.write_session_meta(sid, &meta2).await.unwrap();

    let read = storage.read_session_meta(sid).await.unwrap().unwrap();
    assert_eq!(read.scenario, "writing", "覆盖写入应保留最新值");
    assert!((read.confidence - 0.9).abs() < 1e-6);
    assert_eq!(read.method, "llm");
}

#[tokio::test]
async fn test_session_meta_persists_custom_scenario() {
    let tmp = TempDir::new().unwrap();
    let storage = LocalStorage::new(tmp.path().to_path_buf());
    let sid = "test-session-3";

    let meta = make_meta("custom:medical", 0.65, "llm");
    storage.write_session_meta(sid, &meta).await.unwrap();

    let read = storage.read_session_meta(sid).await.unwrap().unwrap();
    assert_eq!(read.scenario, "custom:medical");
}

#[tokio::test]
async fn test_session_meta_isolation_between_sessions() {
    let tmp = TempDir::new().unwrap();
    let storage = LocalStorage::new(tmp.path().to_path_buf());

    let meta_a = make_meta("coding", 0.8, "keyword");
    let meta_b = make_meta("writing", 0.75, "llm");
    storage.write_session_meta("session-a", &meta_a).await.unwrap();
    storage.write_session_meta("session-b", &meta_b).await.unwrap();

    let read_a = storage.read_session_meta("session-a").await.unwrap().unwrap();
    let read_b = storage.read_session_meta("session-b").await.unwrap().unwrap();
    assert_eq!(read_a.scenario, "coding");
    assert_eq!(read_b.scenario, "writing");
}
