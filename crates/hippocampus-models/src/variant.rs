//! # 模型型号（ModelVariant）
//!
//! 具体模型版本的参数描述，高频迭代（几个月一代）。
//! 内置 2026 年 7 月最新主流型号构造器，用户也可通过 [`ModelVariant::custom`] 自定义。
//!
//! ## 家族/型号分离设计
//!
//! - 家族（[`crate::family::ModelFamily`]）：稳定大类，enum，低频迭代
//! - 型号（[`ModelVariant`]）：具体版本，struct + 构造器，高频迭代
//!
//! 新型号发布时只需新增构造器方法，无需改家族 enum。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::family::ModelFamily;
use crate::tokenizer::{Tokenizer, TokenizerKind};

/// 工具调用格式
///
/// 不同模型家族支持的工具调用协议不同，影响 tool_calls 消息的序列化方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallFormat {
    /// OpenAI function calling 格式（JSON）
    /// GPT 系列、DeepSeek、Qwen、Llama、Grok 等
    OpenAI,

    /// Anthropic tool_use content block 格式
    /// Claude 系列
    Anthropic,

    /// Gemini function call 格式
    /// Gemini 系列
    Gemini,

    /// XML 标签格式（部分开源模型）
    Xml,

    /// 无工具调用能力
    None,
}

/// 归档策略
///
/// 根据模型上下文窗口大小，采用不同的归档阈值与策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "threshold")]
pub enum ArchiveStrategy {
    /// 长窗口模型（≥200K）：阈值高，单次归档多内容
    /// 如 Claude Opus 4.6（1M）、Gemini 3 Pro（1M+）
    LargeWindow { threshold: usize },

    /// 标准窗口（32K-128K）：标准归档
    /// 如 GPT-5.2（128K）、Qwen 3（128K）、Llama 4（128K）
    Standard { threshold: usize },

    /// 小窗口（≤16K）：频繁归档，摘要更精炼
    /// 如本地小模型、旧模型
    SmallWindow { threshold: usize },
}

impl ArchiveStrategy {
    /// 返回归档阈值
    pub fn threshold(&self) -> usize {
        match self {
            Self::LargeWindow { threshold } => *threshold,
            Self::Standard { threshold } => *threshold,
            Self::SmallWindow { threshold } => *threshold,
        }
    }

    /// 返回硬上限（1.5 倍阈值）
    pub fn hard_limit(&self) -> usize {
        (self.threshold() as f32 * 1.5) as usize
    }
}

/// 模型型号（具体版本参数）
///
/// 描述一个具体模型的所有参数，驱动 Hippocampus 的针对化记忆工作流。
///
/// # 设计原则
///
/// - 内置构造器（如 [`ModelVariant::claude_opus_4_6`]）提供 2026 最新型号预设
/// - 用户可通过 [`ModelVariant::custom`] 自定义新型号，无需等发版
/// - 家族稳定，型号高频迭代——新型号只需新增构造器
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelVariant {
    /// 模型家族
    pub family: ModelFamily,

    /// 型号名称（如 "claude-opus-4.6" / "gpt-5.2" / "gemini-3-pro"）
    pub name: String,

    /// 上下文窗口大小（token 数）
    pub context_window: usize,

    /// Tokenizer 类型
    pub tokenizer: TokenizerKind,

    /// 是否支持思考链（reasoning / thinking）
    ///
    /// Claude 4.x / DeepSeek R1 / GPT-5（o1/o3 系列）支持
    /// 影响：Thinking 标签特殊处理、思考链独立归档
    pub supports_thinking: bool,

    /// 是否支持多模态（图片输入）
    ///
    /// Claude 4.x / GPT-5 / Gemini 3 / Qwen 3 支持
    /// 影响：Image 标签 + 附件归档策略
    pub supports_vision: bool,

    /// 是否支持音频输入
    ///
    /// Gemini 3 / Qwen 3 Audio 支持
    /// 影响：Voice 标签处理
    pub supports_audio: bool,

    /// 工具调用格式
    pub tool_call_format: ToolCallFormat,

    /// 归档策略（基于上下文窗口大小）
    pub archive_strategy: ArchiveStrategy,

    /// 摘要生成的最大 token 数
    pub summary_max_tokens: usize,
}

impl ModelVariant {
    /// 构建对应的 Tokenizer 实例
    pub fn build_tokenizer(&self) -> Arc<dyn Tokenizer> {
        self.tokenizer.build()
    }

    /// 快速计算文本的 token 数
    pub fn count_tokens(&self, text: &str) -> usize {
        self.build_tokenizer().count_tokens(text)
    }

    // ========================================================================
    // 2026 年 7 月最新主流型号内置构造器
    // ========================================================================

