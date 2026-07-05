//! # 归档模块
//!
//! 负责将会话窗口的完整上下文冻结为记忆文件。
//!
//! ## 归档触发机制
//!
//! - **Token 计量**：累计当前会话窗口的 token 数
//! - **动态范围**：达到阈值后，若当前轮次（Agent 任务/工具调用链）未完成则等待
//! - **硬上限**：达到 1.5 倍阈值时强制截断（标记 `truncated=true`）
//!
//! ## 归档流程
//!
//! 1. 检测 token 数达到阈值（调用方检查 [`Archiver::should_archive`]）
//! 2. 若 `wait_for_turn_completion=true`，由调用方判断轮次是否完成
//! 3. 调用 [`Archiver::archive`] 执行归档：
//!    a. 消费缓冲的轮次（`pending_turns`）
//!    b. 生成 [`MemoryFile`]（自动计算标签并集 + total_tokens）
//!    c. 写入 Storage 得到相对路径
//!    d. 用路径生成 [`IndexHook`]
//!    e. 追加钩子到索引文档（`Storage::append_hook`）
//!    f. 重置 token 计数器，返回 `(MemoryFile, IndexHook)`
//! 4. 调用方从 LLM 上下文丢弃该批次（前端渲染可保留供查看）

use crate::generate::SummaryGenerator;
use crate::model::{ArchiveConfig, ArchivePeriod, IndexHook, MemoryFile, MessageTurn};
use crate::storage::Storage;
use std::sync::Arc;

/// 归档器
///
/// 负责检测归档触发条件并执行归档操作。
/// 持有 [`Storage`] 引用，`archive()` 内部完成全流程（写入 + 索引追加）。
///
/// ## 用法
///
/// ```rust,ignore
/// let mut archiver = Archiver::new(config, storage, "session-001", None);
///
/// // 持续追加轮次
/// archiver.push_turn(turn1);
/// archiver.push_turn(turn2);
///
/// // 检查是否达到阈值
/// if archiver.should_archive() {
///     let (memory, hook) = archiver.archive().await?;
///     // 从 LLM 上下文丢弃该批次
/// }
/// ```
pub struct Archiver {
    /// 归档配置
    config: ArchiveConfig,
    /// 当前累计 token 数
    current_tokens: usize,
    /// 当前缓冲的轮次（待归档）
    pending_turns: Vec<MessageTurn>,
    /// 存储后端
    storage: Arc<dyn Storage>,
    /// 会话 ID
    session_id: String,
    /// 项目 ID（可选）
    project_id: Option<String>,
    /// 可选的 LLM 摘要生成器（v2.21 批次 8a）
    ///
    /// 注入后 `archive()` 时调用 `generate_summary()` 生成结构化摘要填入 IndexHook。
    /// 未注入或调用失败时降级为 `Summary::from_title`（启发式，向后兼容）。
    summary_generator: Option<Arc<dyn SummaryGenerator>>,
    /// 可选的摘要模板覆盖（v2.29 Presets 落地）
    ///
    /// 来自 `CombinedProfile::summary_template()`，archive 时通过
    /// [`SummaryGenerator::generate_summary_with_template`] 传入。
    /// `None` 时调用 [`generate_summary`](SummaryGenerator::generate_summary)（向后兼容）。
    summary_template_override: Option<String>,
}

impl Archiver {
    /// 创建新的归档器
    ///
    /// - `config`：归档阈值配置
    /// - `storage`：存储后端（Arc<dyn Storage>）
    /// - `session_id`：当前会话 ID
    /// - `project_id`：项目 ID（可选，影响存储路径）
    pub fn new(
        config: ArchiveConfig,
        storage: Arc<dyn Storage>,
        session_id: impl Into<String>,
        project_id: Option<String>,
    ) -> Self {
        Self {
            config,
            current_tokens: 0,
            pending_turns: Vec::new(),
            storage,
            session_id: session_id.into(),
            project_id,
            summary_generator: None,
            summary_template_override: None,
        }
    }

