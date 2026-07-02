//! # Hippocampus FFI
//!
//! C ABI 动态库，将 [`hippocampus_core`] 的核心能力暴露为 C 接口，
//! 供 Python / Node / Go / Java 等语言通过 FFI 调用。
//!
//! ## 设计原则
//!
//! - **FFI 边界统一用 JSON 字符串**：复杂数据结构通过 JSON 序列化字符串传递
//! - **内部 tokio Runtime**：FFI 函数同步签名，内部通过 `block_on` 执行 Core 异步方法
//! - **单线程模型**：handle 不保证线程安全，调用方需串行化对同一 handle 的并发访问
//! - **错误处理**：所有函数返回 `HippocampusResult*`，调用方负责释放
//!
//! ## 基本用法（伪代码）
//!
//! ```c
//! HippocampusHandle* h = hippocampus_new("/path/to/store", "session-1", NULL);
//! HippocampusResult* r = hippocampus_archive(h, turns_json);
//! if (hippocampus_is_ok(r)) {
//!     char* summary = hippocampus_get_data(r);
//!     // ...处理 summary...
//!     hippocampus_free_string(summary);
//! }
//! hippocampus_result_free(r);
//! hippocampus_free(h);
//! ```

#![allow(clippy::missing_safety_doc)]

use hippocampus_core::archive::Archiver;
use hippocampus_core::compact::Compactor;
use hippocampus_core::model::{ArchiveConfig, MessageTurn};
use hippocampus_core::retrieve::{Retriever, SummaryView};
use hippocampus_core::score::DefaultScorer;
use hippocampus_core::storage::{LocalStorage, Storage};
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::Arc;

/// 周期任务参数：周级合并
pub const COMPACTION_WEEKLY: u32 = 0;
/// 周期任务参数：月级评分淘汰
pub const COMPACTION_MONTHLY: u32 = 1;

/// Hippocampus 实例句柄（不透明指针）
///
/// 持有 tokio runtime + storage + 会话配置。
///
/// **线程安全**：不保证。调用方需串行化对同一 handle 的并发访问。
/// 多线程场景下，每线程独立创建 handle，或由调用方自行加锁。
pub struct HippocampusHandle {
    /// 存储后端
    storage: Arc<dyn Storage>,
    /// 异步运行时（current_thread，轻量，适合 FFI 单线程模型）
    runtime: tokio::runtime::Runtime,
    /// 归档配置
    config: ArchiveConfig,
    /// 会话 ID
    session_id: String,
    /// 项目 ID（可选，影响存储路径隔离）
    project_id: Option<String>,
}

/// 操作结果（包含成功标志、错误消息、返回数据）
pub struct HippocampusResult {
    is_ok: bool,
    error_message: Option<CString>,
    data: Option<CString>,
}

/// 周期任务返回的精简结果（用于 [`hippocampus_run_compaction`]）
#[derive(serde::Serialize)]
struct CompactionResult {
    /// 合并后的记忆文件 ID
    memory_file_id: String,
    /// 总轮次数
    total_turns: usize,
    /// 总 token 数
    total_tokens: usize,
    /// 索引钩子数量
    hooks_count: usize,
    /// 周期层级（daily/weekly/monthly）
    period: String,
}

// ============================================================================
// 内部辅助函数
// ============================================================================

/// 构造成功结果（将可序列化数据转为 JSON 字符串）
fn ok_result<T: serde::Serialize>(data: &T) -> *mut HippocampusResult {
    match serde_json::to_string(data) {
        Ok(json) => {
            // JSON 不会包含 null 字节，unwrap 安全
            let cstr = CString::new(json).unwrap_or_else(|_| CString::new("{}").unwrap());
            Box::into_raw(Box::new(HippocampusResult {
                is_ok: true,
                error_message: None,
                data: Some(cstr),
            }))
        }
        Err(e) => err_result(&format!("序列化失败: {}", e)),
    }
}

/// 构造成功结果（直接字符串，非 JSON，用于 render_prompt）
fn ok_result_str(s: &str) -> *mut HippocampusResult {
    match CString::new(s) {
        Ok(cstr) => Box::into_raw(Box::new(HippocampusResult {
            is_ok: true,
            error_message: None,
            data: Some(cstr),
        })),
        Err(e) => err_result(&format!("字符串包含内部 null 字节: {}", e)),
    }
}

