//! pre_compress_hook 集成测试（v2.34 Task 8）
//!
//! 验证 pre_compress_hook 工具的完整流程：
//! 1. JSON 数组输入 → 解析成功，parsed_turns_count 正确
//! 2. 纯文本输入 → 解析失败，仅存 raw_context（parse_success=false）
//! 3. User:/Assistant: 分隔符格式 → 解析成功，parsed_turns_count 正确
//! 4. raw_context 文件实际落盘（双轨存储兜底）
//!
//! 调用方式：直接调 HippocampusMcp::pre_compress_hook 方法（非走 MCP 协议层）。
//! 方法返回 Result<String, McpError>，String 为 PreCompressResult 序列化后的 JSON。

use hippocampus_mcp::{HippocampusMcp, PreCompressParams};
use rmcp::handler::server::wrapper::Parameters;
use serde_json::Value;
use tempfile::TempDir;

/// 构造绑定到临时目录的 MCP 实例（与 lib.rs 内联测试 make_mcp 一致）
fn make_mcp(tmpdir: &TempDir) -> HippocampusMcp {
    HippocampusMcp::new(tmpdir.path().to_path_buf())
}

/// 通过 serde_json 构造 PreCompressParams（字段私有，借 Deserialize 反序列化）
fn make_params(session_id: &str, full_context: &str) -> PreCompressParams {
    let json = serde_json::json!({
        "session_id": session_id,
        "full_context": full_context,
    });
    serde_json::from_value(json).expect("PreCompressParams 反序列化失败")
}

/// 调用 pre_compress_hook 并解析返回的 JSON 字符串为 serde_json::Value
async fn call_hook(
    mcp: &HippocampusMcp,
    params: PreCompressParams,
) -> Value {
    let result_str = mcp
        .pre_compress_hook(Parameters(params))
        .await
        .expect("pre_compress_hook 调用失败");
    serde_json::from_str(&result_str).expect("返回结果不是合法 JSON")
}

#[tokio::test]
async fn test_pre_compress_hook_with_json_context() {
    let tmp = TempDir::new().unwrap();
    let mcp = make_mcp(&tmp);

    // JSON 数组格式：1 轮对话（context_parser 期望 user_message/llm_message 为字符串）
    let full_context = r#"[{"user_message":"你好","llm_message":"你好!"}]"#;
    let params = make_params("integration-json-sess", full_context);
    let result = call_hook(&mcp, params).await;

    // 验证解析成功，且 parsed_turns_count=1
    assert_eq!(result["parse_success"], true, "JSON 输入应解析成功");
    assert_eq!(
        result["parsed_turns_count"], 1,
        "JSON 输入应解析出 1 轮"
    );
    // hook_id / raw_context_path 应非空
    assert!(
        result["hook_id"].as_str().unwrap().len() > 0,
        "hook_id 不应为空"
    );
    assert!(
        result["raw_context_path"].as_str().unwrap().contains("raw_contexts"),
        "raw_context_path 应包含 raw_contexts 目录"
    );
}

#[tokio::test]
async fn test_pre_compress_hook_with_plain_text_context() {
    let tmp = TempDir::new().unwrap();
    let mcp = make_mcp(&tmp);

    // 纯文本：无 JSON 数组、无 User:/Assistant: 标记 → 解析失败
    let full_context = "这是一段纯文本，没有 JSON 结构也没有 User:/Assistant: 标记";
    let params = make_params("integration-plain-sess", full_context);
    let result = call_hook(&mcp, params).await;

    // 验证解析失败，仅存 raw_context
    assert_eq!(
        result["parse_success"], false,
        "纯文本输入应解析失败"
    );
    assert_eq!(
        result["parsed_turns_count"], 0,
        "解析失败时 parsed_turns_count 应为 0"
    );
    // raw_context 仍应写入
    assert!(
        result["raw_context_path"].as_str().unwrap().len() > 0,
        "即使解析失败，raw_context_path 也应非空"
    );
}

#[tokio::test]
async fn test_pre_compress_hook_with_user_assistant_markers() {
    let tmp = TempDir::new().unwrap();
    let mcp = make_mcp(&tmp);

    // User:/Assistant: 分隔符格式：2 轮对话
    let full_context = "User: 第一个问题\nAssistant: 第一个回答\nUser: 第二个问题\nAssistant: 第二个回答";
    let params = make_params("integration-sep-sess", full_context);
    let result = call_hook(&mcp, params).await;

    // 验证解析成功，且 parsed_turns_count=2
    assert_eq!(
        result["parse_success"], true,
        "User:/Assistant: 格式应解析成功"
    );
    assert_eq!(
        result["parsed_turns_count"], 2,
        "应解析出 2 轮对话"
    );
}

