//! HippocampusCore - WASM 主入口绑定
//!
//! 暴露给 JS 的核心 API：archive / list_memories / read_memory / read_index。
//! 通过静态构造方法 `with_memory_storage` / `with_js_storage` 注入 Storage 后端。
//!
//! ## JS 调用示例
//!
//! ```js
//! import { HippocampusCore, MemoryStorage } from "hippocampus_wasm";
//!
//! const core = HippocampusCore.with_memory_storage(new MemoryStorage());
//! const hookId = await core.archive("session-001", [
//!   { user_message: { text: "你好" }, llm_message: { text: "你好！" }, token_count: 10 }
//! ]);
//! const memories = await core.list_memories("session-001", "daily");
//! ```

use crate::error::error_to_js;
use hippocampus_core_logic::archive::Archiver;
use hippocampus_core_logic::model::{ArchiveConfig, ArchivePeriod, MessageTurn};
use hippocampus_core_logic::storage::Storage;
use std::sync::Arc;
use wasm_bindgen::prelude::*;

/// HippocampusCore - JS 调用 hippocampus 主入口
///
/// 持有 [`Storage`] 引用，提供 archive / list_memories / read_memory / read_index API。
/// 通过静态构造方法注入 Storage 实现（[`MemoryStorage`](crate::MemoryStorage) 或
/// [`JsStorage`](crate::JsStorage)）。
#[wasm_bindgen]
pub struct HippocampusCore {
    storage: Arc<dyn Storage>,
}

#[wasm_bindgen]
impl HippocampusCore {
    /// 用 [`MemoryStorage`](crate::MemoryStorage) 构造 HippocampusCore
    ///
    /// 纯内存存储，重启丢失。适用于 demo / 测试 / 无状态计算。
    ///
    /// JS: `HippocampusCore.with_memory_storage(new MemoryStorage())`
    #[wasm_bindgen(static_method_of = HippocampusCore)]
    pub fn with_memory_storage(storage: crate::MemoryStorage) -> HippocampusCore {
        Self {
            storage: Arc::new(storage),
        }
    }

    /// 用 [`JsStorage`](crate::JsStorage) 构造 HippocampusCore
    ///
    /// JS 调用方实现存储后端（IndexedDB / Workers KV / 远程服务）。
    ///
    /// JS: `HippocampusCore.with_js_storage(new JsStorage(callbacks))`
    #[wasm_bindgen(static_method_of = HippocampusCore)]
    pub fn with_js_storage(storage: crate::JsStorage) -> HippocampusCore {
        Self {
            storage: Arc::new(storage),
        }
    }

    /// 归档轮次，返回 hook_id（uuid 字符串）
    ///
    /// ## 参数
    /// - `session_id`：会话 ID
    /// - `turns_js`：MessageTurn 数组（JS 对象数组，字段名 snake_case）
    ///
    /// ## 返回
    /// 成功：hook_id 字符串（36 字符 uuid）
    /// 失败：JsValue 错误对象（含 code + message 字段）
    ///
    /// ## turns_js 字段结构
    /// ```js
    /// [
    ///   {
    ///     user_message: { text: "用户消息" },
    ///     llm_message: { text: "LLM 回复" },
    ///     token_count: 100,
    ///     // 以下字段可选，缺省由服务端补全
    ///     id: "uuid",           // 缺省自动生成
    ///     tags: ["Text"],       // 缺省为 ["Text"]
    ///     timestamp: "..."      // 缺省取当前时间
    ///   }
    /// ]
    /// ```
    pub async fn archive(&self, session_id: &str, turns_js: JsValue) -> Result<String, JsValue> {
        let turns: Vec<MessageTurn> = serde_wasm_bindgen::from_value(turns_js)
            .map_err(|e| JsValue::from(format!("turns 反序列化失败: {:?}", e)))?;

        let config = ArchiveConfig::default();
        // clone Arc<dyn Storage> 得到 'static 引用，供 async fn 使用
        let storage = self.storage.clone();
        let mut archiver = Archiver::new(config, storage, session_id, None);
        for turn in turns {
            archiver.push_turn(turn);
        }
        let (_memory_file, index_hook) = archiver.archive().await.map_err(error_to_js)?;
        Ok(index_hook.id.to_string())
    }

    /// 列出指定 session + period 的所有记忆 memory_id
    ///
    /// ## 参数
    /// - `session_id`：会话 ID
    /// - `period`："daily" / "weekly" / "monthly"
    ///
    /// ## 返回
    /// 成功：string[] （memory_id 数组）
    /// 失败：JsValue 错误对象
    pub async fn list_memories(&self, session_id: &str, period: &str) -> Result<JsValue, JsValue> {
        let period = parse_period(period)?;
        let storage = self.storage.clone();
        let memories = storage
            .list_memories(session_id, None, period)
            .await
            .map_err(error_to_js)?;
        serde_wasm_bindgen::to_value(&memories)
            .map_err(|e| JsValue::from(format!("序列化失败: {:?}", e)))
    }

    /// 读取记忆文件
    ///
    /// ## 参数
    /// - `memory_id`：memory_id（来自 list_memories 或 archive 返回路径）
    ///
    /// ## 返回
    /// 成功：MemoryFile 对象
    /// 失败：JsValue 错误对象
    pub async fn read_memory(&self, memory_id: &str) -> Result<JsValue, JsValue> {
        let storage = self.storage.clone();
        let file = storage
            .read_memory(memory_id)
            .await
            .map_err(error_to_js)?;
        serde_wasm_bindgen::to_value(&file)
            .map_err(|e| JsValue::from(format!("序列化失败: {:?}", e)))
    }

    /// 读取索引文档
    ///
    /// ## 参数
    /// - `session_id`：会话 ID
    /// - `period`："daily" / "weekly" / "monthly"
    ///
    /// ## 返回
    /// 成功：IndexDocument 对象 或 null（未归档）
    /// 失败：JsValue 错误对象
    pub async fn read_index(&self, session_id: &str, period: &str) -> Result<JsValue, JsValue> {
        let period = parse_period(period)?;
        let storage = self.storage.clone();
        let doc = storage
            .read_index(session_id, None, period)
            .await
            .map_err(error_to_js)?;
        serde_wasm_bindgen::to_value(&doc)
            .map_err(|e| JsValue::from(format!("序列化失败: {:?}", e)))
    }
}

/// 将 period 字符串解析为 ArchivePeriod 枚举
fn parse_period(s: &str) -> Result<ArchivePeriod, JsValue> {
    ArchivePeriod::from_str(s)
        .ok_or_else(|| JsValue::from(format!("period 必须是 daily/weekly/monthly，实际: {}", s)))
}
