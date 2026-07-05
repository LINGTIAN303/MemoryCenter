//! # Hippocampus Python 绑定
//!
//! 使用 PyO3 将 [`hippocampus_core`] 的能力暴露为 Python 原生扩展模块。
//!
//! ## 架构
//!
//! - **同步 API**：内部 tokio runtime block_on（与 FFI 层一致）
//! - **OOP 风格**：`Hippocampus` 类持有句柄，方法 archive/retrieve/summaries/prompt/compaction
//! - **dict 数据类型**：Python dict 作为消息轮次的输入输出格式（通过 JSON 中间转换）
//! - **上下文管理器**：支持 `with Hippocampus(...) as hp:` 用法
//!
//! ## 使用示例
//!
//! ```python
//! from hippocampus_python import Hippocampus
//!
//! with Hippocampus("./data", "session-1", project_id="proj-a") as hp:
//!     # 归档
//!     summary = hp.archive([
//!         {"user_message": {"text": "你好"}, "llm_message": {"text": "你好！"}, ...}
//!     ])
//!     # 检索
//!     memory = hp.retrieve(summary["hook_id"])
//!     # 摘要列表
//!     summaries = hp.summaries()
//! ```

use hippocampus_core::archive::Archiver;
use hippocampus_core::compact::Compactor;
use hippocampus_core::generate::SummaryGenerator;
use hippocampus_core::model::ArchiveConfig;
use hippocampus_core::retrieve::Retriever;
use hippocampus_core::score::DefaultScorer;
use hippocampus_core::storage::{LocalStorage, Storage};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::runtime::Runtime;

// ============================================================================
// Python 模块声明
// ============================================================================

/// Hippocampus Python 扩展模块
///
/// 模块名 `hippocampus_python`（与 Cargo.toml lib.name 一致）
#[pymodule]
mod hippocampus_python {
    use super::*;

    /// 模块级函数：返回版本号
    #[pyfunction]
    fn version() -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    /// 模块级函数：返回支持的操作列表
    #[pyfunction]
    fn operations() -> Vec<&'static str> {
        vec!["archive", "retrieve", "summaries", "prompt", "compaction"]
    }

    /// 模块级函数：返回支持的 Agent family 列表（v2.21 批次 8d）
    ///
    /// 用于 with_agent() 的可选值参考（不含 Custom 兜底）。
    #[pyfunction]
    fn supported_agents() -> Vec<&'static str> {
        vec![
            "Claude Code", "Cursor", "Trae", "Codex", "Zcode", "OpenCode",
            "Qoder", "WorkBuddy", "CatPaw", "OpenClaw", "Marvis",
        ]
    }

    /// 模块级函数：返回支持的 Scenario 列表（v2.21 批次 8d）
    ///
    /// 用于 with_scenario() 的可选值参考（不含 Custom 兜底）。
    #[pyfunction]
    fn supported_scenarios() -> Vec<&'static str> {
        vec![
            "coding", "writing", "research", "daily",
            "finance", "design", "officework",
        ]
    }

    /// 模块级函数：返回支持的 ModelVariant 名称列表（v2.29）
    ///
    /// 用于 with_model() 的可选值参考。共 15 个内置型号。
    #[pyfunction]
    fn supported_models() -> Vec<String> {
        hippocampus_models::ModelRegistry::all_variants()
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// 模块级函数：返回支持的技能名称列表（v2.29）
    ///
    /// 用于 with_skill() / with_skills() 的可选值参考。共 15 个内置技能。
    /// 其他字符串将作为 Custom 技能处理。
    #[pyfunction]
    fn supported_skills() -> Vec<&'static str> {
        vec![
            "Read", "Write", "Edit", "Glob", "Grep", "LS",
            "Bash", "Task", "WebSearch", "WebFetch", "SearchCodebase",
            "AskUserQuestion", "TodoWrite", "Schedule", "Skill",
        ]
    }

    /// 模块级函数：返回支持的窗口预设名列表（v2.29）
    ///
    /// 用于 with_window() 的 scheme 参数可选值参考。
    #[pyfunction]
    fn supported_windows() -> Vec<&'static str> {
        vec![
            "claude_code", "cursor", "trae", "codex",
            "no_compression", "generic",
        ]
    }

    // 导出 Hippocampus 类
    #[pymodule_export]
    use super::Hippocampus;
    // v2.21 批次 8d：导出 PresetBuilder 类
    #[pymodule_export]
    use super::PyPresetBuilder;
}

// ============================================================================
// Hippocampus 类
// ============================================================================

/// Hippocampus 记忆库句柄
///
/// 持有存储根目录、tokio runtime、会话 ID 和项目 ID，
/// 一个实例对应一个会话（与 FFI 层 HippocampusHandle 一致）。
///
/// Python 用法：
/// ```python
/// hp = Hippocampus("./data", "session-1", project_id="proj-a")
/// summary = hp.archive(turns)
/// hp.close()  # 或用 with 上下文管理器
/// ```
#[pyclass(name = "Hippocampus")]
struct Hippocampus {
    /// 存储根目录
    storage_root: PathBuf,
    /// tokio 异步运行时（内部 block_on Core 异步方法）
    runtime: Runtime,
    /// 会话 ID
    session_id: String,
    /// 项目 ID（可选）
    project_id: Option<String>,
    /// 可选的 LLM 摘要生成器（v2.23）
    ///
    /// 由 `from_env()` 类方法从环境变量构建注入。
    /// 为 None 时 archive/compaction 使用启发式 Summary（向后兼容）。
    summary_generator: Option<Arc<dyn SummaryGenerator>>,
}

// ============================================================================
// 辅助函数
// ============================================================================

/// 将 Python 对象转为 JSON 字符串
///
/// 使用 Python 内置 json 模块的 dumps 方法
fn py_to_json_string(py: Python<'_>, obj: &Bound<'_, PyAny>) -> PyResult<String> {
    let json_mod = py.import("json")?;
    let dumps = json_mod.getattr("dumps")?;
    let result = dumps.call1((obj,))?;
    let s: String = result.extract()?;
    Ok(s)
}

/// 将 JSON 字符串转为 Python 对象
///
/// 使用 Python 内置 json 模块的 loads 方法
fn json_string_to_py<'py>(
    py: Python<'py>,
    json_str: &str,
) -> PyResult<Bound<'py, PyAny>> {
    let json_mod = py.import("json")?;
    let loads = json_mod.getattr("loads")?;
    loads.call1((json_str,))
}

