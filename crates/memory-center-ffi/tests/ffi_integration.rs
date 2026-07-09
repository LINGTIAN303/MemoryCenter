//! # FFI 集成测试
//!
//! 通过 Rust 直接调用 C ABI 函数，验证 FFI 边界的正确性。
//! 测试覆盖：句柄生命周期 + archive + retrieve + get_summaries + render_prompt + 错误处理。

use chrono::Utc;
use memory_center_core::model::{MessageContent, MessageTurn, Tag};
use MemoryCenter::*; // crate lib name = "MemoryCenter"
use std::ffi::CStr;
use std::ptr;
use tempfile::TempDir;
use uuid::Uuid;

// ============================================================================
// 测试辅助函数
// ============================================================================

/// 构造测试用 MessageTurn
fn make_turn(user: &str, llm: &str) -> MessageTurn {
    MessageTurn {
        id: Uuid::new_v4(),
        user_message: MessageContent {
            text: Some(user.into()),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            thinking: None,
            file_changes: Vec::new(),
        },
        llm_message: MessageContent {
            text: Some(llm.into()),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            thinking: None,
            file_changes: Vec::new(),
        },
        tags: vec![Tag::Text],
        timestamp: Utc::now(),
        token_count: 100,
        stop_reason: None,
        cost: None,
    }
}

/// 创建 handle（无 project_id）
fn make_handle(dir: &TempDir, session: &str) -> *mut MemoryCenterHandle {
    let root = dir.path().to_str().unwrap();
    let root_c = CString::new(root).unwrap();
    let session_c = CString::new(session).unwrap();
    unsafe { memory_center_new(root_c.as_ptr(), session_c.as_ptr(), ptr::null()) }
}

/// 从结果获取数据字符串（负责释放）
unsafe fn get_data(result: *const MemoryCenterResult) -> String {
    let data_ptr = memory_center_get_data(result);
    if data_ptr.is_null() {
        return String::new();
    }
    let s = CStr::from_ptr(data_ptr).to_string_lossy().to_string();
    memory_center_free_string(data_ptr);
    s
}

/// 从结果获取错误字符串（负责释放）
unsafe fn get_error(result: *const MemoryCenterResult) -> String {
    let err_ptr = memory_center_get_error(result);
    if err_ptr.is_null() {
        return String::new();
    }
    let s = CStr::from_ptr(err_ptr).to_string_lossy().to_string();
    memory_center_free_string(err_ptr);
    s
}

/// 归档一批 turns 并返回 hook_id
fn archive_turns(handle: *mut MemoryCenterHandle, turns: Vec<MessageTurn>) -> String {
    let turns_json = serde_json::to_string(&turns).unwrap();
    let turns_c = CString::new(turns_json).unwrap();
    let result = unsafe { memory_center_archive(handle, turns_c.as_ptr()) };
    assert!(unsafe { memory_center_is_ok(result) }, "归档失败");
    let data = unsafe { get_data(result) };
    unsafe { memory_center_result_free(result) };
    // 解析 hook_id（SummaryView JSON）
    let v: serde_json::Value = serde_json::from_str(&data).unwrap();
    v["hook_id"].as_str().unwrap().to_string()
}

// 需要 CString 类型（辅助函数中用到）
use std::ffi::CString;

// ============================================================================
// 句柄生命周期测试
// ============================================================================

#[test]
fn test_handle_lifecycle() {
    let dir = TempDir::new().unwrap();
    let handle = make_handle(&dir, "test-session");
    assert!(!handle.is_null());
    unsafe { memory_center_free(handle) };
}

#[test]
fn test_handle_null_params() {
    unsafe {
        // root_path 为 NULL
        let session_c = CString::new("s").unwrap();
        assert!(memory_center_new(ptr::null(), session_c.as_ptr(), ptr::null()).is_null());
        // session_id 为 NULL
        let root_c = CString::new("/tmp").unwrap();
        assert!(memory_center_new(root_c.as_ptr(), ptr::null(), ptr::null()).is_null());
    }
}

#[test]
fn test_handle_with_project_id() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let root_c = CString::new(root).unwrap();
    let session_c = CString::new("session-proj").unwrap();
    let project_c = CString::new("project-1").unwrap();
    let handle = unsafe {
        memory_center_new(
            root_c.as_ptr(),
            session_c.as_ptr(),
            project_c.as_ptr(),
        )
    };
    assert!(!handle.is_null());
    unsafe { memory_center_free(handle) };
}

// ============================================================================
// 归档 + 检索 全链路测试
// ============================================================================

