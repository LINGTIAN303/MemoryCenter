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
//! 1. 检测 token 数达到阈值
//! 2. 若 `wait_for_turn_completion=true`，等待当前轮次完成
//! 3. 截取该批次完整上下文（用户消息 + LLM 消息）
//! 4. 生成 [`MemoryFile`]，打标签集合
//! 5. 生成 [`IndexHook`] 指向该记忆文件
//! 6. 将记忆文件写入存储后端
//! 7. 从 LLM 上下文丢弃该批次（前端渲染可保留供查看）

use crate::model::{ArchiveConfig, IndexHook, MemoryFile, MessageTurn};

/// 归档器
///
/// 负责检测归档触发条件并执行归档操作。
pub struct Archiver {
    config: ArchiveConfig,
    /// 当前累计 token 数
    current_tokens: usize,
    /// 当前缓冲的轮次（待归档）
    pending_turns: Vec<MessageTurn>,
}

impl Archiver {
    /// 创建新的归档器
    pub fn new(config: ArchiveConfig) -> Self {
        Self {
            config,
            current_tokens: 0,
            pending_turns: Vec::new(),
        }
    }

    /// 追加一轮消息，返回是否达到归档阈值
    pub fn push_turn(&mut self, turn: MessageTurn) -> bool {
        self.current_tokens += turn.token_count;
        self.pending_turns.push(turn);
        self.current_tokens >= self.config.token_threshold
    }

    /// 当前累计 token 数
    pub fn current_tokens(&self) -> usize {
        self.current_tokens
    }

    /// 是否达到归档阈值
    pub fn should_archive(&self) -> bool {
        self.current_tokens >= self.config.token_threshold
    }

    /// 是否超过强制截断上限
    pub fn should_force_truncate(&self) -> bool {
        self.current_tokens >= self.config.force_truncate_limit
    }

    /// 执行归档：消费待归档的轮次，生成记忆文件和索引钩子
    ///
    /// TODO: P2 阶段实现完整归档逻辑
    pub fn archive(&mut self) -> crate::Result<(MemoryFile, IndexHook)> {
        let turns = std::mem::take(&mut self.pending_turns);
        let total_tokens = self.current_tokens;
        self.current_tokens = 0;

        let _ = (turns, total_tokens); // 占位，P2 实现
        Err(crate::Error::Storage("archive() 待 P2 实现".into()))
    }
}