/// 创建 Storage 实例
fn create_storage(root: &std::path::Path) -> Arc<dyn Storage> {
    Arc::new(LocalStorage::new(root.to_path_buf()))
}

/// 从环境变量构建 LLM 摘要生成器（v2.23）
///
/// 环境变量前缀 `HIPPOCAMPUS_GENERATOR_`，与 server/mcp 一致：
/// - `HIPPOCAMPUS_GENERATOR_API_URL`：LLM API 地址（必填，未设置则返回 None）
/// - `HIPPOCAMPUS_GENERATOR_API_KEY`：API Key（可选）
/// - `HIPPOCAMPUS_GENERATOR_MODEL`：模型名（可选，默认 gpt-4o-mini）
/// - `HIPPOCAMPUS_GENERATOR_MAX_TOKENS`：最大 token 数（可选，默认 1024）
///
/// 返回 None 时调用方使用启发式 Summary（向后兼容）。
fn build_summary_generator_from_env() -> Option<Arc<dyn SummaryGenerator>> {
    use hippocampus_core::generate::LlmGeneratorConfig;
    use hippocampus_llm::HttpSummaryGenerator;

    let config = LlmGeneratorConfig::from_env()?;
    Some(Arc::new(HttpSummaryGenerator::new(config)))
}

/// 从 Python dict 解析 preset 参数并构建 CombinedProfile（v2.29）
///
/// 与 `hippocampus-server` 的 `build_combined_from_request` 和
/// `hippocampus-mcp` 的 archive tool 复用同一套 `build_from_strings` 解析逻辑。
///
/// ## 支持的 dict 字段（全部可选）
///
/// - `agent`: str（如 "Claude Code"）
/// - `scenario`: str（如 "coding"）
/// - `model`: str（如 "claude-opus-4.8"）
/// - `archive_threshold`: int（用户覆盖归档阈值）
/// - `summary_template`: str（用户覆盖摘要模板，需含 `{conversation}`）
///
/// ## 错误
///
/// - `model` 未找到：`"未找到型号: {name}"`
/// - `summary_template` 缺少 `{conversation}`：`"summary_template 必须包含 {conversation} 占位符"`
/// - Profile 校验失败：`"预设构建失败: {err}"`
fn build_combined_from_preset(
    preset_obj: Py<PyAny>,
) -> PyResult<hippocampus_presets::CombinedProfile> {
    // 将 Python dict 转为 JSON 字符串，再反序列化为 PresetRequest 等价结构
    let json_str: String = Python::attach(|py| -> PyResult<String> {
        py_to_json_string(py, preset_obj.bind(py))
    })?;

    // 解析为中间结构（与 server 的 PresetRequest 一致）
    #[derive(serde::Deserialize)]
    struct PresetDict {
        #[serde(default)] agent: Option<String>,
        #[serde(default)] scenario: Option<String>,
        #[serde(default)] model: Option<String>,
        #[serde(default)] archive_threshold: Option<usize>,
        #[serde(default)] summary_template: Option<String>,
    }

    let req: PresetDict = serde_json::from_str(&json_str).map_err(|e| {
        PyValueError::new_err(format!("解析 preset dict 失败: {}", e))
    })?;

    // 复用 presets crate 的 build_from_strings（与 server / mcp 一致）
    hippocampus_presets::build_from_strings(
        req.agent.as_deref(),
        req.scenario.as_deref(),
        req.model.as_deref(),
        req.archive_threshold,
        req.summary_template.as_deref(),
    )
    .map_err(|e| PyValueError::new_err(format!("预设构建失败: {}", e)))
}

// ============================================================================
// Hippocampus 方法实现
// ============================================================================

#[pymethods]
impl Hippocampus {
    /// 创建新的 Hippocampus 句柄
    ///
    /// 参数：
    /// - `storage_root`：存储根目录路径
    /// - `session_id`：会话 ID
    /// - `project_id`：项目 ID（可选，默认 None）
    ///
    /// 返回：Hippocampus 实例
    ///
    /// **注意**：此构造器不注入 LLM 摘要生成器，archive/compaction 使用启发式 Summary。
    /// 若需 LLM 摘要，请使用 [`Hippocampus::from_env`] 类方法。
    #[new]
    #[pyo3(signature = (storage_root, session_id, project_id=None))]
    fn new(
        storage_root: String,
        session_id: String,
        project_id: Option<String>,
    ) -> PyResult<Self> {
        let root = PathBuf::from(&storage_root);
        // 确保存储目录存在
        std::fs::create_dir_all(&root).map_err(|e| {
            PyValueError::new_err(format!("创建存储目录失败 {}: {}", storage_root, e))
        })?;
        let runtime = Runtime::new().map_err(|e| {
            PyValueError::new_err(format!("创建 tokio runtime 失败: {}", e))
        })?;
        Ok(Self {
            storage_root: root,
            runtime,
            session_id,
            project_id,
            summary_generator: None,
        })
    }

    /// 从环境变量构建 Hippocampus 实例（v2.23）
    ///
    /// 在 [`Hippocampus::new`] 基础上，从环境变量读取 LLM 配置并注入摘要生成器。
    /// archive/compaction 将使用 LLM 生成的结构化摘要（失败时降级为启发式）。
    ///
    /// ## 环境变量（前缀 `HIPPOCAMPUS_GENERATOR_`）
    ///
    /// - `HIPPOCAMPUS_GENERATOR_API_URL`：LLM API 地址（必填，未设置则不注入 LLM）
    /// - `HIPPOCAMPUS_GENERATOR_API_KEY`：API Key（可选）
    /// - `HIPPOCAMPUS_GENERATOR_MODEL`：模型名（可选，默认 gpt-4o-mini）
    /// - `HIPPOCAMPUS_GENERATOR_MAX_TOKENS`：最大 token 数（可选，默认 1024）
    ///
    /// ## 参数
    ///
    /// - `storage_root`：存储根目录路径
    /// - `session_id`：会话 ID
    /// - `project_id`：项目 ID（可选，默认 None）
    ///
    /// ## 返回
    ///
    /// Hippocampus 实例（含 LLM 摘要生成器，若环境变量未配置则降级为启发式）
    ///
    /// ## Python 用法
    ///
    /// ```python
    /// import os
    /// os.environ["HIPPOCAMPUS_GENERATOR_API_URL"] = "https://api.openai.com/v1"
    /// os.environ["HIPPOCAMPUS_GENERATOR_API_KEY"] = "sk-xxx"
    /// os.environ["HIPPOCAMPUS_GENERATOR_MODEL"] = "gpt-4o-mini"
    ///
    /// from hippocampus_python import Hippocampus
    /// hp = Hippocampus.from_env("./data", "session-1")
    /// summary = hp.archive(turns)  # 使用 LLM 生成的结构化摘要
    /// ```
    #[classmethod]
    #[pyo3(signature = (storage_root, session_id, project_id=None))]
    fn from_env(
        _cls: &Bound<'_, pyo3::types::PyType>,
        storage_root: String,
        session_id: String,
        project_id: Option<String>,
    ) -> PyResult<Self> {
        // 先调用普通构造器创建实例
        let instance = Self::new(storage_root, session_id, project_id)?;
        // 从环境变量构建 summary_generator
        let summary_generator = build_summary_generator_from_env();
        // 注入到实例
        Ok(Self {
            summary_generator,
            ..instance
        })
    }

