//! # Tiktoken 分词器实现
//!
//! 基于 tiktoken-rs 的 BPE 分词器，支持 OpenAI 系列模型与 Claude/DeepSeek 近似。
//!
//! ## 支持的 encoding
//!
//! - `o200k_base`：GPT-4o/4-turbo/5/5.2 系列
//! - `cl100k_base`：GPT-4/3.5 系列（向后兼容）
//! - `ClaudeApprox`：cl100k_base + 系数 1.05（Claude 官方未开源）
//! - `DeepSeekApprox`：cl100k_base + 系数 1.1（DeepSeek 近似）
//!
//! ## 降级策略
//!
//! tiktoken-rs 初始化失败（如缺少词表文件）时返回 Err，
//! 由 [`crate::tokenizer::TokenizerKind::build`] 降级为 [`crate::char_impl::CharTokenizer`]。

use crate::tokenizer::Tokenizer;

/// Tiktoken 分词器
///
/// 内部持有 tiktoken-rs 的 Core BPE 实例，支持多种 encoding。
pub struct TiktokenTokenizer {
    /// BPE 编码器
    bpe: tiktoken_rs::CoreBPE,
    /// 类型名称
    kind_name: &'static str,
    /// 系数（用于 Claude/DeepSeek 近似，1.0 表示无调整）
    coefficient: f32,
}

impl TiktokenTokenizer {
    /// 创建 o200k_base 分词器（GPT-4o/5 系列）
    pub fn o200k_base() -> Result<Self, String> {
        let bpe = tiktoken_rs::o200k_base().map_err(|e| format!("o200k_base 初始化失败: {}", e))?;
        Ok(Self {
            bpe,
            kind_name: "o200k_base",
            coefficient: 1.0,
        })
    }

    /// 创建 cl100k_base 分词器（GPT-4/3.5 系列）
    pub fn cl100k_base() -> Result<Self, String> {
        let bpe = tiktoken_rs::cl100k_base().map_err(|e| format!("cl100k_base 初始化失败: {}", e))?;
        Ok(Self {
            bpe,
            kind_name: "cl100k_base",
            coefficient: 1.0,
        })
    }

    /// 创建 Claude 近似分词器（cl100k_base + 系数 1.05）
    ///
    /// Claude 官方未开源 tokenizer，业界经验：cl100k 计数 × 1.05 近似 Claude 实际 token 数。
    pub fn claude_approx() -> Result<Self, String> {
        let bpe = tiktoken_rs::cl100k_base().map_err(|e| format!("cl100k_base 初始化失败: {}", e))?;
        Ok(Self {
            bpe,
            kind_name: "claude_approx",
            coefficient: 1.05,
        })
    }

    /// 创建 DeepSeek 近似分词器（cl100k_base + 系数 1.1）
    ///
    /// DeepSeek 中文优化，token 数略多于 cl100k，系数 1.1 近似。
    pub fn deepseek_approx() -> Result<Self, String> {
        let bpe = tiktoken_rs::cl100k_base().map_err(|e| format!("cl100k_base 初始化失败: {}", e))?;
        Ok(Self {
            bpe,
            kind_name: "deepseek_approx",
            coefficient: 1.1,
        })
    }
}

impl Tokenizer for TiktokenTokenizer {
    fn count_tokens(&self, text: &str) -> usize {
        // tiktoken-rs 0.6: encode_with_special_tokens 返回 Vec<usize>（token id 列表）
        let tokens = self.bpe.encode_with_special_tokens(text);
        let raw_count = tokens.len();

        // 应用系数（Claude/DeepSeek 近似）
        if self.coefficient > 1.0 {
            ((raw_count as f32) * self.coefficient).round() as usize
        } else {
            raw_count
        }
    }

    fn name(&self) -> &str {
        self.kind_name
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_o200k_base_english() {
        let tk = TiktokenTokenizer::o200k_base().expect("o200k_base 应可用");
        let count = tk.count_tokens("Hello, world!");
        assert!(count > 0, "英文应能正确计数");
        assert!(count < 10, "短英文 token 数应 < 10");
    }

    #[test]
    fn test_o200k_base_chinese() {
        let tk = TiktokenTokenizer::o200k_base().expect("o200k_base 应可用");
        let count = tk.count_tokens("你好，世界！");
        assert!(count > 0, "中文应能正确计数");
    }

    #[test]
    fn test_cl100k_base_english() {
        let tk = TiktokenTokenizer::cl100k_base().expect("cl100k_base 应可用");
        let count = tk.count_tokens("Hello, world!");
        assert!(count > 0);
    }

    #[test]
    fn test_claude_approx_higher_than_cl100k() {
        // Claude 近似系数 1.05，token 数应略高于 cl100k 原始计数
        let cl100k = TiktokenTokenizer::cl100k_base().expect("cl100k_base 应可用");
        let claude = TiktokenTokenizer::claude_approx().expect("claude_approx 应可用");

        let text = "这是一段测试文本，用于验证 Claude 近似分词器的系数是否生效。Hello world.";
        let raw = cl100k.count_tokens(text);
        let approx = claude.count_tokens(text);

        // 系数 1.05，近似值应 >= 原始值（允许浮点舍入误差）
        assert!(
            approx >= raw,
            "Claude 近似 ({}) 应 >= cl100k 原始 ({})",
            approx,
            raw
        );
    }

    #[test]
    fn test_deepseek_approx_higher_than_cl100k() {
        let cl100k = TiktokenTokenizer::cl100k_base().expect("cl100k_base 应可用");
        let deepseek = TiktokenTokenizer::deepseek_approx().expect("deepseek_approx 应可用");

        let text = "这是一段测试文本，用于验证 DeepSeek 近似分词器的系数是否生效。";
        let raw = cl100k.count_tokens(text);
        let approx = deepseek.count_tokens(text);

        // 系数 1.1，近似值应 > 原始值（中长文本差异更明显）
        assert!(
            approx > raw,
            "DeepSeek 近似 ({}) 应 > cl100k 原始 ({})",
            approx,
            raw
        );
    }

    #[test]
    fn test_name() {
        let tk = TiktokenTokenizer::o200k_base().expect("o200k_base 应可用");
        assert_eq!(tk.name(), "o200k_base");
    }

    #[test]
    fn test_long_text() {
        let tk = TiktokenTokenizer::o200k_base().expect("o200k_base 应可用");
        let long_text = "Hello world. ".repeat(1000);
        let count = tk.count_tokens(&long_text);
        assert!(count > 1000, "长文本 token 数应 > 1000");
    }
}