/// 构造错误结果
fn err_result(msg: &str) -> *mut HippocampusResult {
    Box::into_raw(Box::new(HippocampusResult {
        is_ok: false,
        error_message: CString::new(msg).ok(),
        data: None,
    }))
}

/// 从 Core Error 构造错误结果
fn err_from_core(e: hippocampus_core::Error) -> *mut HippocampusResult {
    err_result(&format!("{}", e))
}

// ============================================================================
// 句柄生命周期
// ============================================================================

/// 创建 Hippocampus 实例
///
/// 创建一个绑定到指定存储路径和会话的 Hippocampus 句柄。
/// 一个句柄对应一个会话（session_id），不可跨会话复用。
///
/// # 参数
/// - `root_path`：存储根目录路径（UTF-8 编码，null 结尾）
/// - `session_id`：会话 ID（UTF-8 编码，null 结尾）
/// - `project_id`：项目 ID（UTF-8 编码，null 结尾），可为 NULL 表示无项目隔离
///
/// # 返回
/// 成功返回句柄指针，失败返回 NULL（参数无效或 runtime 创建失败）
///
/// # Safety
/// - `root_path` 必须是有效的 C 字符串，非 NULL
/// - `session_id` 必须是有效的 C 字符串，非 NULL
/// - `project_id` 可为 NULL
#[no_mangle]
pub unsafe extern "C" fn hippocampus_new(
    root_path: *const c_char,
    session_id: *const c_char,
    project_id: *const c_char,
) -> *mut HippocampusHandle {
    if root_path.is_null() || session_id.is_null() {
        return std::ptr::null_mut();
    }

    let root = match CStr::from_ptr(root_path).to_str() {
        Ok(s) => std::path::PathBuf::from(s),
        Err(_) => return std::ptr::null_mut(),
    };
    let session = match CStr::from_ptr(session_id).to_str() {
        Ok(s) => s.to_string(),
        Err(_) => return std::ptr::null_mut(),
    };
    let project = if project_id.is_null() {
        None
    } else {
        match CStr::from_ptr(project_id).to_str() {
            Ok(s) => Some(s.to_string()),
            Err(_) => return std::ptr::null_mut(),
        }
    };

    // 创建 tokio runtime（current_thread，轻量，适合 FFI 单线程模型）
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => return std::ptr::null_mut(),
    };

    let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(root));

    Box::into_raw(Box::new(HippocampusHandle {
        storage,
        runtime,
        config: ArchiveConfig::default(),
        session_id: session,
        project_id: project,
    }))
}

/// 释放 Hippocampus 实例
///
/// # Safety
/// `handle` 必须是之前通过 [`hippocampus_new`] 创建的有效指针，或 NULL
#[no_mangle]
pub unsafe extern "C" fn hippocampus_free(handle: *mut HippocampusHandle) {
    if !handle.is_null() {
        drop(Box::from_raw(handle));
    }
}

// ============================================================================
// 结果处理
// ============================================================================

/// 检查结果是否成功
///
/// # Safety
/// `result` 必须是之前由其他 Hippocampus 操作返回的有效指针，或 NULL
#[no_mangle]
pub unsafe extern "C" fn hippocampus_is_ok(result: *const HippocampusResult) -> bool {
    if result.is_null() {
        return false;
    }
    (*result).is_ok
}