    /// 注入 LLM 摘要生成器（v2.21 批次 8a）
    ///
    /// 注入后 `archive()` 时自动调用 LLM 生成结构化摘要
    /// （title + abstract + key_facts + key_entities）填入 IndexHook。
    /// 未注入时使用启发式 `Summary::from_title`（首条消息前 80 字符）。
    ///
    /// ## 降级策略
    ///
    /// - LLM 调用失败：降级为 `Summary::from_title`，归档主流程不中断
    /// - 未注入：使用 `Summary::from_title`（向后兼容）
    pub fn with_summary_generator(mut self, gen: Arc<dyn SummaryGenerator>) -> Self {
        self.summary_generator = Some(gen);
        self
    }

    /// 注入摘要模板覆盖（v2.29 Presets 落地）
    ///
    /// 来自 `CombinedProfile::summary_template()`，archive 时通过
    /// [`SummaryGenerator::generate_summary_with_template`] 传入。
    ///
    /// - `Some(template)`：调用 `generate_summary_with_template(Some(template))`
    /// - `None`：调用 `generate_summary()`（向后兼容）
    ///
    /// 模板需含 `{conversation}` 占位符。未注入 summary_generator 时本字段无效果。
    pub fn with_summary_template_override(mut self, template: impl Into<String>) -> Self {
        self.summary_template_override = Some(template.into());
        self
    }

    /// 追加一轮消息，返回是否达到归档阈值
    ///
    /// 调用方应持续追加轮次，并检查 [`should_archive`](Self::should_archive)
    /// 或返回值决定何时归档。
    pub fn push_turn(&mut self, turn: MessageTurn) -> bool {
        self.current_tokens += turn.token_count;
        self.pending_turns.push(turn);
        self.current_tokens >= self.config.token_threshold
    }

    /// 当前累计 token 数
    pub fn current_tokens(&self) -> usize {
        self.current_tokens
    }

    /// 当前缓冲的轮次数量
    pub fn pending_turns_count(&self) -> usize {
        self.pending_turns.len()
    }

    /// 是否达到归档阈值
    ///
    /// 调用方应在轮次完成后检查此方法，决定是否调用 [`archive`](Self::archive)。
    pub fn should_archive(&self) -> bool {
        self.current_tokens >= self.config.token_threshold
    }

    /// 是否超过强制截断上限
    ///
    /// 即使 `wait_for_turn_completion=true`，超过硬上限也必须立即截断。
    pub fn should_force_truncate(&self) -> bool {
        self.current_tokens >= self.config.force_truncate_limit
    }

    /// 归档配置引用
    pub fn config(&self) -> &ArchiveConfig {
        &self.config
    }