#[test]
fn test_archive_and_retrieve() {
    let dir = TempDir::new().unwrap();
    let handle = make_handle(&dir, "session-1");
    assert!(!handle.is_null());

    // 归档
    let turns = vec![
        make_turn("你好", "你好！有什么可以帮你的？"),
        make_turn("讲个笑话", "为什么程序员喜欢黑夜？因为光会引来 bug。"),
    ];
    let hook_id = archive_turns(handle, turns);
    assert!(!hook_id.is_empty());

    // retrieve
    let hook_id_c = CString::new(hook_id).unwrap();
    let result = unsafe { memory_center_retrieve(handle, hook_id_c.as_ptr()) };
    assert!(unsafe { memory_center_is_ok(result) });

    let data = unsafe { get_data(result) };
    assert!(data.contains("session-1"));
    assert!(data.contains("讲个笑话"));
    assert!(data.contains("bug"));

    unsafe { memory_center_result_free(result) };
    unsafe { memory_center_free(handle) };
}

#[test]
fn test_retrieve_invalid_hook_id() {
    let dir = TempDir::new().unwrap();
    let handle = make_handle(&dir, "session-retr-err");
    assert!(!handle.is_null());

    let invalid_id = CString::new("invalid-uuid").unwrap();
    let result = unsafe { memory_center_retrieve(handle, invalid_id.as_ptr()) };
    // 无效 hook_id 应返回失败
    assert!(!unsafe { memory_center_is_ok(result) });

    let error = unsafe { get_error(result) };
    assert!(!error.is_empty());

    unsafe { memory_center_result_free(result) };
    unsafe { memory_center_free(handle) };
}

// ============================================================================
// 摘要视图测试
// ============================================================================

#[test]
fn test_get_summaries_empty() {
    let dir = TempDir::new().unwrap();
    let handle = make_handle(&dir, "session-empty");
    assert!(!handle.is_null());

    let result = unsafe { memory_center_get_summaries(handle) };
    assert!(unsafe { memory_center_is_ok(result) });

    let data = unsafe { get_data(result) };
    assert_eq!(data, "[]");

    unsafe { memory_center_result_free(result) };
    unsafe { memory_center_free(handle) };
}

#[test]
fn test_get_summaries_after_archive() {
    let dir = TempDir::new().unwrap();
    let handle = make_handle(&dir, "session-multi");
    assert!(!handle.is_null());

    // 归档两次
    archive_turns(handle, vec![make_turn("第一个问题", "第一个回答")]);
    archive_turns(handle, vec![make_turn("第二个问题", "第二个回答")]);

    // 获取摘要
    let result = unsafe { memory_center_get_summaries(handle) };
    assert!(unsafe { memory_center_is_ok(result) });

    let data = unsafe { get_data(result) };
    let v: serde_json::Value = serde_json::from_str(&data).unwrap();
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 2);

    unsafe { memory_center_result_free(result) };
    unsafe { memory_center_free(handle) };
}

// ============================================================================
// 渲染 prompt 测试
// ============================================================================

#[test]
fn test_render_prompt_empty() {
    let dir = TempDir::new().unwrap();
    let handle = make_handle(&dir, "session-prompt-empty");
    assert!(!handle.is_null());

    let result = unsafe { memory_center_render_prompt(handle) };
    assert!(unsafe { memory_center_is_ok(result) });

    let prompt = unsafe { get_data(result) };
    assert_eq!(prompt, "");

    unsafe { memory_center_result_free(result) };
    unsafe { memory_center_free(handle) };
}

#[test]
fn test_render_prompt_with_memory() {
    let dir = TempDir::new().unwrap();
    let handle = make_handle(&dir, "session-prompt");
    assert!(!handle.is_null());

    archive_turns(handle, vec![make_turn("测试渲染功能", "这是渲染测试内容")]);

    let result = unsafe { memory_center_render_prompt(handle) };
    assert!(unsafe { memory_center_is_ok(result) });

    let prompt = unsafe { get_data(result) };
    assert!(prompt.contains("可用记忆索引"));
    assert!(prompt.contains("测试渲染功能"));
    assert!(prompt.contains("近期记忆"));

    unsafe { memory_center_result_free(result) };
    unsafe { memory_center_free(handle) };
}

// ============================================================================
// 错误处理测试
// ============================================================================

#[test]
fn test_archive_empty_turns() {
    let dir = TempDir::new().unwrap();
    let handle = make_handle(&dir, "session-err");
    assert!(!handle.is_null());

    let turns_c = CString::new("[]").unwrap();
    let result = unsafe { memory_center_archive(handle, turns_c.as_ptr()) };
    assert!(!unsafe { memory_center_is_ok(result) });

    let error = unsafe { get_error(result) };
    assert!(error.contains("空"));

    unsafe { memory_center_result_free(result) };
    unsafe { memory_center_free(handle) };
}