/// 获取结果中的数据字符串（调用方需用 [`hippocampus_free_string`] 释放）
///
/// 数据内容因操作而异：
/// - `archive`：返回 SummaryView JSON（钩子摘要，含 hook_id）
/// - `retrieve`：返回 MemoryFile JSON（完整记忆文件，含所有 turns）
/// - `get_summaries`：返回 SummaryView 数组 JSON
/// - `render_prompt`：返回渲染好的 prompt 文本（非 JSON）
/// - `run_compaction`：返回 CompactionResult JSON
///
/// # Safety
/// `result` 必须是之前由其他 Hippocampus 操作返回的有效指针，或 NULL
#[no_mangle]
pub unsafe extern "C" fn hippocampus_get_data(result: *const HippocampusResult) -> *mut c_char {
    if result.is_null() {
        return std::ptr::null_mut();
    }
    (*result)
        .data
        .as_ref()
        .map(|s| CString::clone(s).into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// 获取结果中的错误消息（调用方需用 [`hippocampus_free_string`] 释放）
///
/// # Safety
/// `result` 必须是之前由其他 Hippocampus 操作返回的有效指针，或 NULL
#[no_mangle]
pub unsafe extern "C" fn hippocampus_get_error(result: *const HippocampusResult) -> *mut c_char {
    if result.is_null() {
        return std::ptr::null_mut();
    }
    (*result)
        .error_message
        .as_ref()
        .map(|s| CString::clone(s).into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// 释放结果
///
/// # Safety
/// `result` 必须是之前由其他 Hippocampus 操作返回的有效指针，或 NULL。
/// 释放后不得再次使用该指针。
#[no_mangle]
pub unsafe extern "C" fn hippocampus_result_free(result: *mut HippocampusResult) {
    if !result.is_null() {
        drop(Box::from_raw(result));
    }
}

/// 释放字符串（由 [`hippocampus_get_data`] 或 [`hippocampus_get_error`] 返回）
///
/// # Safety
/// `s` 必须是由 [`hippocampus_get_data`] 或 [`hippocampus_get_error`] 返回的字符串指针，或 NULL。
/// 释放后不得再次使用该指针。
#[no_mangle]
pub unsafe extern "C" fn hippocampus_free_string(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}

// ============================================================================
// 核心操作
// ============================================================================

/// 归档上下文
///
/// 将一批轮次（turns）归档为记忆文件，生成索引钩子。
/// 内部通过 tokio runtime 的 `block_on` 执行 Core 异步归档流程。
///
/// # 参数
/// - `handle`：实例句柄
/// - `turns_json`：`MessageTurn` 数组的 JSON 字符串
///
/// # 返回
/// 成功时 data 为 SummaryView JSON（钩子摘要，含 hook_id/memory_file_id/summary_title/tags 等）
///
/// # Safety
/// - `handle` 必须是有效的非 NULL 句柄
/// - `turns_json` 必须是有效的 C 字符串（UTF-8 编码，null 结尾）
#[no_mangle]
pub unsafe extern "C" fn hippocampus_archive(
    handle: *mut HippocampusHandle,
    turns_json: *const c_char,
) -> *mut HippocampusResult {
    let handle = match handle.as_ref() {
        Some(h) => h,
        None => return err_result("handle 为 NULL"),
    };
    if turns_json.is_null() {
        return err_result("turns_json 为 NULL");
    }

    let json_str = match CStr::from_ptr(turns_json).to_str() {
        Ok(s) => s,
        Err(_) => return err_result("turns_json 不是有效的 UTF-8"),
    };

    // 解析 turns
    let turns: Vec<MessageTurn> = match serde_json::from_str(json_str) {
        Ok(t) => t,
        Err(e) => return err_result(&format!("解析 turns_json 失败: {}", e)),
    };

    if turns.is_empty() {
        return err_result("turns 数组为空");
    }

    // 构造 Archiver 并归档
    let mut archiver = Archiver::new(
        handle.config.clone(),
        handle.storage.clone(),
        handle.session_id.clone(),
        handle.project_id.clone(),
    );

    for turn in turns {
        archiver.push_turn(turn);
    }

    match handle.runtime.block_on(archiver.archive()) {
        Ok((_memory, hook)) => {
            let summary = SummaryView::from(&hook);
            ok_result(&summary)
        }
        Err(e) => err_from_core(e),
    }
}

/// 检索记忆文件（按钩子 ID）
///
/// 通过索引钩子 ID 检索对应的完整记忆文件（含所有 turns）。
///
/// # 参数
/// - `handle`：实例句柄
/// - `hook_id`：索引钩子 ID（UUID 字符串）
///
/// # 返回
/// 成功时 data 为 MemoryFile JSON（完整记忆文件）
///
/// # Safety
/// - `handle` 必须是有效的非 NULL 句柄
/// - `hook_id` 必须是有效的 C 字符串
#[no_mangle]
pub unsafe extern "C" fn hippocampus_retrieve(
    handle: *mut HippocampusHandle,
    hook_id: *const c_char,
) -> *mut HippocampusResult {
    let handle = match handle.as_ref() {
        Some(h) => h,
        None => return err_result("handle 为 NULL"),
    };
    if hook_id.is_null() {
        return err_result("hook_id 为 NULL");
    }

    let hook_id_str = match CStr::from_ptr(hook_id).to_str() {
        Ok(s) => s,
        Err(_) => return err_result("hook_id 不是有效的 UTF-8"),
    };

    let retriever = Retriever::new(
        handle.storage.clone(),
        handle.session_id.clone(),
        handle.project_id.clone(),
    );

    match handle
        .runtime
        .block_on(retriever.retrieve_memory(hook_id_str))
    {
        Ok(memory_file) => ok_result(&memory_file),
        Err(e) => err_from_core(e),
    }
}

/// 获取所有周期的摘要视图
///
/// 实时从 Storage 读取 daily/weekly/monthly 三个周期的索引文档，
/// 合并所有钩子转为摘要视图，按归档时间排序（旧→新）。
///
/// # 返回
/// 成功时 data 为 `SummaryView` 数组 JSON
///
/// # Safety
/// `handle` 必须是有效的非 NULL 句柄
#[no_mangle]
pub unsafe extern "C" fn hippocampus_get_summaries(
    handle: *mut HippocampusHandle,
) -> *mut HippocampusResult {
    let handle = match handle.as_ref() {
        Some(h) => h,
        None => return err_result("handle 为 NULL"),
    };

    let retriever = Retriever::new(
        handle.storage.clone(),
        handle.session_id.clone(),
        handle.project_id.clone(),
    );

    match handle.runtime.block_on(retriever.get_summaries()) {
        Ok(summaries) => ok_result(&summaries),
        Err(e) => err_from_core(e),
    }
}

/// 渲染摘要为 system prompt 文本
///
/// 将所有周期的摘要钩子渲染为可直接注入 LLM system prompt 的文本。
/// 格式：按周期分组（近期记忆/周度记忆/月度记忆），每个钩子一行（标题+标签+时间）。
/// 若无任何记忆，返回空字符串。
///
/// # 返回
/// 成功时 data 为渲染好的 prompt 文本（非 JSON，可直接使用）
///
/// # Safety
/// `handle` 必须是有效的非 NULL 句柄
#[no_mangle]
pub unsafe extern "C" fn hippocampus_render_prompt(
    handle: *mut HippocampusHandle,
) -> *mut HippocampusResult {
    let handle = match handle.as_ref() {
        Some(h) => h,
        None => return err_result("handle 为 NULL"),
    };

    let retriever = Retriever::new(
        handle.storage.clone(),
        handle.session_id.clone(),
        handle.project_id.clone(),
    );

    match handle
        .runtime
        .block_on(retriever.render_to_system_prompt())
    {
        Ok(prompt) => ok_result_str(&prompt),
        Err(e) => err_from_core(e),
    }
}

/// 触发周期任务（周级合并 / 月级评分淘汰）
///
/// # 参数
/// - `handle`：实例句柄
/// - `period`：0=周级合并（`COMPACTION_WEEKLY`），1=月级评分淘汰（`COMPACTION_MONTHLY`）
///
/// # 返回
/// 成功时 data 为 CompactionResult JSON（合并后的记忆文件概况）
///
/// # Safety
/// `handle` 必须是有效的非 NULL 句柄
#[no_mangle]
pub unsafe extern "C" fn hippocampus_run_compaction(
    handle: *mut HippocampusHandle,
    period: u32,
) -> *mut HippocampusResult {
    let handle = match handle.as_ref() {
        Some(h) => h,
        None => return err_result("handle 为 NULL"),
    };

    let compactor = Compactor::new(
        handle.storage.clone(),
        Box::new(DefaultScorer::new()),
        handle.session_id.clone(),
        handle.project_id.clone(),
    );

    let result = match period {
        COMPACTION_WEEKLY => handle.runtime.block_on(compactor.weekly_merge()),
        COMPACTION_MONTHLY => handle.runtime.block_on(compactor.monthly_evict()),
        _ => {
            return err_result(&format!(
                "无效的 period 值: {}（应为 0=weekly 或 1=monthly）",
                period
            ))
        }
    };

    match result {
        Ok((memory_file, index_doc)) => {
            let compaction_result = CompactionResult {
                memory_file_id: memory_file.id.to_string(),
                total_turns: memory_file.turns.len(),
                total_tokens: memory_file.total_tokens,
                hooks_count: index_doc.hooks.len(),
                period: memory_file.period.as_dir_name().to_string(),
            };
            ok_result(&compaction_result)
        }
        Err(e) => err_from_core(e),
    }
}