    /// 上下文管理器：进入
    fn __enter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    /// 上下文管理器：退出（自动释放 runtime）
    fn __exit__(
        &mut self,
        _exc_type: &Bound<'_, PyAny>,
        _exc_value: &Bound<'_, PyAny>,
        _traceback: &Bound<'_, PyAny>,
    ) -> PyResult<bool> {
        // runtime 会在 drop 时自动释放，无需特殊处理
        Ok(false) // 不抑制异常
    }

    /// 友好的字符串表示
    fn __repr__(&self) -> String {
        format!(
            "Hippocampus(storage_root={:?}, session_id={:?}, project_id={:?})",
            self.storage_root, self.session_id, self.project_id
        )
    }

    /// 归档一批轮次为记忆文件
    ///
    /// 参数：
    /// - `turns`：消息轮次列表（list[dict]，每个 dict 符合 MessageTurn 结构）
    /// - `preset`：预设配置 dict（v2.29 新增，可选），含字段：
    ///   - `agent`: str（如 "Claude Code"）
    ///   - `scenario`: str（如 "coding"）
    ///   - `model`: str（如 "claude-opus-4.8"）
    ///   - `archive_threshold`: int（用户覆盖归档阈值）
    ///   - `summary_template`: str（用户覆盖摘要模板，需含 `{conversation}`）
    ///
    /// 返回：摘要视图 dict（含 hook_id/memory_file_id/summary_title/tags/archived_at/period/token_count）
    ///
    /// ## preset 行为
    ///
    /// - 未传入 `preset`：保持原行为（`ArchiveConfig::default()` + 内部默认模板）
    /// - 传入 `preset`：服务端即时构建 CombinedProfile，应用：
    ///   - `archive_threshold` 覆盖 `ArchiveConfig.token_threshold`
    ///   - `summary_template` 通过 `with_summary_template_override` 注入 Archiver
    ///
    /// ## turn 结构示例
    ///
    /// ```python
    /// {
    ///     "id": "uuid-string",  # 可选，不传会自动生成
    ///     "user_message": {"text": "...", "attachments": [], "tool_calls": [], "thinking": null},
    ///     "llm_message": {"text": "...", "attachments": [], "tool_calls": [], "thinking": null},
    ///     "tags": [{"kind": "Text"}],  # 17 类标签
    ///     "timestamp": "2026-07-02T12:00:00Z",  # 可选
    ///     "token_count": 100
    /// }
    /// ```
    #[pyo3(signature = (turns, preset=None))]
    fn archive(
        &self,
        turns: Vec<Py<PyAny>>,
        preset: Option<Py<PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        if turns.is_empty() {
            return Err(PyValueError::new_err("turns 不能为空"));
        }

        // 1. 将 Python dict 列表转为 JSON 字符串数组
        let json_str: String = Python::attach(|py| -> PyResult<String> {
            let json_strings: PyResult<Vec<String>> = turns
                .iter()
                .map(|t| py_to_json_string(py, t.bind(py)))
                .collect();
            let json_strings = json_strings?;
            // 拼接成 JSON 数组
            Ok(format!("[{}]", json_strings.join(",")))
        })?;

        // 2. 反序列化为 Vec<MessageTurn>
        let message_turns: Vec<hippocampus_core::model::MessageTurn> =
            serde_json::from_str(&json_str).map_err(|e| {
                PyValueError::new_err(format!("解析 turns 失败: {}", e))
            })?;

        // 3. v2.29：解析可选 preset，构建 CombinedProfile 提取 archive_threshold + summary_template
        let (archive_threshold, summary_template) = if let Some(preset_obj) = preset {
            let combined = build_combined_from_preset(preset_obj)?;
            (
                Some(combined.archive_threshold()),
                Some(combined.summary_template().to_string()),
            )
        } else {
            (None, None)
        };

        // 4. 调用 Core archive
        let storage = create_storage(&self.storage_root);
        let config = if let Some(threshold) = archive_threshold {
            // 与 server / mcp 一致：force_truncate_limit 按 3/2 比例放大
            ArchiveConfig {
                token_threshold: threshold,
                force_truncate_limit: threshold * 3 / 2,
                wait_for_turn_completion: true,
            }
        } else {
            ArchiveConfig::default()
        };
        let mut archiver = Archiver::new(
            config,
            storage,
            &self.session_id,
            self.project_id.clone(),
        );

        // v2.23: 若注入了 summary_generator，注入到 Archiver
        if let Some(gen) = &self.summary_generator {
            archiver = archiver.with_summary_generator(gen.clone());
        }

        // v2.29: 若 preset 提供了 summary_template，注入到 Archiver
        if let Some(tpl) = summary_template {
            archiver = archiver.with_summary_template_override(tpl);
        }

        for turn in message_turns {
            archiver.push_turn(turn);
        }

        let (_, hook) = self
            .runtime
            .block_on(async { archiver.archive().await })
            .map_err(|e| PyValueError::new_err(format!("归档失败: {}", e)))?;

        // 5. 将 SummaryView 转为 Python dict
        let summary = hippocampus_core::retrieve::SummaryView::from(&hook);
        let summary_json = serde_json::to_string(&summary)
            .map_err(|e| PyValueError::new_err(format!("序列化摘要失败: {}", e)))?;

        Python::attach(|py| json_string_to_py(py, &summary_json).map(|b| b.into()))
    }