    /// Anthropic Claude Opus 4.6（2026 年 2 月发布）
    ///
    /// - 上下文：100 万 token（Beta 版，正式版 200K）
    /// - 架构特点：思考链、多模态、超长上下文
    /// - tokenizer：cl100k 近似 × 1.05
    pub fn claude_opus_4_6() -> Self {
        Self {
            family: ModelFamily::Claude,
            name: "claude-opus-4.6".into(),
            context_window: 1_000_000,
            tokenizer: TokenizerKind::ClaudeApprox,
            supports_thinking: true,
            supports_vision: true,
            supports_audio: false,
            tool_call_format: ToolCallFormat::Anthropic,
            archive_strategy: ArchiveStrategy::LargeWindow { threshold: 400_000 },
            summary_max_tokens: 1024,
        }
    }

    /// Anthropic Claude Opus 4.5（2025 年发布）
    ///
    /// - 上下文：200 万 token
    /// - 架构特点：思考链、多模态、文档理解 98.7%
    pub fn claude_opus_4_5() -> Self {
        Self {
            family: ModelFamily::Claude,
            name: "claude-opus-4.5".into(),
            context_window: 2_000_000,
            tokenizer: TokenizerKind::ClaudeApprox,
            supports_thinking: true,
            supports_vision: true,
            supports_audio: false,
            tool_call_format: ToolCallFormat::Anthropic,
            archive_strategy: ArchiveStrategy::LargeWindow { threshold: 500_000 },
            summary_max_tokens: 1024,
        }
    }

    /// Anthropic Claude Sonnet 4.5（2025 年发布）
    ///
    /// - 上下文：200K token
    /// - 架构特点：思考链、多模态、性价比高
    pub fn claude_sonnet_4_5() -> Self {
        Self {
            family: ModelFamily::Claude,
            name: "claude-sonnet-4.5".into(),
            context_window: 200_000,
            tokenizer: TokenizerKind::ClaudeApprox,
            supports_thinking: true,
            supports_vision: true,
            supports_audio: false,
            tool_call_format: ToolCallFormat::Anthropic,
            archive_strategy: ArchiveStrategy::Standard { threshold: 80_000 },
            summary_max_tokens: 1024,
        }
    }

    /// OpenAI GPT-5.2（2026 年最新）
    ///
    /// - 上下文：128K token
    /// - 架构特点：function calling、JSON mode、六边形战士
    /// - tokenizer：o200k_base
    pub fn gpt_5_2() -> Self {
        Self {
            family: ModelFamily::Gpt,
            name: "gpt-5.2".into(),
            context_window: 128_000,
            tokenizer: TokenizerKind::O200kBase,
            supports_thinking: false,
            supports_vision: true,
            supports_audio: false,
            tool_call_format: ToolCallFormat::OpenAI,
            archive_strategy: ArchiveStrategy::Standard { threshold: 60_000 },
            summary_max_tokens: 1024,
        }
    }

    /// OpenAI GPT-5-Codex（编程优化版）
    ///
    /// - 上下文：128K token
    /// - 架构特点：Codex 编程优化、沙箱执行
    pub fn gpt_5_codex() -> Self {
        Self {
            family: ModelFamily::Gpt,
            name: "gpt-5-codex".into(),
            context_window: 128_000,
            tokenizer: TokenizerKind::O200kBase,
            supports_thinking: false,
            supports_vision: true,
            supports_audio: false,
            tool_call_format: ToolCallFormat::OpenAI,
            archive_strategy: ArchiveStrategy::Standard { threshold: 60_000 },
            summary_max_tokens: 1024,
        }
    }

    /// Google Gemini 3 Pro（2026 年最新）
    ///
    /// - 上下文：1M+ token
    /// - 架构特点：原生多模态、超长上下文、sentencepiece 分词
    pub fn gemini_3_pro() -> Self {
        Self {
            family: ModelFamily::Gemini,
            name: "gemini-3-pro".into(),
            context_window: 1_000_000,
            tokenizer: TokenizerKind::CharacterBased, // sentencepiece 未集成，先用字符级
            supports_thinking: false,
            supports_vision: true,
            supports_audio: true,
            tool_call_format: ToolCallFormat::Gemini,
            archive_strategy: ArchiveStrategy::LargeWindow { threshold: 400_000 },
            summary_max_tokens: 1024,
        }
    }