#[tokio::test]
async fn test_pre_compress_hook_raw_context_file_exists() {
    let tmp = TempDir::new().unwrap();
    let storage_root = tmp.path().to_path_buf();
    let mcp = make_mcp(&tmp);

    // 用任意可识别格式触发归档（context_parser 期望 user_message/llm_message 为字符串）
    let full_context = r#"[{"user_message":"raw 落盘验证","llm_message":"ok"}]"#;
    let params = make_params("integration-rawfile-sess", full_context);
    let result = call_hook(&mcp, params).await;

    // 取 raw_context_path（相对 POSIX 路径，如 sessions/{sid}/raw_contexts/{hook_id}.txt）
    let raw_rel = result["raw_context_path"]
        .as_str()
        .expect("raw_context_path 应为字符串");
    let raw_abs = storage_root.join(raw_rel);

    // 验证文件实际存在
    assert!(
        raw_abs.exists(),
        "raw_context 文件应存在: {:?}",
        raw_abs
    );

    // 验证文件内容与传入的 full_context 一致（双轨存储兜底的核心保证）
    let content = std::fs::read_to_string(&raw_abs).expect("读取 raw_context 文件失败");
    assert_eq!(
        content, full_context,
        "raw_context 文件内容应与 full_context 完全一致"
    );
}

// ============================================================================
// v2.34 风险点 2 & 3：hook_id 一致性 + archive_reason + raw_context_path 测试
// ============================================================================

/// 验证 pre_compress_hook 返回的 hook_id 与 IndexHook.id 一致，
/// 且 IndexHook.archive_reason == "pre_compress"、raw_context_path 与返回值一致。
///
/// 这是 v2.34 修复的核心保证：预生成 hook_id（用于 raw_context 文件命名）
/// 必须与 Archiver 内部生成的 IndexHook.id 对齐，否则 retrieve 时无法按 hook_id 检索。
#[tokio::test]
async fn test_pre_compress_hook_id_consistency() {
    use hippocampus_core::model::ArchivePeriod;
    use hippocampus_core::storage::{LocalStorage, Storage};
    use std::sync::Arc;

    let tmp = TempDir::new().unwrap();
    let storage_root = tmp.path().to_path_buf();
    let mcp = make_mcp(&tmp);

    // JSON 数组格式：1 轮对话（context_parser 期望 user_message/llm_message 为字符串）
    let full_context = r#"[{"user_message":"一致性验证","llm_message":"ok"}]"#;
    let params = make_params("integration-consistency-sess", full_context);
    let result = call_hook(&mcp, params).await;

    // 1. 验证 parse_success=true（解析成功才会走 Archiver 归档路径）
    assert_eq!(
        result["parse_success"], true,
        "JSON 输入应解析成功（否则不会走 Archiver，无法验证 hook_id 一致性）"
    );

    // 2. 验证 hook_id 是合法 UUID（36 字符，含 4 个连字符）
    let hook_id = result["hook_id"].as_str().expect("hook_id 应为字符串");
    assert_eq!(
        hook_id.len(),
        36,
        "hook_id 应为 36 字符 UUID，实际: {}",
        hook_id
    );
    assert_eq!(
        hook_id.matches('-').count(),
        4,
        "hook_id 应含 4 个连字符"
    );

    // 3. 验证 raw_context_path 包含 hook_id（文件命名一致性）
    let raw_context_path = result["raw_context_path"]
        .as_str()
        .expect("raw_context_path 应为字符串");
    assert!(
        raw_context_path.contains(hook_id),
        "raw_context_path 应包含 hook_id，实际: {} (hook_id={})",
        raw_context_path,
        hook_id
    );

    // 4. 验证 raw_context 文件实际存在且内容正确
    let raw_abs = storage_root.join(raw_context_path);
    assert!(
        raw_abs.exists(),
        "raw_context 文件应存在: {:?}",
        raw_abs
    );
    let content = std::fs::read_to_string(&raw_abs).expect("读取 raw_context 文件失败");
    assert_eq!(
        content, full_context,
        "raw_context 文件内容应与 full_context 完全一致"
    );

    // 5. 通过 LocalStorage 读取索引文档，验证 IndexHook 一致性
    let storage: Arc<LocalStorage> = Arc::new(LocalStorage::new(storage_root));
    let index = storage
        .read_index("integration-consistency-sess", None, ArchivePeriod::Daily)
        .await
        .expect("读取索引文档失败")
        .expect("索引文档应存在（pre_compress_hook 解析成功后应写入 Archiver）");

    // 索引文档中应有 1 个 hook
    assert_eq!(
        index.hooks.len(),
        1,
        "索引文档应有 1 个 hook（pre_compress_hook 归档一次）"
    );

    let hook = &index.hooks[0];

    // 6. 风险点 2：IndexHook.id == 预生成 hook_id（核心一致性保证）
    assert_eq!(
        hook.id.to_string(),
        hook_id,
        "IndexHook.id 应等于预生成 hook_id（v2.34 风险点 2 修复）"
    );

    // 7. 风险点 3：IndexHook.archive_reason == "pre_compress"
    assert_eq!(
        hook.archive_reason.as_deref(),
        Some("pre_compress"),
        "IndexHook.archive_reason 应为 'pre_compress'（v2.34 风险点 3 修复）"
    );

    // 8. 风险点 3：IndexHook.raw_context_path == 返回值
    assert_eq!(
        hook.raw_context_path.as_deref(),
        Some(raw_context_path),
        "IndexHook.raw_context_path 应等于返回的 raw_context_path（v2.34 风险点 3 修复）"
    );
}
