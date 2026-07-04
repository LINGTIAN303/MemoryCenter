//! # 模型家族 enum
//!
//! 稳定的模型大类，低频迭代（几年一变）。
//! 具体型号（如 Claude Opus 4.6 / GPT-5.2）由 [`crate::variant::ModelVariant`] 表达。

use serde::{Deserialize, Serialize};

/// LLM 模型家族（稳定大类）
///
/// 每个家族对应一类架构相似的模型系列，具体型号由 `ModelVariant` 描述。
/// 家族稳定，型号高频迭代——新型号只需新增 `ModelVariant` 构造器，无需改家族 enum。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelFamily {
    /// Anthropic Claude 系列（Opus/Sonnet/Haiku）
    /// 架构特点：思考链、多模态、超长上下文（200K-2M）
    Claude,

    /// OpenAI GPT 系列（GPT-4/4o/5/5.2/Codex）
    /// 架构特点：function calling、JSON mode、Codex 编程优化
    Gpt,

    /// Google Gemini 系列（Gemini 2.5/3 Pro）
    /// 架构特点：原生多模态、超长上下文（1M+）、sentencepiece 分词
    Gemini,

    /// DeepSeek 系列（V3/V3.2/R1）
    /// 架构特点：R1 思考链、MoE 架构、近似 cl100k 分词
    DeepSeek,

    /// 阿里 Qwen 系列（Qwen 2.5/3）
    /// 架构特点：中文优化、多模态、BPE 分词
    Qwen,

    /// Meta Llama 系列（Llama 3.3/4）
    /// 架构特点：开源、sentencepiece 分词、128K 上下文
    Llama,

    /// xAI Grok 系列（Grok 3/4/4.1）
    /// 架构特点：实时数据接入、128K 上下文
    Grok,

    /// 本地模型（Ollama/vLLM/llama.cpp 部署的开源模型）
    /// 架构特点：离线运行、隐私优先、tokenizer 取决于具体模型
    Local,

    /// 自定义模型（用户通过 ModelVariant::custom 配置）
    Custom,
}

impl ModelFamily {
    /// 返回家族的中文名称
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Claude => "Anthropic Claude",
            Self::Gpt => "OpenAI GPT",
            Self::Gemini => "Google Gemini",
            Self::DeepSeek => "DeepSeek",
            Self::Qwen => "阿里 Qwen",
            Self::Llama => "Meta Llama",
            Self::Grok => "xAI Grok",
            Self::Local => "本地模型",
            Self::Custom => "自定义模型",
        }
    }

    /// 返回家族的默认 tokenizer 类型（用户未指定时使用）
    pub fn default_tokenizer(&self) -> crate::tokenizer::TokenizerKind {
        use crate::tokenizer::TokenizerKind;
        match self {
            Self::Claude => TokenizerKind::ClaudeApprox,    // Claude 官方未开源 tokenizer
            Self::Gpt => TokenizerKind::O200kBase,          // GPT-4o/5 系列
            Self::Gemini => TokenizerKind::CharacterBased,  // sentencepiece 需额外依赖，先用字符级兜底
            Self::DeepSeek => TokenizerKind::DeepSeekApprox,
            Self::Qwen => TokenizerKind::CharacterBased,    // Qwen tokenizer 需额外依赖
            Self::Llama => TokenizerKind::CharacterBased,   // sentencepiece 需额外依赖
            Self::Grok => TokenizerKind::O200kBase,         // Grok 近似 GPT 分词
            Self::Local => TokenizerKind::CharacterBased,
            Self::Custom => TokenizerKind::CharacterBased,
        }
    }

    /// 返回所有家族变体（用于遍历）
    pub fn all() -> [Self; 9] {
        [
            Self::Claude,
            Self::Gpt,
            Self::Gemini,
            Self::DeepSeek,
            Self::Qwen,
            Self::Llama,
            Self::Grok,
            Self::Local,
            Self::Custom,
        ]
    }
}

impl std::fmt::Display for ModelFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}