    /// 执行归档
    ///
    /// 完整流程：
    /// 1. 消费 `pending_turns`
    /// 2. 生成 [`MemoryFile`]（自动计算标签并集 + total_tokens）
    /// 3. 若超过硬上限，标记 `truncated=true`
    /// 4. 写入 Storage 得到相对路径
    /// 5. 生成 [`IndexHook`] 指向该记忆文件
    /// 6. 追加钩子到 daily 索引文档
    /// 7. 重置 token 计数器
    ///
    /// **注意**：归档后 `pending_turns` 和 `current_tokens` 会被清零。
    /// 调用方应从 LLM 上下文丢弃该批次（前端渲染可保留）。
    pub async fn archive(&mut self) -> crate::Result<(MemoryFile, IndexHook)> {
        if self.pending_turns.is_empty() {
            return Err(crate::Error::Storage("归档失败: pending_turns 为空".into()));
        }

        // 1. 消费 pending_turns
        let turns = std::mem::take(&mut self.pending_turns);
        let was_over_limit = self.current_tokens >= self.config.force_truncate_limit;
        let total_tokens = self.current_tokens;
        self.current_tokens = 0;

        // 2. 生成 MemoryFile
        let mut memory_file = MemoryFile::new(
            self.session_id.clone(),
            self.project_id.clone(),
            turns,
            ArchivePeriod::Daily,
        );

        // 3. 若超过硬上限，标记截断
        if was_over_limit {
            memory_file.mark_truncated();
        }

        // 校验 total_tokens 一致性
        debug_assert_eq!(
            memory_file.total_tokens, total_tokens,
            "MemoryFile total_tokens 与 Archiver 计量不一致"
        );

        // 4. 写入 Storage
        let memory_path = self.storage.write_memory(&memory_file).await?;

        // 5. 生成 IndexHook（默认启发式摘要：首条消息前 80 字符）
        let mut hook = IndexHook::from_memory_file(&memory_file, memory_path);

        // 5.1 v2.21 批次 8a: 若注入了 LLM 摘要生成器，尝试生成结构化摘要替换启发式
        // v2.29: 若注入了 summary_template_override，通过 generate_summary_with_template 覆盖模板
        //
        // 降级策略：LLM 调用失败时保留启发式摘要，归档主流程不中断
        if let Some(gen) = &self.summary_generator {
            let result = if let Some(tpl) = &self.summary_template_override {
                gen.generate_summary_with_template(&memory_file, Some(tpl)).await
            } else {
                gen.generate_summary(&memory_file).await
            };
            match result {
                Ok(llm_summary) => {
                    tracing::info!(
                        title = %llm_summary.title,
                        facts_count = llm_summary.key_facts.len(),
                        entities_count = llm_summary.key_entities.len(),
                        has_template_override = self.summary_template_override.is_some(),
                        "LLM 摘要生成成功，替换启发式摘要"
                    );
                    hook.summary = llm_summary;
                }
                Err(e) => {
                    // 降级：保留启发式摘要，归档主流程不中断
                    tracing::warn!(
                        error = %e,
                        "LLM 摘要生成失败，降级为启发式 Summary::from_title"
                    );
                }
            }
        }

        // 6. 追加钩子到 daily 索引文档（session 级）
        self.storage
            .append_hook(
                &self.session_id,
                self.project_id.as_deref(),
                ArchivePeriod::Daily,
                hook.clone(),
            )
            .await?;

        // 7. v2.4: 双写 - 若有 project_id，同时追加到 project 级聚合索引
        if let Some(pid) = &self.project_id {
            self.storage
                .append_project_hook(pid, ArchivePeriod::Daily, hook.clone())
                .await?;
        }

        // 记录日志（tracing）
        tracing::info!(
            memory_id = %memory_file.id,
            tokens = memory_file.total_tokens,
            truncated = memory_file.truncated,
            "归档完成: 记忆文件已写入 Storage"
        );

        Ok((memory_file, hook))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MessageContent, Tag};
    use crate::storage::LocalStorage;
    use chrono::Utc;
    use tempfile::TempDir;
    use uuid::Uuid;

    /// 构造测试用 MessageTurn
    fn make_turn(token_count: usize) -> MessageTurn {
        MessageTurn {
            id: Uuid::new_v4(),
            user_message: MessageContent {
                text: Some("测试用户消息".into()),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                thinking: None,
            },
            llm_message: MessageContent {
                text: Some("测试 LLM 回复".into()),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                thinking: None,
            },
            tags: vec![Tag::Text],
            timestamp: Utc::now(),
            token_count,
        }
    }

    #[tokio::test]
    async fn test_archiver_push_and_threshold() {
        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> =
            Arc::new(LocalStorage::new(tmp.path()));
        let config = ArchiveConfig {
            token_threshold: 100,
            force_truncate_limit: 150,
            wait_for_turn_completion: true,
        };
        let mut archiver = Archiver::new(config, storage, "sess-001", None);

        // 推入 2 个 turn（各 50 token），达到阈值
        assert!(!archiver.push_turn(make_turn(50)));
        assert_eq!(archiver.current_tokens(), 50);
        assert!(archiver.push_turn(make_turn(50))); // 100 >= 100
        assert!(archiver.should_archive());
        assert!(!archiver.should_force_truncate());
    }

