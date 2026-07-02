//! # Hippocampus FFI
//!
//! C ABI 动态库，将 [`hippocampus_core`] 的核心能力暴露为 C 接口，
//! 供 Python / Node / Go / Java 等语言通过 FFI 调用。
//!
//! ## 设计原则
//!
//! - **FFI 边界统一用 JSON 字符串**：复杂数据结构（如 `Vec<Tag>`）通过
//!   JSON 序列化字符串传递，可调试优先（MVP），v2 支持 MessagePack
//! - **C ABI 稳定**：导出的函数签名遵循 C 调用约定，不暴露 Rust 特有类型
//! - **错误处理**：所有函数返回 `HippocampusError*`，调用方负责释放
//!
//! ## 基本用法（伪代码）
//!
//! ```c
//! HippocampusHandle* h = hippocampus_new("/path/to/store");
//! HippocampusResult* r = hippocampus_archive(h, json_context);
//! if (hippocampus_is_ok(r)) {
//!     char* memory_id = hippocampus_get_memory_id(r);
//!     // ...
//!     hippocampus_free_string(memory_id);
//! }
//! hippocampus_result_free(r);
//! hippocampus_free(h);
//! ```
//!
//! TODO: P4 阶段实现完整 C ABI

#![allow(clippy::missing_safety_doc)]

use hippocampus_core::model::ArchiveConfig;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;

/// Hippocampus 实例句柄（不透明指针）
///
/// 字段当前为占位（P4 阶段会被 Archiver/Storage 等填充）
#[allow(dead_code)]
pub struct HippocampusHandle {
    /// 存储根路径
    root: std::path::PathBuf,
    /// 归档配置
    config: ArchiveConfig,
}

/// 操作结果（包含错误码和返回数据）
pub struct HippocampusResult {
    is_ok: bool,
    error_message: Option<CString>,
    data: Option<CString>,
}

// ============================================================================
// 句柄生命周期
// ============================================================================

/// 创建 Hippocampus 实例
///
/// # Safety
/// `root_path` 必须是有效的 C 字符串（UTF-8 编码，以 null 结尾）
#[no_mangle]
pub unsafe extern "C" fn hippocampus_new(root_path: *const c_char) -> *mut HippocampusHandle {
    if root_path.is_null() {
        return std::ptr::null_mut();
    }
    let c_str = CStr::from_ptr(root_path);
    let root = match c_str.to_str() {
        Ok(s) => std::path::PathBuf::from(s),
        Err(_) => return std::ptr::null_mut(),
    };
    Box::into_raw(Box::new(HippocampusHandle {
        root,
        config: ArchiveConfig::default(),
    }))
}

/// 释放 Hippocampus 实例
///
/// # Safety
/// `handle` 必须是之前通过 [`hippocampus_new`] 创建的有效指针
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
#[no_mangle]
pub extern "C" fn hippocampus_is_ok(result: *const HippocampusResult) -> bool {
    if result.is_null() {
        return false;
    }
    unsafe { (*result).is_ok }
}

/// 获取结果中的数据字符串（调用方需用 [`hippocampus_free_string`] 释放）
#[no_mangle]
pub extern "C" fn hippocampus_get_data(result: *const HippocampusResult) -> *mut c_char {
    if result.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        (*result)
            .data
            .as_ref()
            .map(|s| CString::clone(s).into_raw())
            .unwrap_or(std::ptr::null_mut())
    }
}

/// 获取结果中的错误消息
#[no_mangle]
pub extern "C" fn hippocampus_get_error(result: *const HippocampusResult) -> *mut c_char {
    if result.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        (*result)
            .error_message
            .as_ref()
            .map(|s| CString::clone(s).into_raw())
            .unwrap_or(std::ptr::null_mut())
    }
}

/// 释放结果
#[no_mangle]
pub extern "C" fn hippocampus_result_free(result: *mut HippocampusResult) {
    if !result.is_null() {
        unsafe {
            drop(Box::from_raw(result));
        }
    }
}

/// 释放字符串（由 [`hippocampus_get_data`] 或 [`hippocampus_get_error`] 返回）
#[no_mangle]
pub extern "C" fn hippocampus_free_string(s: *mut c_char) {
    if !s.is_null() {
        unsafe {
            drop(CString::from_raw(s));
        }
    }
}

// ============================================================================
// 核心操作（占位，P4 阶段实现）
// ============================================================================

/// 归档上下文（JSON 字符串输入，返回记忆文件 JSON）
///
/// TODO: P4 阶段实现
#[no_mangle]
pub extern "C" fn hippocampus_archive(
    _handle: *mut HippocampusHandle,
    _context_json: *const c_char,
) -> *mut HippocampusResult {
    Box::into_raw(Box::new(HippocampusResult {
        is_ok: false,
        error_message: CString::new("hippocampus_archive() 待 P4 实现").ok(),
        data: None,
    }))
}

/// 检索记忆文件（按钩子 ID）
///
/// TODO: P4 阶段实现
#[no_mangle]
pub extern "C" fn hippocampus_retrieve(
    _handle: *mut HippocampusHandle,
    _hook_id: *const c_char,
) -> *mut HippocampusResult {
    Box::into_raw(Box::new(HippocampusResult {
        is_ok: false,
        error_message: CString::new("hippocampus_retrieve() 待 P4 实现").ok(),
        data: None,
    }))
}

/// 获取摘要视图（用于注入 system prompt）
///
/// TODO: P4 阶段实现
#[no_mangle]
pub extern "C" fn hippocampus_get_summaries(
    _handle: *mut HippocampusHandle,
) -> *mut HippocampusResult {
    Box::into_raw(Box::new(HippocampusResult {
        is_ok: false,
        error_message: CString::new("hippocampus_get_summaries() 待 P4 实现").ok(),
        data: None,
    }))
}

/// 触发周期任务（周级合并 / 月级评分淘汰）
///
/// TODO: P4 阶段实现
#[no_mangle]
pub extern "C" fn hippocampus_run_compaction(
    _handle: *mut HippocampusHandle,
    _period: u32, // 0=weekly, 1=monthly
) -> *mut HippocampusResult {
    Box::into_raw(Box::new(HippocampusResult {
        is_ok: false,
        error_message: CString::new("hippocampus_run_compaction() 待 P4 实现").ok(),
        data: None,
    }))
}