    /// 按钩子 ID 检索完整记忆文件
    ///
    /// 参数：
    /// - `hook_id`：钩子 ID（字符串）
    ///
    /// 返回：完整记忆文件 dict（含 turns 列表、session_id、project_id 等）
    fn retrieve(&self, hook_id: String) -> PyResult<Py<PyAny>> {
        let storage = create_storage(&self.storage_root);
        let retriever = Retriever::new(storage, &self.session_id, self.project_id.clone());

        let memory = self
            .runtime
            .block_on(async { retriever.retrieve_memory(&hook_id).await })
            .map_err(|e| PyValueError::new_err(format!("检索失败: {}", e)))?;

        let memory_json = serde_json::to_string(&memory)
            .map_err(|e| PyValueError::new_err(format!("序列化记忆失败: {}", e)))?;

        Python::attach(|py| json_string_to_py(py, &memory_json).map(|b| b.into()))
    }

    /// 获取所有周期的摘要视图列表
    ///
    /// 返回：摘要视图列表 list[dict]
    fn summaries(&self) -> PyResult<Vec<Py<PyAny>>> {
        let storage = create_storage(&self.storage_root);
        let retriever = Retriever::new(storage, &self.session_id, self.project_id.clone());

        let summaries = self
            .runtime
            .block_on(async { retriever.get_summaries().await })
            .map_err(|e| PyValueError::new_err(format!("获取摘要失败: {}", e)))?;

        let summaries_json = serde_json::to_string(&summaries)
            .map_err(|e| PyValueError::new_err(format!("序列化摘要失败: {}", e)))?;

        Python::attach(|py| {
            let arr = json_string_to_py(py, &summaries_json)?;
            // 转为 Vec<Py<PyAny>>
            let list: Bound<'_, pyo3::types::PyList> = arr.extract()?;
            list.iter().map(|item| Ok(item.into())).collect()
        })
    }

    /// 渲染摘要为 system prompt 文本
    ///
    /// 返回：prompt 字符串（可直接注入 system prompt）
    fn prompt(&self) -> PyResult<String> {
        let storage = create_storage(&self.storage_root);
        let retriever = Retriever::new(storage, &self.session_id, self.project_id.clone());

        let prompt = self
            .runtime
            .block_on(async { retriever.render_to_system_prompt().await })
            .map_err(|e| PyValueError::new_err(format!("渲染 prompt 失败: {}", e)))?;

        Ok(prompt)
    }

    /// 触发周期任务（周级合并 / 月级评分淘汰）
    ///
    /// 参数：
    /// - `period`：周期类型字符串 "weekly" 或 "monthly"
    ///
    /// 返回：精简结果 dict（memory_file_id/total_turns/total_tokens/hooks_count/period）
    fn compaction(&self, period: String) -> PyResult<Py<PyAny>> {
        let storage = create_storage(&self.storage_root);
        let mut compactor = Compactor::new(
            storage,
            Box::new(DefaultScorer::new()),
            &self.session_id,
            self.project_id.clone(),
        );

        // v2.23: 若注入了 summary_generator，注入到 Compactor
        if let Some(gen) = &self.summary_generator {
            compactor = compactor.with_summary_generator(gen.clone());
        }

        let (memory, index_doc) = self
            .runtime
            .block_on(async {
                match period.as_str() {
                    "weekly" => compactor.weekly_merge().await,
                    "monthly" => compactor.monthly_evict().await,
                    other => Err(hippocampus_core::Error::Storage(format!(
                        "无效的 period 值: {}（支持: weekly, monthly）",
                        other
                    ))),
                }
            })
            .map_err(|e| PyValueError::new_err(format!("周期任务失败: {}", e)))?;

        // 构造结果（与 HTTP API 一致的精简结构）
        let result = serde_json::json!({
            "memory_file_id": memory.id.to_string(),
            "total_turns": memory.turns.len(),
            "total_tokens": memory.total_tokens,
            "hooks_count": index_doc.hooks.len(),
            "period": period,
        });
        let result_json = result.to_string();

        Python::attach(|py| json_string_to_py(py, &result_json).map(|b| b.into()))
    }

    /// 显式关闭（释放 runtime）
    ///
    /// 使用 with 上下文管理器时可自动调用
    fn close(&mut self) {
        // runtime 会在 drop 时自动释放，这里无需特殊处理
        // 保留方法供显式调用（API 兼容性）
    }
}

// ============================================================================
// PresetBuilder 类（v2.21 批次 8d）
// ============================================================================

/// 字符串解析为 WindowProfile 预设（v2.29）
///
/// 支持的预设名（大小写不敏感）：
/// - `claude_code` / `claude-code`：Claude Code /compact（180K 触发）
/// - `cursor`：Cursor Chat 压缩（150K 触发）
/// - `trae`：Trae 对话压缩（120K 触发）
/// - `codex`：Codex 滚动窗口（100K 触发）
/// - `no_compression` / `none`：无压缩
/// - `generic` / `sliding`：通用滑动窗口（默认 100K）
///
/// 未匹配的字符串返回 None。
fn window_scheme_from_str(s: &str) -> Option<hippocampus_windows::WindowProfile> {
    use hippocampus_windows::WindowProfile;
    let lower = s.to_lowercase();
    match lower.as_str() {
        "claude_code" | "claude-code" | "claudecode" => Some(WindowProfile::claude_code()),
        "cursor" => Some(WindowProfile::cursor()),
        "trae" => Some(WindowProfile::trae()),
        "codex" => Some(WindowProfile::codex()),
        "no_compression" | "none" | "nocompression" => Some(WindowProfile::no_compression()),
        "generic" | "sliding" | "generic_sliding" => Some(WindowProfile::default()),
        _ => None,
    }
}

/// 预设构造器
///
/// 链式收集 5 个可选 Profile + 用户覆盖参数，build() 后返回最终配置 dict。
///
/// Python 用法：
/// ```python
/// from hippocampus_python import PresetBuilder
///
/// preset = (PresetBuilder()
///     .with_agent("Claude Code")
///     .with_scenario("coding")
///     .with_user_archive_threshold(450_000)
///     .build())
///
/// print(preset["archive_threshold"])    # 450000
/// print(preset["session_prefix"])       # "claude-code"
/// print(preset["archive_to_hippocampus"])  # True
/// ```
#[pyclass(name = "PresetBuilder")]
struct PyPresetBuilder {
    inner: hippocampus_presets::PresetBuilder,
}