    #[tokio::test]
    async fn test_archiver_force_truncate() {
        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> =
            Arc::new(LocalStorage::new(tmp.path()));
        let config = ArchiveConfig {
            token_threshold: 100,
            force_truncate_limit: 150,
            wait_for_turn_completion: true,
        };
        let mut archiver = Archiver::new(config, storage, "sess-002", None);

        // 推入超过硬上限的 turn
        archiver.push_turn(make_turn(160));
        assert!(archiver.should_force_truncate());
    }

    #[tokio::test]
    async fn test_archiver_archive_full_flow() {
        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> =
            Arc::new(LocalStorage::new(tmp.path()));
        let config = ArchiveConfig {
            token_threshold: 100,
            force_truncate_limit: 150,
            wait_for_turn_completion: true,
        };
        let mut archiver = Archiver::new(config, storage.clone(), "sess-003", None);

        // 推入 2 个 turn
        archiver.push_turn(make_turn(60));
        archiver.push_turn(make_turn(50));
        assert_eq!(archiver.pending_turns_count(), 2);

        // 归档
        let (memory, hook) = archiver.archive().await.unwrap();

        // 验证 MemoryFile
        assert_eq!(memory.session_id, "sess-003");
        assert_eq!(memory.turns.len(), 2);
        assert_eq!(memory.total_tokens, 110);
        assert!(!memory.truncated);
        assert_eq!(memory.period, ArchivePeriod::Daily);

        // 验证 IndexHook
        // memory_id 在 LocalStorage 中为文件相对路径
        assert!(!hook.memory_id.is_empty());
        assert!(hook.memory_id.contains("sessions/sess-003/daily/"));

        // 归档后状态清零
        assert_eq!(archiver.current_tokens(), 0);
        assert_eq!(archiver.pending_turns_count(), 0);

        // 验证 Storage 中有记忆文件和索引文档
        let memories = storage
            .list_memories("sess-003", None, ArchivePeriod::Daily)
            .await
            .unwrap();
        assert_eq!(memories.len(), 1);

        let index = storage
            .read_index("sess-003", None, ArchivePeriod::Daily)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(index.hooks.len(), 1);
    }

    #[tokio::test]
    async fn test_archiver_archive_truncated() {
        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> =
            Arc::new(LocalStorage::new(tmp.path()));
        let config = ArchiveConfig {
            token_threshold: 100,
            force_truncate_limit: 150,
            wait_for_turn_completion: true,
        };
        let mut archiver = Archiver::new(config, storage, "sess-004", None);

        // 推入超过硬上限的 turn
        archiver.push_turn(make_turn(160));
        assert!(archiver.should_force_truncate());

        // 归档（应标记 truncated）
        let (memory, _) = archiver.archive().await.unwrap();
        assert!(memory.truncated);
    }

    #[tokio::test]
    async fn test_archiver_multiple_archives() {
        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> =
            Arc::new(LocalStorage::new(tmp.path()));
        let config = ArchiveConfig {
            token_threshold: 100,
            force_truncate_limit: 150,
            wait_for_turn_completion: true,
        };
        let mut archiver = Archiver::new(config, storage.clone(), "sess-005", None);

        // 第一次归档
        archiver.push_turn(make_turn(60));
        archiver.push_turn(make_turn(50));
        archiver.archive().await.unwrap();

        // 第二次归档
        archiver.push_turn(make_turn(70));
        archiver.push_turn(make_turn(40));
        archiver.archive().await.unwrap();

        // Storage 中应有 2 个记忆文件
        let memories = storage
            .list_memories("sess-005", None, ArchivePeriod::Daily)
            .await
            .unwrap();
        assert_eq!(memories.len(), 2);

        // 索引文档应有 2 个钩子
        let index = storage
            .read_index("sess-005", None, ArchivePeriod::Daily)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(index.hooks.len(), 2);
    }

