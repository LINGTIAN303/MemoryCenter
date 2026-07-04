//! # Tokenizer trait 与 TokenizerKind enum
//!
//! Token 计数抽象层，支持多种分词器实现。
//! 重依赖（tiktoken-rs）隔离在 [`crate::tiktoken_impl`]，未启用时降级为字符级。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Tokenizer 抽象 trait
///
/// 任何能计算文本 token 数的实现都应符合此 trait。
/// 内置实现：
/// - [`crate::tiktoken_impl::TiktokenTokenizer`]：基于 tiktoken-rs（GPT/Claude 近似）
/// - [`crate::char_impl::CharTokenizer`]：字符级兜底（无依赖，中文 1 字 ≈ 1.5 token）
pub trait Tokenizer: Send + Sync {
    /// 计算文本的 token 数
    fn count_tokens(&self, text: &str) -> usize;

    /// 返回 tokenizer 名称（用于调试/日志）
    fn name(&self) -> &str;
}

/// Tokenizer 类型枚举（用于配置选择）
///
/// 对照 2026 年主流模型的分词器：
/// - `O200kBase`：GPT-4o/4-turbo/5/5.2 系列
/// - `Cl100kBase`：GPT-4/3.5 系列（向后兼容）
/// - `ClaudeApprox`：Claude 系列（官方未开源，用 cl100k + 系数 1.05 近似）
/// - `DeepSeekApprox`：DeepSeek 系列（近似 cl100k，系数 1.1）
/// - `CharacterBased`：字符级兜底（中文 1 字 ≈ 1.5 token，英文按词）
///
/// # 序列化说明
///
/// 序列化时只存储类型名称（如 `"o200k_base"`），不序列化 `Custom` 变体内部的 trait 对象。
/// 反序列化 `Custom` 时回退为 `CharacterBased`（因为 trait 对象无法反序列化）。
#[derive(Clone)]
pub enum TokenizerKind {
    /// GPT-4o/5 系列分词器（o200k_base）
    O200kBase,

    /// GPT-4/3.5 系列分词器（cl100k_base）
    Cl100kBase,

    /// Claude 近似分词器（cl100k + 系数 1.05）
    ClaudeApprox,

    /// DeepSeek 近似分词器（cl100k + 系数 1.1）
    DeepSeekApprox,

    /// 字符级分词器（无依赖兜底，中文 1 字 ≈ 1.5 token）
    CharacterBased,

    /// 自定义分词器（用户注入实现，不可序列化）
    Custom(Arc<dyn Tokenizer>),
}

impl TokenizerKind {
    /// 构建对应的 Tokenizer 实例
    ///
    /// - `O200kBase` / `Cl100kBase` / `ClaudeApprox` / `DeepSeekApprox` → TiktokenTokenizer（失败降级 CharTokenizer）
    /// - `CharacterBased` → CharTokenizer
    /// - `Custom` → 返回内部 Arc 的克隆
    pub fn build(&self) -> Arc<dyn Tokenizer> {
        match self {
            Self::O200kBase => match crate::tiktoken_impl::TiktokenTokenizer::o200k_base() {
                Ok(tk) => Arc::new(tk),
                Err(e) => {
                    tracing::warn!("o200k_base 初始化失败: {}，降级为 CharTokenizer", e);
                    Arc::new(crate::char_impl::CharTokenizer::new())
                }
            },
            Self::Cl100kBase => match crate::tiktoken_impl::TiktokenTokenizer::cl100k_base() {
                Ok(tk) => Arc::new(tk),
                Err(e) => {
                    tracing::warn!("cl100k_base 初始化失败: {}，降级为 CharTokenizer", e);
                    Arc::new(crate::char_impl::CharTokenizer::new())
                }
            },
            Self::ClaudeApprox => match crate::tiktoken_impl::TiktokenTokenizer::claude_approx() {
                Ok(tk) => Arc::new(tk),
                Err(e) => {
                    tracing::warn!("claude_approx 初始化失败: {}，降级为 CharTokenizer", e);
                    Arc::new(crate::char_impl::CharTokenizer::new())
                }
            },
            Self::DeepSeekApprox => match crate::tiktoken_impl::TiktokenTokenizer::deepseek_approx() {
                Ok(tk) => Arc::new(tk),
                Err(e) => {
                    tracing::warn!("deepseek_approx 初始化失败: {}，降级为 CharTokenizer", e);
                    Arc::new(crate::char_impl::CharTokenizer::new())
                }
            },
            Self::CharacterBased => Arc::new(crate::char_impl::CharTokenizer::new()),
            Self::Custom(t) => t.clone(),
        }
    }

    /// 返回类型名称（用于日志/调试/序列化）
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::O200kBase => "o200k_base",
            Self::Cl100kBase => "cl100k_base",
            Self::ClaudeApprox => "claude_approx",
            Self::DeepSeekApprox => "deepseek_approx",
            Self::CharacterBased => "character_based",
            Self::Custom(_) => "custom",
        }
    }

    /// 从类型名称构建 TokenizerKind（反序列化用）
    ///
    /// `custom` 回退为 `CharacterBased`（因为 trait 对象无法反序列化）
    pub fn from_type_name(name: &str) -> Self {
        match name {
            "o200k_base" => Self::O200kBase,
            "cl100k_base" => Self::Cl100kBase,
            "claude_approx" => Self::ClaudeApprox,
            "deepseek_approx" => Self::DeepSeekApprox,
            "character_based" | "custom" | _ => Self::CharacterBased,
        }
    }
}

impl Default for TokenizerKind {
    fn default() -> Self {
        // 默认用字符级（无依赖，向后兼容）
        Self::CharacterBased
    }
}

/// 手动实现 Debug（因为 `Custom(Arc<dyn Tokenizer>)` 无法自动 derive）
impl std::fmt::Debug for TokenizerKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::O200kBase => write!(f, "TokenizerKind(O200kBase)"),
            Self::Cl100kBase => write!(f, "TokenizerKind(Cl100kBase)"),
            Self::ClaudeApprox => write!(f, "TokenizerKind(ClaudeApprox)"),
            Self::DeepSeekApprox => write!(f, "TokenizerKind(DeepSeekApprox)"),
            Self::CharacterBased => write!(f, "TokenizerKind(CharacterBased)"),
            Self::Custom(_) => write!(f, "TokenizerKind(Custom(<tokenizer>))"),
        }
    }
}

/// 手动实现 Serialize（只存储类型名称）
impl Serialize for TokenizerKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.type_name())
    }
}

/// 手动实现 Deserialize（从类型名称重建，custom 回退为 character_based）
impl<'de> Deserialize<'de> for TokenizerKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let name = String::deserialize(deserializer)?;
        Ok(Self::from_type_name(&name))
    }
}

impl std::fmt::Display for TokenizerKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.type_name())
    }
}
