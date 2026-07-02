//! # 检索模块
//!
//! 混合检索机制：摘要钩子注入 + tool 主动检索。
//!
//! ## 两种检索模式
//!
//! 1. **摘要钩子注入**：将索引钩子的摘要信息（标题+标签+时间戳）注入到
//!    system prompt，让 LLM 知道"有哪些记忆"，轻量
//! 2. **Tool 主动检索**：LLM 根据需要通过 tool 调用检索详细记忆文件，
//!    返回完整上下文
//!
//! ## 分层钩子设计
//!
//! - [`IndexHook`] 包含完整信息
//! - [`SummaryView`] 是轻量摘要视图（用于注入 system prompt）
//! - 详细检索返回完整 [`MemoryFile`]
//!
//! TODO: P2 阶段实现

use crate::model::{IndexHook, MemoryFile};

/// 摘要视图（用于注入 system prompt）
///
/// 仅包含轻量信息，避免占用过多上下文。
#[derive(Debug, Clone)]
pub struct SummaryView {
    /// 钩子 ID
    pub hook_id: String,
    /// 摘要标题
    pub summary_title: String,
    /// 标签集合（字符串形式）
    pub tags: Vec<String>,
    /// 归档时间
    pub archived_at: String,
}

impl From<&IndexHook> for SummaryView {
    fn from(hook: &IndexHook) -> Self {
        Self {
            hook_id: hook.id.to_string(),
            summary_title: hook.summary_title.clone(),
            tags: hook.tags.iter().map(|t| format!("{:?}", t)).collect(),
            archived_at: hook.archived_at.to_rfc3339(),
        }
    }
}

/// 检索器
pub struct Retriever {
    /// 当前可注入的摘要视图集合
    summaries: Vec<SummaryView>,
}

impl Retriever {
    /// 创建新的检索器
    pub fn new() -> Self {
        Self { summaries: Vec::new() }
    }

    /// 添加索引钩子（自动转为摘要视图）
    pub fn add_hook(&mut self, hook: &IndexHook) {
        self.summaries.push(SummaryView::from(hook));
    }

    /// 获取所有摘要视图（用于注入 system prompt）
    pub fn summary_views(&self) -> &[SummaryView] {
        &self.summaries
    }

    /// 渲染摘要视图为 system prompt 文本
    ///
    /// TODO: P2 阶段实现完整渲染逻辑
    pub fn render_to_system_prompt(&self) -> String {
        let mut out = String::from("# 可用记忆索引\n\n");
        for s in &self.summaries {
            out.push_str(&format!(
                "- [{}] {} (tags: {}, at: {})\n",
                s.hook_id, s.summary_title, s.tags.join(", "), s.archived_at
            ));
        }
        out
    }

    /// 按钩子 ID 检索完整记忆文件（tool 调用入口）
    ///
    /// TODO: P2 阶段实现（需接入 Storage trait）
    pub fn retrieve_memory(&self, _hook_id: &str) -> crate::Result<MemoryFile> {
        Err(crate::Error::Index("retrieve_memory() 待 P2 实现".into()))
    }
}

impl Default for Retriever {
    fn default() -> Self {
        Self::new()
    }
}