    #[tokio::test]
    async fn test_archiver_empty_archive_fails() {
        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> =
            Arc::new(LocalStorage::new(tmp.path()));
        let config = ArchiveConfig::default();
        let mut archiver = Archiver::new(config, storage, "sess-empty", None);

        // 空归档应失败
        let result = archiver.archive().await;
        assert!(result.is_err());
    }

    // ========================================================================
    // v2.21 批次 8a: SummaryGenerator 注入测试
    // ========================================================================

    use crate::model::Summary;

    /// Mock 摘要生成器（测试用）
    ///
    /// - `fail = true`：模拟 LLM 调用失败，返回 Err
    /// - `fail = false`：返回固定的结构化 Summary
    struct MockSummaryGenerator {
        fail: bool,
        title: String,
    }

    impl MockSummaryGenerator {
        fn new(title: impl Into<String>) -> Self {
            Self {
                fail: false,
                title: title.into(),
            }
        }
        fn failing() -> Self {
            Self {
                fail: true,
                title: String::new(),
            }
        }
    }

    #[async_trait::async_trait]
    impl SummaryGenerator for MockSummaryGenerator {
        async fn generate_summary(&self, _file: &MemoryFile) -> crate::Result<Summary> {
            if self.fail {
                return Err(crate::Error::Storage("Mock 摘要生成失败".into()));
            }
            Ok(Summary {
                title: self.title.clone(),
                abstract_text: Some("Mock 摘要内容".into()),
                key_facts: vec!["事实1".into(), "事实2".into()],
                key_entities: vec!["实体A".into()],
                clue_anchors: Vec::new(),
            })
        }
    }

