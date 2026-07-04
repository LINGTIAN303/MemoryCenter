//! # 字符级分词器（无依赖兜底）
//!
//! 当 tiktoken-rs 不可用或初始化失败时，用字符级近似计算 token 数。
//!
//! ## 近似规则
//!
//! - 中文/日文/韩文（CJK）：1 字符 ≈ 1.5 token（BPE 通常将 1 个 CJK 字符拆为 1-2 token）
//! - 英文/拉丁文：按空格分词，1 词 ≈ 1.3 token（平均）
//! - 标点/数字：1 字符 ≈ 0.5 token

use crate::tokenizer::Tokenizer;

/// 字符级分词器（无依赖兜底）
///
/// 用于：
/// - tiktoken-rs 不可用时的降级
/// - Gemini/Llama/Qwen 等未集成 sentencepiece 时的近似
/// - 本地模型无需精确 token 计数的场景
pub struct CharTokenizer {
    /// CJK 字符的 token 系数（默认 1.5）
    cjk_coefficient: f32,
    /// 拉丁文单词的 token 系数（默认 1.3）
    latin_coefficient: f32,
}

impl CharTokenizer {
    /// 创建默认字符级分词器
    pub fn new() -> Self {
        Self {
            cjk_coefficient: 1.5,
            latin_coefficient: 1.3,
        }
    }

    /// 创建带自定义系数的字符级分词器
    ///
    /// # 参数
    /// - `cjk_coefficient`：CJK 字符的 token 系数
    /// - `latin_coefficient`：拉丁文单词的 token 系数
    pub fn with_coefficients(cjk_coefficient: f32, latin_coefficient: f32) -> Self {
        Self {
            cjk_coefficient,
            latin_coefficient,
        }
    }

    /// 判断字符是否为 CJK（中文/日文/韩文）
    fn is_cjk(c: char) -> bool {
        let code = c as u32;
        // CJK 统一表意文字：U+4E00 - U+9FFF
        // CJK 扩展 A：U+3400 - U+4DBF
        // 日文平假名：U+3040 - U+309F
        // 日文片假名：U+30A0 - U+30FF
        // 韩文谚文音节：U+AC00 - U+D7AF
        (0x4E00..=0x9FFF).contains(&code)
            || (0x3400..=0x4DBF).contains(&code)
            || (0x3040..=0x309F).contains(&code)
            || (0x30A0..=0x30FF).contains(&code)
            || (0xAC00..=0xD7AF).contains(&code)
    }

    /// 判断字符是否为拉丁字母或数字
    fn is_latin_alnum(c: char) -> bool {
        c.is_ascii_alphanumeric()
    }
}

impl Default for CharTokenizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Tokenizer for CharTokenizer {
    fn count_tokens(&self, text: &str) -> usize {
        let mut cjk_count: usize = 0;
        let mut latin_word_count: usize = 0;
        let mut other_count: usize = 0;
        let mut in_latin_word = false;

        for c in text.chars() {
            if Self::is_cjk(c) {
                cjk_count += 1;
                in_latin_word = false;
            } else if Self::is_latin_alnum(c) {
                if !in_latin_word {
                    latin_word_count += 1;
                    in_latin_word = true;
                }
            } else {
                // 标点、空格、emoji 等
                if !c.is_whitespace() {
                    other_count += 1;
                }
                in_latin_word = false;
            }
        }

        let cjk_tokens = (cjk_count as f32 * self.cjk_coefficient).round() as usize;
        let latin_tokens = (latin_word_count as f32 * self.latin_coefficient).round() as usize;
        let other_tokens = (other_count as f32 * 0.5).round() as usize;

        cjk_tokens + latin_tokens + other_tokens
    }

    fn name(&self) -> &str {
        "character_based"
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chinese_count() {
        let tk = CharTokenizer::new();
        // 4 个中文字符，系数 1.5，预期 6 token
        let count = tk.count_tokens("你好世界");
        assert_eq!(count, 6);
    }

    #[test]
    fn test_english_count() {
        let tk = CharTokenizer::new();
        // 2 个英文单词，系数 1.3，预期 3 token（2.6 四舍五入）
        let count = tk.count_tokens("hello world");
        assert_eq!(count, 3);
    }

    #[test]
    fn test_mixed_count() {
        let tk = CharTokenizer::new();
        // "你好" (2 CJK) + "world" (1 拉丁词) + "！" (1 标点)
        // CJK: 2 × 1.5 = 3
        // 拉丁: 1 × 1.3 = 1.3 → 1
        // 标点: 1 × 0.5 = 0.5 → 1
        // 合计: 3 + 1 + 1 = 5
        let count = tk.count_tokens("你好world！");
        assert_eq!(count, 5);
    }

    #[test]
    fn test_empty_string() {
        let tk = CharTokenizer::new();
        assert_eq!(tk.count_tokens(""), 0);
    }

    #[test]
    fn test_whitespace_only() {
        let tk = CharTokenizer::new();
        assert_eq!(tk.count_tokens("   \n\t  "), 0);
    }

    #[test]
    fn test_japanese_count() {
        let tk = CharTokenizer::new();
        // 5 个日文平假名字符，系数 1.5 → 7.5 → 8
        let count = tk.count_tokens("こんにちは");
        assert_eq!(count, 8); // 5 × 1.5 = 7.5 四舍五入 → 8
    }

    #[test]
    fn test_korean_count() {
        let tk = CharTokenizer::new();
        // 3 个韩文字符
        let count = tk.count_tokens("안녕하세요");
        assert!(count >= 4); // 至少 3 × 1.5 = 4.5 → 5（안녕하세요 是 5 个字符）
    }

    #[test]
    fn test_custom_coefficient() {
        let tk = CharTokenizer::with_coefficients(2.0, 1.0);
        // 4 个中文字符，系数 2.0，预期 8 token
        let count = tk.count_tokens("你好世界");
        assert_eq!(count, 8);
    }

    #[test]
    fn test_name() {
        let tk = CharTokenizer::new();
        assert_eq!(tk.name(), "character_based");
    }

    #[test]
    fn test_long_text() {
        let tk = CharTokenizer::new();
        let long_text = "你好世界".repeat(100);
        let count = tk.count_tokens(&long_text);
        assert_eq!(count, 600); // 400 字符 × 1.5
    }
}