#[test]
fn test_archive_invalid_json() {
    let dir = TempDir::new().unwrap();
    let handle = make_handle(&dir, "session-bad-json");
    assert!(!handle.is_null());

    let bad_c = CString::new("not a json").unwrap();
    let result = unsafe { memory_center_archive(handle, bad_c.as_ptr()) };
    assert!(!unsafe { memory_center_is_ok(result) });

    let error = unsafe { get_error(result) };
    assert!(error.contains("解析") || error.contains("JSON"));

    unsafe { memory_center_result_free(result) };
    unsafe { memory_center_free(handle) };
}

#[test]
fn test_null_handle_returns_error() {
    unsafe {
        let turns_c = CString::new("[]").unwrap();
        let result = memory_center_archive(ptr::null_mut(), turns_c.as_ptr());
        assert!(!memory_center_is_ok(result));
        memory_center_result_free(result);

        let result = memory_center_get_summaries(ptr::null_mut());
        assert!(!memory_center_is_ok(result));
        memory_center_result_free(result);
    }
}

#[test]
fn test_run_compaction_invalid_period() {
    let dir = TempDir::new().unwrap();
    let handle = make_handle(&dir, "session-compaction-err");
    assert!(!handle.is_null());

    // 无效的 period 值（应为 0 或 1）
    let result = unsafe { memory_center_run_compaction(handle, 99) };
    assert!(!unsafe { memory_center_is_ok(result) });

    let error = unsafe { get_error(result) };
    assert!(error.contains("无效") || error.contains("period"));

    unsafe { memory_center_result_free(result) };
    unsafe { memory_center_free(handle) };
}

// ============================================================================
// 周期任务测试
// ============================================================================

#[test]
fn test_run_compaction_weekly_no_daily() {
    let dir = TempDir::new().unwrap();
    let handle = make_handle(&dir, "session-no-daily");
    assert!(!handle.is_null());

    // 无 daily 记忆文件时，weekly_merge 应失败
    let result = unsafe { memory_center_run_compaction(handle, COMPACTION_WEEKLY) };
    assert!(!unsafe { memory_center_is_ok(result) });

    let error = unsafe { get_error(result) };
    assert!(error.contains("无") || error.contains("daily"));

    unsafe { memory_center_result_free(result) };
    unsafe { memory_center_free(handle) };
}

#[test]
fn test_full_workflow_archive_then_compact() {
    let dir = TempDir::new().unwrap();
    let handle = make_handle(&dir, "session-full");
    assert!(!handle.is_null());

    // 1. 归档几条含实质内容的 turns
    archive_turns(handle, vec![make_turn("讲解 Rust 的所有权", "所有权是 Rust 的核心特性...")]);
    archive_turns(handle, vec![make_turn("解释生命周期", "生命周期确保引用始终有效...")]);

    // 2. 验证摘要
    let summaries_result = unsafe { memory_center_get_summaries(handle) };
    assert!(unsafe { memory_center_is_ok(summaries_result) });
    let summaries_data = unsafe { get_data(summaries_result) };
    let v: serde_json::Value = serde_json::from_str(&summaries_data).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 2);
    unsafe { memory_center_result_free(summaries_result) };

    // 3. 周级合并
    let compact_result = unsafe { memory_center_run_compaction(handle, COMPACTION_WEEKLY) };
    assert!(unsafe { memory_center_is_ok(compact_result) });

    let compact_data = unsafe { get_data(compact_result) };
    let cv: serde_json::Value = serde_json::from_str(&compact_data).unwrap();
    assert_eq!(cv["period"].as_str(), Some("weekly"));
    assert!(cv["total_turns"].as_u64().unwrap() >= 1);
    assert!(cv["hooks_count"].as_u64().unwrap() >= 1);
    unsafe { memory_center_result_free(compact_result) };

    // 4. 释放
    unsafe { memory_center_free(handle) };
}

// ============================================================================
// 内存安全测试（验证无泄漏/双释放）
// ============================================================================

#[test]
fn test_result_free_idempotent() {
    // 释放 NULL 结果不应崩溃
    unsafe { memory_center_result_free(ptr::null_mut()) };
    // 释放 NULL 字符串不应崩溃
    unsafe { memory_center_free_string(ptr::null_mut()) };
    // 释放 NULL handle 不应崩溃
    unsafe { memory_center_free(ptr::null_mut()) };
}

#[test]
fn test_get_data_on_null_result() {
    unsafe {
        assert!(memory_center_get_data(ptr::null()).is_null());
        assert!(memory_center_get_error(ptr::null()).is_null());
        assert!(!memory_center_is_ok(ptr::null()));
    }
}
