//! # Hippocampus WASM
//!
//! WASM 绑定层：将 hippocampus-core-logic 编译为 WASM，提供 JS 调用 API。

#![forbid(unsafe_code)]

pub mod error;
pub mod memory_storage;
pub mod js_storage;
pub mod bindings;

// Task 8-10 启用
pub use memory_storage::MemoryStorage;
pub use js_storage::JsStorage;
pub use bindings::HippocampusCore;