    /// 注入 SummaryGenerator 后，archive() 应使用 LLM 生成的摘要
    #[tokio::test]
    async fn test_archive_with_summary_generator() {
        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));
        let config = ArchiveConfig {
            token_threshold: 100,
            force_truncate_limit: 150,
            wait_for_turn_completion: true,
        };
        let gen: Arc<dyn SummaryGenerator> =
            Arc::new(MockSummaryGenerator::new("LLM 生成的标题"));
        let mut archiver = Archiver::new(config, storage, "sess-gen-001", None)
            .with_summary_generator(gen);

        archiver.push_turn(make_turn(110));
        let (_memory, hook) = archiver.archive().await.unwrap();

        // 验证 IndexHook 的 summary 是 LLM 生成的，而非启发式
        assert_eq!(hook.summary.title, "LLM 生成的标题");
        assert!(hook.summary.abstract_text.is_some());
        assert_eq!(hook.summary.key_facts.len(), 2);
        assert_eq!(hook.summary.key_entities.len(), 1);
    }

    /// LLM 摘要生成失败时，降级为启发式 Summary::from_title
    #[tokio::test]
    async fn test_archive_summary_generator_failure_degrades() {
        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));
        let config = ArchiveConfig {
            token_threshold: 100,
            force_truncate_limit: 150,
            wait_for_turn_completion: true,
        };
        let gen: Arc<dyn SummaryGenerator> = Arc::new(MockSummaryGenerator::failing());
        let mut archiver = Archiver::new(config, storage, "sess-gen-002", None)
            .with_summary_generator(gen);

        archiver.push_turn(make_turn(110));
        let (_memory, hook) = archiver.archive().await.unwrap();

        // LLM 失败，降级为启发式（首条消息前 80 字符 + "..."）
        assert!(hook.summary.title.contains("测试用户消息"));
        assert!(hook.summary.title.ends_with("..."));
        // 启发式无 abstract/key_facts/key_entities
        assert!(hook.summary.abstract_text.is_none());
        assert!(hook.summary.key_facts.is_empty());
    }

    /// 未注入 SummaryGenerator 时，使用启发式摘要（向后兼容）
    #[tokio::test]
    async fn test_archive_without_summary_generator_uses_heuristic() {
        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));
        let config = ArchiveConfig {
            token_threshold: 100,
            force_truncate_limit: 150,
            wait_for_turn_completion: true,
        };
        // 不注入 summary_generator
        let mut archiver = Archiver::new(config, storage, "sess-gen-003", None);

        archiver.push_turn(make_turn(110));
        let (_memory, hook) = archiver.archive().await.unwrap();

        // 启发式摘要：首条消息前 80 字符 + "..."
        assert!(hook.summary.title.contains("测试用户消息"));
        assert!(hook.summary.title.ends_with("..."));
    }

    // ========================================================================
    // v2.29: summary_template_override 测试
    // ========================================================================

    /// 记录传入的 template_override 参数的 Mock 生成器
    struct TemplateRecordingGenerator {
        last_template: std::sync::Mutex<Option<String>>,
    }

    impl TemplateRecordingGenerator {
        fn new() -> Self {
            Self {
                last_template: std::sync::Mutex::new(None),
            }
        }
        fn last_template(&self) -> Option<String> {
            self.last_template.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl SummaryGenerator for TemplateRecordingGenerator {
        async fn generate_summary(&self, _file: &MemoryFile) -> crate::Result<Summary> {
            // 默认方法：记录 None
            *self.last_template.lock().unwrap() = None;
            Ok(Summary::from_title("default-call"))
        }

        async fn generate_summary_with_template(
            &self,
            _file: &MemoryFile,
            template_override: Option<&str>,
        ) -> crate::Result<Summary> {
            // 记录传入的 template_override
            *self.last_template.lock().unwrap() =
                template_override.map(|s| s.to_string());
            Ok(Summary::from_title("template-call"))
        }
    }

    /// 注入 summary_template_override 后，archive() 应调用
    /// `generate_summary_with_template(Some(template))` 而非 `generate_summary`
    #[tokio::test]
    async fn test_archive_with_summary_template_override() {
        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));
        let config = ArchiveConfig {
            token_threshold: 100,
            force_truncate_limit: 150,
            wait_for_turn_completion: true,
        };
        let gen: Arc<TemplateRecordingGenerator> = Arc::new(TemplateRecordingGenerator::new());
        let gen_dyn: Arc<dyn SummaryGenerator> = gen.clone();
        let mut archiver = Archiver::new(config, storage, "sess-tpl-001", None)
            .with_summary_generator(gen_dyn)
            .with_summary_template_override("custom preset template {conversation}");

        archiver.push_turn(make_turn(110));
        let (_memory, hook) = archiver.archive().await.unwrap();

        // 验证调用了 generate_summary_with_template，且传入了 template
        assert_eq!(gen.last_template(), Some("custom preset template {conversation}".to_string()));
        // 验证返回的是 template-call 的结果
        assert_eq!(hook.summary.title, "template-call");
    }

    /// 未注入 summary_template_override 时，archive() 应调用
    /// `generate_summary`（向后兼容）
    #[tokio::test]
    async fn test_archive_without_template_override_uses_default() {
        let tmp = TempDir::new().unwrap();
        let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(tmp.path()));
        let config = ArchiveConfig {
            token_threshold: 100,
            force_truncate_limit: 150,
            wait_for_turn_completion: true,
        };
        let gen: Arc<TemplateRecordingGenerator> = Arc::new(TemplateRecordingGenerator::new());
        let gen_dyn: Arc<dyn SummaryGenerator> = gen.clone();
        let mut archiver = Archiver::new(config, storage, "sess-tpl-002", None)
            .with_summary_generator(gen_dyn);
        // 不注入 summary_template_override

        archiver.push_turn(make_turn(110));
        let (_memory, hook) = archiver.archive().await.unwrap();

        // 验证调用了 generate_summary（template 记录为 None）
        assert_eq!(gen.last_template(), None);
        assert_eq!(hook.summary.title, "default-call");
    }
}