#[pymethods]
impl PyPresetBuilder {
    /// 创建空的构造器
    #[new]
    fn new() -> Self {
        Self {
            inner: hippocampus_presets::PresetBuilder::new(),
        }
    }

    /// 设置 Agent（字符串，对应 AgentFamily::display_name）
    ///
    /// 支持的值（大小写敏感，与 display_name 一致）：
    /// "Claude Code" / "Cursor" / "Trae" / "Codex" / "Zcode" / "OpenCode"
    /// / "Qoder" / "WorkBuddy" / "CatPaw" / "OpenClaw" / "Marvis"
    ///
    /// 其他字符串将作为 Custom Agent 处理。
    ///
    /// 设置后若未显式 with_window，会触发联动推导 Window。
    fn with_agent(&mut self, agent: String) {
        let family = hippocampus_agents::AgentFamily::from_str(&agent)
            .unwrap_or_else(|| hippocampus_agents::AgentFamily::Custom(agent.clone()));
        let profile = hippocampus_agents::AgentProfile::from_family(family);
        self.inner = self.inner.clone().with_agent(profile);
    }

    /// 设置场景（字符串，大小写不敏感）
    ///
    /// 支持的值：coding/writing/research/daily/finance/design/officework
    /// 其他字符串将作为 Custom 场景处理。
    fn with_scenario(&mut self, scenario: String) {
        // v2.29：复用 presets crate 的 scenario_from_str（与 server / mcp 一致）
        let sc = hippocampus_presets::scenario_from_str(&scenario);
        let profile = hippocampus_scenarios::ScenarioProfile::from_scenario(sc);
        self.inner = self.inner.clone().with_scenario(profile);
    }

    /// 用户覆盖：归档阈值（token 数，最高优先级）
    ///
    /// 优先级：用户 > scenario > model > 默认 400K
    fn with_user_archive_threshold(&mut self, threshold: usize) {
        self.inner = self.inner.clone().with_user_archive_threshold(threshold);
    }

    /// 用户覆盖：摘要模板（最高优先级）
    ///
    /// 模板需包含 `{conversation}` 占位符。
    fn with_user_summary_template(&mut self, template: String) {
        self.inner = self.inner.clone().with_user_summary_template(template);
    }

    /// 设置模型型号（v2.29 补齐）
    ///
    /// ## 参数
    ///
    /// - `model`：ModelVariant 名称（如 "claude-opus-4.8" / "gpt-5.2" / "gemini-3.1-pro"）
    ///
    /// ## 错误
    ///
    /// 未找到型号时抛出 ValueError（用 `supported_models()` 查询可用值）。
    ///
    /// ## 影响
    ///
    /// 设置后归档阈值会从 `model.archive_strategy.threshold()` 推导
    /// （优先级低于 scenario 和用户覆盖）。
    fn with_model(&mut self, model: String) -> PyResult<()> {
        let variant = hippocampus_models::ModelRegistry::find(&model).ok_or_else(|| {
            PyValueError::new_err(format!(
                "未找到型号: {}（用 hippocampus_python.supported_models() 查询可用值）",
                model
            ))
        })?;
        self.inner = self.inner.clone().with_model(variant);
        Ok(())
    }

    /// 设置窗口配置（v2.29 补齐）
    ///
    /// ## 参数
    ///
    /// - `scheme`：窗口预设名（大小写不敏感），支持：
    ///   - `"claude_code"` / `"claude-code"`：Claude Code /compact（180K 触发）
    ///   - `"cursor"`：Cursor Chat 压缩（150K 触发）
    ///   - `"trae"`：Trae 对话压缩（120K 触发）
    ///   - `"codex"`：Codex 滚动窗口（100K 触发）
    ///   - `"no_compression"` / `"none"`：无压缩
    ///   - `"generic"` / `"sliding"`：通用滑动窗口（默认 100K）
    /// - `trigger_threshold`：可选，覆盖触发阈值（token 数）
    ///
    /// ## 错误
    ///
    /// 未匹配预设时抛出 ValueError。
    ///
    /// ## 注意
    ///
    /// 显式调用此方法后不触发 Agent → Window 联动推导。
    fn with_window(
        &mut self,
        scheme: String,
        trigger_threshold: Option<usize>,
    ) -> PyResult<()> {
        let mut profile = window_scheme_from_str(&scheme).ok_or_else(|| {
            PyValueError::new_err(format!(
                "未匹配窗口预设: {}（支持: claude_code / cursor / trae / codex / no_compression / generic）",
                scheme
            ))
        })?;
        if let Some(threshold) = trigger_threshold {
            profile = profile.with_trigger_threshold(threshold);
        }
        self.inner = self.inner.clone().with_window(profile);
        Ok(())
    }

    /// 添加单个技能（v2.29 补齐，可链式调用多次）
    ///
    /// ## 参数
    ///
    /// - `skill`：技能名（大小写敏感，与 BuiltinSkill 枚举名一致）
    ///   支持：Read / Write / Edit / Glob / Grep / LS / Bash / Task /
    ///   WebSearch / WebFetch / SearchCodebase / AskUserQuestion /
    ///   TodoWrite / Schedule / Skill
    ///   其他字符串将作为 Custom 技能处理。
    ///
    /// ## 用法
    ///
    /// ```python
    /// builder = (PresetBuilder()
    ///     .with_skill("Read")
    ///     .with_skill("Bash"))
    /// ```
    fn with_skill(&mut self, skill: String) {
        let builtin = hippocampus_skills::BuiltinSkill::from_str(&skill)
            .unwrap_or_else(|| hippocampus_skills::BuiltinSkill::Custom(skill.clone()));
        let profile = hippocampus_skills::SkillProfile::new(builtin);
        self.inner = self.inner.clone().with_skill(profile);
    }

    /// 批量设置技能（v2.29 补齐，覆盖原有列表）
    ///
    /// ## 参数
    ///
    /// - `skills`：技能名列表（每个元素与 `with_skill` 一致）
    ///
    /// ## 用法
    ///
    /// ```python
    /// builder = PresetBuilder().with_skills(["Read", "Write", "Bash"])
    /// ```
    fn with_skills(&mut self, skills: Vec<String>) {
        let profiles: Vec<hippocampus_skills::SkillProfile> = skills
            .into_iter()
            .map(|s| {
                let builtin = hippocampus_skills::BuiltinSkill::from_str(&s)
                    .unwrap_or_else(|| hippocampus_skills::BuiltinSkill::Custom(s.clone()));
                hippocampus_skills::SkillProfile::new(builtin)
            })
            .collect();
        self.inner = self.inner.clone().with_skills(profiles);
    }