    /// DeepSeek V3.2（2026 年最新）
    ///
    /// - 上下文：64K token
    /// - 架构特点：MoE 架构、R1 思考链、中文优化
    pub fn deepseek_v3_2() -> Self {
        Self {
            family: ModelFamily::DeepSeek,
            name: "deepseek-v3.2".into(),
            context_window: 64_000,
            tokenizer: TokenizerKind::DeepSeekApprox,
            supports_thinking: false,
            supports_vision: false,
            supports_audio: false,
            tool_call_format: ToolCallFormat::OpenAI,
            archive_strategy: ArchiveStrategy::Standard { threshold: 30_000 },
            summary_max_tokens: 1024,
        }
    }

    /// DeepSeek R1（推理模型）
    ///
    /// - 上下文：64K token
    /// - 架构特点：思考链、推理优化
    pub fn deepseek_r1() -> Self {
        Self {
            family: ModelFamily::DeepSeek,
            name: "deepseek-r1".into(),
            context_window: 64_000,
            tokenizer: TokenizerKind::DeepSeekApprox,
            supports_thinking: true,
            supports_vision: false,
            supports_audio: false,
            tool_call_format: ToolCallFormat::OpenAI,
            archive_strategy: ArchiveStrategy::Standard { threshold: 30_000 },
            summary_max_tokens: 1024,
        }
    }

    /// 阿里 Qwen 3（2026 年最新）
    ///
    /// - 上下文：128K token
    /// - 架构特点：中文优化、多模态、BPE 分词
    pub fn qwen_3() -> Self {
        Self {
            family: ModelFamily::Qwen,
            name: "qwen-3".into(),
            context_window: 128_000,
            tokenizer: TokenizerKind::CharacterBased, // Qwen tokenizer 未集成，先用字符级
            supports_thinking: false,
            supports_vision: true,
            supports_audio: false,
            tool_call_format: ToolCallFormat::OpenAI,
            archive_strategy: ArchiveStrategy::Standard { threshold: 60_000 },
            summary_max_tokens: 1024,
        }
    }

    /// Meta Llama 4（2026 年最新）
    ///
    /// - 上下文：128K token
    /// - 架构特点：开源、sentencepiece 分词
    pub fn llama_4() -> Self {
        Self {
            family: ModelFamily::Llama,
            name: "llama-4".into(),
            context_window: 128_000,
            tokenizer: TokenizerKind::CharacterBased,
            supports_thinking: false,
            supports_vision: true,
            supports_audio: false,
            tool_call_format: ToolCallFormat::OpenAI,
            archive_strategy: ArchiveStrategy::Standard { threshold: 60_000 },
            summary_max_tokens: 1024,
        }
    }

    /// xAI Grok 4.1（2026 年最新）
    ///
    /// - 上下文：128K token
    /// - 架构特点：实时数据接入
    pub fn grok_4_1() -> Self {
        Self {
            family: ModelFamily::Grok,
            name: "grok-4.1".into(),
            context_window: 128_000,
            tokenizer: TokenizerKind::O200kBase,
            supports_thinking: false,
            supports_vision: true,
            supports_audio: false,
            tool_call_format: ToolCallFormat::OpenAI,
            archive_strategy: ArchiveStrategy::Standard { threshold: 60_000 },
            summary_max_tokens: 1024,
        }
    }

    /// 本地模型（通用预设）
    ///
    /// - 上下文：默认 8K（用户应通过 [`ModelVariant::custom`] 覆盖）
    /// - 架构特点：离线运行、隐私优先
    pub fn local_default() -> Self {
        Self {
            family: ModelFamily::Local,
            name: "local-default".into(),
            context_window: 8_000,
            tokenizer: TokenizerKind::CharacterBased,
            supports_thinking: false,
            supports_vision: false,
            supports_audio: false,
            tool_call_format: ToolCallFormat::None,
            archive_strategy: ArchiveStrategy::SmallWindow { threshold: 4_000 },
            summary_max_tokens: 512,
        }
    }

    /// 自定义模型
    ///
    /// 用户通过此方法配置任意新型号，无需等 Hippocampus 发版。
    ///
    /// # 参数
    /// - `name`：型号名称
    /// - `family`：模型家族（决定默认 tokenizer）
    /// - `context_window`：上下文窗口大小
    pub fn custom(name: impl Into<String>, family: ModelFamily, context_window: usize) -> Self {
        let tokenizer = family.default_tokenizer();
        let archive_strategy = if context_window >= 200_000 {
            ArchiveStrategy::LargeWindow { threshold: context_window / 5 }
        } else if context_window >= 32_000 {
            ArchiveStrategy::Standard { threshold: context_window / 4 }
        } else {
            ArchiveStrategy::SmallWindow { threshold: context_window / 4 }
        };

        Self {
            family,
            name: name.into(),
            context_window,
            tokenizer,
            supports_thinking: false,
            supports_vision: false,
            supports_audio: false,
            tool_call_format: ToolCallFormat::OpenAI,
            archive_strategy,
            summary_max_tokens: 1024,
        }
    }
}

