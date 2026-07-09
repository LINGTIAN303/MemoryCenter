//! CachedStorage session_meta 透传测试（v2.33）

use memory_center_core::cache::CachedStorage;
use memory_center_core::storage::{LocalStorage, SessionMeta, Storage};
use chrono::Utc;
use tempfile::TempDir;

fn make_meta(scenario: &str) -> SessionMeta {
    SessionMeta {
        scenario: scenario.to_string(),
        confidence: 0.8,
        method: "keyword".to_string(),
        detected_at: Utc::now(),
        agent_family: "Trae".to_string(),
        hook_mode: "pseudo".to_string(),
    }
}

#[tokio::test]
async fn test_cached_storage_passes_through_write_and_read() {
    let tmp = TempDir::new().unwrap();
    let inner = LocalStorage::new(tmp.path().to_path_buf());
    let cached = CachedStorage::new(inner);

    let meta = make_meta("coding");
    cached.write_session_meta("sess-cached", &meta).await.unwrap();

    // 通过 CachedStorage 读取，应命中 inner 的写入
    let read = cached.read_session_meta("sess-cached").await.unwrap();
    assert!(read.is_some(), "CachedStorage 应透传到 inner");
    assert_eq!(read.unwrap().scenario, "coding");
}

#[tokio::test]
async fn test_cached_storage_read_none_when_inner_absent() {
    let tmp = TempDir::new().unwrap();
    let inner = LocalStorage::new(tmp.path().to_path_buf());
    let cached = CachedStorage::new(inner);

    let read = cached.read_session_meta("never-existed").await.unwrap();
    assert!(read.is_none());
}