    /// 构建最终配置
    ///
    /// 返回 dict，含字段：
    /// - archive_threshold: int（归档阈值，token 数）
    /// - summary_template: str（摘要模板，含 {conversation} 占位符）
    /// - session_prefix: str | None（session ID 前缀，来自 Agent）
    /// - archive_to_hippocampus: bool（是否归档到 Hippocampus）
    /// - has_agent: bool（是否设置了 Agent）
    /// - has_scenario: bool（是否设置了 Scenario）
    /// - has_window: bool（是否设置了 Window，含联动推导）
    /// - has_model: bool（是否设置了 Model）
    /// - skills_count: int（技能数量）
    /// - model_name: str | None（v2.29 新增，模型名称，未设置时为 None）
    /// - window_scheme: str | None（v2.29 新增，窗口方案显示名，未设置时为 None）
    /// - window_trigger_threshold: int | None（v2.29 新增，窗口触发阈值）
    /// - skills: list[str]（v2.29 新增，技能显示名列表）
    ///
    /// 失败时抛出 ValueError（Profile 校验失败）。
    fn build(&self) -> PyResult<Py<PyAny>> {
        let combined = self.inner.clone().build().map_err(|e| {
            PyValueError::new_err(format!("PresetBuilder 构建失败: {}", e))
        })?;

        // v2.29：补充 model_name / window_scheme / window_trigger_threshold / skills 详情
        let model_name = combined.model.as_ref().map(|m| m.name.clone());
        let (window_scheme, window_trigger_threshold): (Option<String>, Option<usize>) =
            combined
                .window
                .as_ref()
                .map(|w| {
                    (
                        Some(w.scheme.display_name().to_string()),
                        Some(w.trigger_threshold),
                    )
                })
                .unwrap_or((None, None));
        let skills: Vec<&str> = combined
            .skills
            .iter()
            .map(|s| s.skill.display_name())
            .collect();

        let result = serde_json::json!({
            // 解析后的最终生效值
            "archive_threshold": combined.archive_threshold(),
            "summary_template": combined.summary_template(),
            "session_prefix": combined.session_prefix(),
            "archive_to_hippocampus": combined.archive_to_hippocampus(),
            // 标志位
            "has_agent": combined.agent.is_some(),
            "has_scenario": combined.scenario.is_some(),
            "has_window": combined.window.is_some(),
            "has_model": combined.model.is_some(),
            "skills_count": combined.skills.len(),
            // v2.29 新增详情字段
            "model_name": model_name,
            "window_scheme": window_scheme,
            "window_trigger_threshold": window_trigger_threshold,
            "skills": skills,
        });
        let result_json = result.to_string();

        Python::attach(|py| json_string_to_py(py, &result_json).map(|b| b.into()))
    }

    /// 友好的字符串表示
    fn __repr__(&self) -> String {
        "PresetBuilder()".to_string()
    }
}