impl Default for ModelVariant {
    fn default() -> Self {
        // 默认用本地模型预设（最保守配置）
        Self::local_default()
    }
}

impl std::fmt::Display for ModelVariant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({}, {}K ctx, thinking={}, vision={})",
            self.name,
            self.family,
            self.context_window / 1000,
            self.supports_thinking,
            self.supports_vision
        )
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claude_opus_4_6() {
        let v = ModelVariant::claude_opus_4_6();
        assert_eq!(v.family, ModelFamily::Claude);
        assert_eq!(v.name, "claude-opus-4.6");
        assert_eq!(v.context_window, 1_000_000);
        assert!(v.supports_thinking);
        assert!(v.supports_vision);
        assert_eq!(v.tool_call_format, ToolCallFormat::Anthropic);
        match v.archive_strategy {
            ArchiveStrategy::LargeWindow { threshold } => assert_eq!(threshold, 400_000),
            _ => panic!("应为 LargeWindow 策略"),
        }
    }

    #[test]
    fn test_gpt_5_2() {
        let v = ModelVariant::gpt_5_2();
        assert_eq!(v.family, ModelFamily::Gpt);
        assert_eq!(v.context_window, 128_000);
        assert!(!v.supports_thinking);
        assert_eq!(v.tool_call_format, ToolCallFormat::OpenAI);
        match v.archive_strategy {
            ArchiveStrategy::Standard { threshold } => assert_eq!(threshold, 60_000),
            _ => panic!("应为 Standard 策略"),
        }
    }

    #[test]
    fn test_gemini_3_pro() {
        let v = ModelVariant::gemini_3_pro();
        assert_eq!(v.family, ModelFamily::Gemini);
        assert_eq!(v.context_window, 1_000_000);
        assert!(v.supports_audio);
        assert_eq!(v.tool_call_format, ToolCallFormat::Gemini);
    }

    #[test]
    fn test_deepseek_r1_thinking() {
        let v = ModelVariant::deepseek_r1();
        assert!(v.supports_thinking, "R1 应支持思考链");
        assert_eq!(v.tool_call_format, ToolCallFormat::OpenAI);
    }

    #[test]
    fn test_custom_model_large_window() {
        let v = ModelVariant::custom("my-model", ModelFamily::Custom, 500_000);
        match v.archive_strategy {
            ArchiveStrategy::LargeWindow { threshold } => assert_eq!(threshold, 100_000),
            _ => panic!("500K 窗口应为 LargeWindow"),
        }
    }

    #[test]
    fn test_custom_model_standard_window() {
        let v = ModelVariant::custom("my-model", ModelFamily::Custom, 64_000);
        match v.archive_strategy {
            ArchiveStrategy::Standard { threshold } => assert_eq!(threshold, 16_000),
            _ => panic!("64K 窗口应为 Standard"),
        }
    }

    #[test]
    fn test_custom_model_small_window() {
        let v = ModelVariant::custom("my-model", ModelFamily::Custom, 8_000);
        match v.archive_strategy {
            ArchiveStrategy::SmallWindow { threshold } => assert_eq!(threshold, 2_000),
            _ => panic!("8K 窗口应为 SmallWindow"),
        }
    }

    #[test]
    fn test_archive_strategy_hard_limit() {
        let s = ArchiveStrategy::LargeWindow { threshold: 400_000 };
        assert_eq!(s.hard_limit(), 600_000);
    }

    #[test]
    fn test_count_tokens() {
        let v = ModelVariant::gpt_5_2();
        let count = v.count_tokens("Hello, world!");
        assert!(count > 0);
    }

    #[test]
    fn test_display() {
        let v = ModelVariant::claude_opus_4_6();
        let s = format!("{}", v);
        assert!(s.contains("claude-opus-4.6"));
        assert!(s.contains("1000K"));
    }

    #[test]
    fn test_all_builtin_variants() {
        // 确保所有内置构造器能正常创建
        let _ = ModelVariant::claude_opus_4_6();
        let _ = ModelVariant::claude_opus_4_5();
        let _ = ModelVariant::claude_sonnet_4_5();
        let _ = ModelVariant::gpt_5_2();
        let _ = ModelVariant::gpt_5_codex();
        let _ = ModelVariant::gemini_3_pro();
        let _ = ModelVariant::deepseek_v3_2();
        let _ = ModelVariant::deepseek_r1();
        let _ = ModelVariant::qwen_3();
        let _ = ModelVariant::llama_4();
        let _ = ModelVariant::grok_4_1();
        let _ = ModelVariant::local_default();
    }
}