// ============================================================================
// 单元测试（v2.21 批次 8d）
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 scenario_from_str 大小写不敏感解析
    ///
    /// v2.29：本地 scenario_from_str 已删除，改用 hippocampus_presets::scenario_from_str
    #[test]
    fn test_scenario_from_str_case_insensitive() {
        use hippocampus_scenarios::Scenario;

        assert_eq!(hippocampus_presets::scenario_from_str("coding"), Scenario::Coding);
        assert_eq!(hippocampus_presets::scenario_from_str("Coding"), Scenario::Coding);
        assert_eq!(hippocampus_presets::scenario_from_str("CODING"), Scenario::Coding);
        assert_eq!(hippocampus_presets::scenario_from_str("writing"), Scenario::Writing);
        assert_eq!(hippocampus_presets::scenario_from_str("research"), Scenario::Research);
        assert_eq!(hippocampus_presets::scenario_from_str("daily"), Scenario::Daily);
        assert_eq!(hippocampus_presets::scenario_from_str("finance"), Scenario::Finance);
        assert_eq!(hippocampus_presets::scenario_from_str("design"), Scenario::Design);
    }

    /// 验证 OfficeWork 别名（office/work/officework）
    #[test]
    fn test_scenario_from_str_office_aliases() {
        use hippocampus_scenarios::Scenario;

        assert_eq!(hippocampus_presets::scenario_from_str("officework"), Scenario::OfficeWork);
        assert_eq!(hippocampus_presets::scenario_from_str("office"), Scenario::OfficeWork);
        assert_eq!(hippocampus_presets::scenario_from_str("work"), Scenario::OfficeWork);
        assert_eq!(hippocampus_presets::scenario_from_str("Office"), Scenario::OfficeWork);
        assert_eq!(hippocampus_presets::scenario_from_str("WORK"), Scenario::OfficeWork);
    }

    /// 验证未知字符串回退到 Custom
    #[test]
    fn test_scenario_from_str_custom_fallback() {
        use hippocampus_scenarios::Scenario;

        match hippocampus_presets::scenario_from_str("游戏场景") {
            Scenario::Custom(s) => assert_eq!(s, "游戏场景"),
            other => panic!("期望 Custom，实际 {:?}", other),
        }

        match hippocampus_presets::scenario_from_str("unknown") {
            Scenario::Custom(s) => assert_eq!(s, "unknown"),
            other => panic!("期望 Custom，实际 {:?}", other),
        }
    }

    /// 验证 supported_agents 返回 11 个内置 Agent
    #[test]
    fn test_supported_agents_count() {
        // 模块函数在 Rust 侧无法直接调用（#[pyfunction]），但可通过 AgentFamily::all_builtin 验证
        assert_eq!(hippocampus_agents::AgentFamily::all_builtin().len(), 11);
    }

    /// 验证 supported_scenarios 返回 7 个内置场景
    #[test]
    fn test_supported_scenarios_count() {
        assert_eq!(hippocampus_scenarios::Scenario::all_builtin().len(), 7);
    }

    /// v2.29：验证 supported_models 通过 ModelRegistry 返回 15 个型号
    #[test]
    fn test_supported_models_count() {
        let count = hippocampus_models::ModelRegistry::all_variants().count();
        assert_eq!(count, 15, "应内置 15 个型号");
    }

    /// v2.29：验证 supported_skills 列表长度（15 个内置技能）
    #[test]
    fn test_supported_skills_count() {
        assert_eq!(hippocampus_skills::BuiltinSkill::all_builtin().len(), 15);
    }

    /// v2.29：验证 window_scheme_from_str 解析 6 种预设
    #[test]
    fn test_window_scheme_from_str_all_presets() {
        // claude_code / claude-code / claudecode 三种写法
        assert!(window_scheme_from_str("claude_code").is_some());
        assert!(window_scheme_from_str("claude-code").is_some());
        assert!(window_scheme_from_str("ClaudeCode").is_some()); // 大小写不敏感

        assert!(window_scheme_from_str("cursor").is_some());
        assert!(window_scheme_from_str("trae").is_some());
        assert!(window_scheme_from_str("codex").is_some());
        assert!(window_scheme_from_str("no_compression").is_some());
        assert!(window_scheme_from_str("none").is_some());
        assert!(window_scheme_from_str("generic").is_some());
        assert!(window_scheme_from_str("sliding").is_some());

        // 未匹配
        assert!(window_scheme_from_str("unknown").is_none());
        assert!(window_scheme_from_str("").is_none());
    }

    /// v2.29：验证 window_scheme_from_str 返回的 profile 阈值正确
    #[test]
    fn test_window_scheme_from_str_thresholds() {
        let cc = window_scheme_from_str("claude_code").unwrap();
        assert_eq!(cc.trigger_threshold, 180_000);

        let cursor = window_scheme_from_str("cursor").unwrap();
        assert_eq!(cursor.trigger_threshold, 150_000);

        let trae = window_scheme_from_str("trae").unwrap();
        assert_eq!(trae.trigger_threshold, 120_000);

        let codex = window_scheme_from_str("codex").unwrap();
        assert_eq!(codex.trigger_threshold, 100_000);
    }

    /// v2.29：验证 PresetBuilder.with_model 链路（Rust 侧直接调用 inner）
    #[test]
    fn test_preset_builder_with_model_rust_side() {
        let variant = hippocampus_models::ModelRegistry::find("claude-opus-4.8")
            .expect("claude-opus-4.8 应存在");
        let combined = hippocampus_presets::PresetBuilder::new()
            .with_model(variant)
            .build()
            .expect("build 失败");

        assert!(combined.model.is_some());
        assert_eq!(combined.model.as_ref().unwrap().name, "claude-opus-4.8");
    }

    /// v2.29：验证 PresetBuilder.with_window 链路（显式设置不触发 Agent 联动）
    #[test]
    fn test_preset_builder_with_window_rust_side() {
        let window = window_scheme_from_str("cursor").unwrap();
        let combined = hippocampus_presets::PresetBuilder::new()
            .with_window(window)
            .build()
            .expect("build 失败");

        assert!(combined.window.is_some());
        assert_eq!(combined.window.as_ref().unwrap().trigger_threshold, 150_000);
    }

    /// v2.29：验证 PresetBuilder.with_skill / with_skills 链路
    #[test]
    fn test_preset_builder_with_skills_rust_side() {
        use hippocampus_skills::{BuiltinSkill, SkillProfile};

        let combined = hippocampus_presets::PresetBuilder::new()
            .with_skill(SkillProfile::new(BuiltinSkill::Read))
            .with_skill(SkillProfile::new(BuiltinSkill::Bash))
            .build()
            .expect("build 失败");

        assert_eq!(combined.skills.len(), 2);

        // with_skills 覆盖
        let batch = vec![
            SkillProfile::new(BuiltinSkill::Write),
            SkillProfile::new(BuiltinSkill::Edit),
            SkillProfile::new(BuiltinSkill::Glob),
        ];
        let combined2 = hippocampus_presets::PresetBuilder::new()
            .with_skills(batch)
            .build()
            .expect("build 失败");

        assert_eq!(combined2.skills.len(), 3);
    }

    /// v2.29：验证 build_from_strings 与 Hippocampus.archive(preset=...) 等价路径
    ///
    /// 此测试直接调用 Rust 侧的 build_from_strings，验证 preset dict 解析逻辑
    /// 与 server / mcp 一致。
    #[test]
    fn test_build_from_strings_full_preset() {
        let combined = hippocampus_presets::build_from_strings(
            Some("Claude Code"),
            Some("coding"),
            Some("claude-opus-4.8"),
            Some(450_000),
            Some("custom template {conversation}"),
        )
        .expect("build_from_strings 失败");

        assert_eq!(combined.archive_threshold(), 450_000); // 用户覆盖优先
        assert_eq!(combined.summary_template(), "custom template {conversation}");
        assert_eq!(combined.session_prefix(), Some("claude-code"));
        assert!(combined.archive_to_hippocampus());
        assert!(combined.agent.is_some());
        assert!(combined.scenario.is_some());
        assert!(combined.window.is_some()); // Agent 联动推导
        assert!(combined.model.is_some());
    }

    /// v2.29：验证 build_from_strings 错误路径
    #[test]
    fn test_build_from_strings_errors() {
        // 未找到型号
        let err = hippocampus_presets::build_from_strings(
            None, None, Some("nonexistent-model"), None, None
        ).unwrap_err();
        assert!(err.contains("未找到型号"), "实际: {}", err);

        // summary_template 缺少 {conversation}
        let err = hippocampus_presets::build_from_strings(
            None, None, None, None, Some("no placeholder")
        ).unwrap_err();
        assert!(err.contains("{conversation}"), "实际: {}", err);
    }

    /// v2.29：验证 archive with preset 在 Rust 侧的应用链路
    ///
    /// 此测试直接调用 Archiver（不经过 PyO3），验证 archive_threshold + summary_template_override
    /// 能正确应用。Python 侧通过 build_combined_from_preset + Archiver 桥接，逻辑等价。
    #[tokio::test]
    async fn test_archive_with_preset_threshold_applied() {
        use hippocampus_core::model::{MessageContent, MessageTurn, Tag};
        use tempfile::TempDir;
        use chrono::Utc;
        use uuid::Uuid;

        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));

        // 模拟 preset 解析结果：scenario=coding → archive_threshold=500_000
        let combined = hippocampus_presets::build_from_strings(
            None,
            Some("coding"),
            None,
            None,
            None,
        ).unwrap();

        let threshold = combined.archive_threshold();
        assert_eq!(threshold, 500_000); // Coding 场景默认 500K

        // 构造 ArchiveConfig（与 Hippocampus.archive 等价逻辑）
        let config = ArchiveConfig {
            token_threshold: threshold,
            force_truncate_limit: threshold * 3 / 2,
            wait_for_turn_completion: true,
        };

        let mut archiver = Archiver::new(
            config,
            storage,
            "sess-py-preset-test",
            None,
        )
        .with_summary_template_override(combined.summary_template().to_string());

        archiver.push_turn(MessageTurn {
            id: Uuid::new_v4(),
            user_message: MessageContent {
                text: Some("test".into()),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                thinking: None,
            },
            llm_message: MessageContent {
                text: Some("reply".into()),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                thinking: None,
            },
            tags: vec![Tag::Text],
            timestamp: Utc::now(),
            token_count: 10,
        });

        let (_, hook) = archiver.archive().await.expect("归档失败");

        // 验证归档成功（hook 有内容）
        assert!(!hook.summary.title.is_empty());
    }

    /// 验证 PresetBuilder 链式构造（不经过 PyO3，直接调用 Rust inner）
    #[test]
    fn test_preset_builder_rust_side() {
        // 验证 Rust 侧 PresetBuilder 能正常工作（Python 侧由 PyO3 桥接）
        let builder = hippocampus_presets::PresetBuilder::new()
            .with_agent(hippocampus_agents::AgentProfile::claude_code())
            .with_scenario(hippocampus_scenarios::ScenarioProfile::from_scenario(
                hippocampus_scenarios::Scenario::Coding,
            ))
            .with_user_archive_threshold(450_000);

        let combined = builder.build().expect("PresetBuilder 构建失败");

        assert_eq!(combined.archive_threshold(), 450_000); // 用户覆盖优先
        assert_eq!(combined.session_prefix(), Some("claude-code"));
        assert!(combined.archive_to_hippocampus());
        assert!(combined.agent.is_some());
        assert!(combined.scenario.is_some());
    }

    /// 验证空 PresetBuilder 也能构建（全 None，使用默认值）
    #[test]
    fn test_preset_builder_empty_uses_defaults() {
        let combined = hippocampus_presets::PresetBuilder::new()
            .build()
            .expect("空 PresetBuilder 构建失败");

        assert_eq!(combined.archive_threshold(), 400_000); // 默认阈值
        assert!(combined.agent.is_none());
        assert!(combined.scenario.is_none());
        assert!(combined.window.is_none());
        assert!(combined.model.is_none());
        assert_eq!(combined.skills.len(), 0);
    }

    // ========================================================================
    // v2.23: from_env + summary_generator 注入测试
    // ========================================================================

    /// 验证 build_summary_generator_from_env 环境变量驱动行为
    ///
    /// 合并为单个测试避免并行竞争（std::env 是进程级全局状态）。
    #[test]
    fn test_build_summary_generator_from_env() {
        // 1. 清理环境变量，验证无配置时返回 None
        std::env::remove_var("HIPPOCAMPUS_GENERATOR_API_URL");
        std::env::remove_var("HIPPOCAMPUS_GENERATOR_API_KEY");
        assert!(
            build_summary_generator_from_env().is_none(),
            "未配置 API_URL/API_KEY 时应返回 None"
        );

        // 2. 配置环境变量后验证返回 Some（注意 from_env 要求 URL 和 KEY 都非空）
        std::env::set_var("HIPPOCAMPUS_GENERATOR_API_URL", "https://api.openai.com/v1");
        std::env::set_var("HIPPOCAMPUS_GENERATOR_API_KEY", "sk-test-key");
        let gen = build_summary_generator_from_env();

        // 清理环境变量（避免影响其他测试）
        std::env::remove_var("HIPPOCAMPUS_GENERATOR_API_URL");
        std::env::remove_var("HIPPOCAMPUS_GENERATOR_API_KEY");

        assert!(gen.is_some(), "配置 API_URL + API_KEY 后应返回 Some");
    }

    /// 验证注入 summary_generator 后 archive 调用 LLM 生成摘要（Rust 侧验证）
    ///
    /// 此测试直接调用 Rust Archiver（不经过 PyO3），验证 summary_generator 字段能正常注入。
    /// Python 侧行为由 PyO3 桥接，逻辑等价。
    #[tokio::test]
    async fn test_archive_with_summary_generator_injection() {
        use hippocampus_core::archive::Archiver;
        use hippocampus_core::generate::SummaryGenerator;
        use hippocampus_core::model::{MemoryFile, Summary};
        use tempfile::TempDir;

        // Mock 摘要生成器
        struct MockGen;
        #[async_trait::async_trait]
        impl SummaryGenerator for MockGen {
            async fn generate_summary(&self, _file: &MemoryFile) -> hippocampus_core::Result<Summary> {
                Ok(Summary {
                    title: "Python 绑定 LLM 摘要".into(),
                    abstract_text: Some("Mock 摘要".into()),
                    key_facts: vec!["事实".into()],
                    key_entities: vec!["实体".into()],
                    clue_anchors: Vec::new(),
                })
            }
        }

        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));
        let gen: Arc<dyn SummaryGenerator> = Arc::new(MockGen);

        let mut archiver = Archiver::new(
            ArchiveConfig::default(),
            storage,
            "sess-py-test",
            None,
        )
        .with_summary_generator(gen);

        // 推入一个 turn
        use hippocampus_core::model::{MessageContent, MessageTurn, Tag};
        use chrono::Utc;
        use uuid::Uuid;
        archiver.push_turn(MessageTurn {
            id: Uuid::new_v4(),
            user_message: MessageContent {
                text: Some("测试消息".into()),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                thinking: None,
            },
            llm_message: MessageContent {
                text: Some("回复".into()),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                thinking: None,
            },
            tags: vec![Tag::Text],
            timestamp: Utc::now(),
            token_count: 10,
        });

        let (_, hook) = archiver.archive().await.unwrap();

        // 验证 LLM 摘要被注入
        assert_eq!(hook.summary.title, "Python 绑定 LLM 摘要");
        assert_eq!(hook.summary.abstract_text.as_deref(), Some("Mock 摘要"));
    }
}
