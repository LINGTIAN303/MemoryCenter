//! # 模型注册表（Registry）
//!
//! 维护家族 → 默认型号的映射，提供按家族快速获取默认 ModelVariant 的能力。
//!
//! ## 设计目的
//!
//! 用户指定家族（如 `ModelFamily::Claude`）但未指定具体型号时，
//! Registry 返回该家族的最新（默认）ModelVariant。

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::family::ModelFamily;
use crate::variant::ModelVariant;

/// 模型注册表
///
/// 维护家族 → 默认 ModelVariant 的映射。
///
/// # 用法
///
/// ```no_run
/// use hippocampus_models::{ModelFamily, ModelRegistry, ModelVariant};
///
/// // 获取家族默认型号
/// let variant = ModelRegistry::default_variant(ModelFamily::Claude);
/// assert_eq!(variant.family, ModelFamily::Claude);
///
/// // 列出所有内置型号
/// for (name, variant) in ModelRegistry::all_variants() {
///     println!("{}: {}", name, variant);
/// }
/// ```
pub struct ModelRegistry;

/// 全局注册表（懒加载，只初始化一次）
static REGISTRY: OnceLock<HashMap<String, ModelVariant>> = OnceLock::new();

impl ModelRegistry {
    /// 初始化注册表（内部，只调用一次）
    fn init() -> &'static HashMap<String, ModelVariant> {
        REGISTRY.get_or_init(|| {
            let mut map = HashMap::new();
            // 2026 年 7 月最新主流型号
            let variants = vec![
                ("claude-opus-4.6", ModelVariant::claude_opus_4_6()),
                ("claude-opus-4.5", ModelVariant::claude_opus_4_5()),
                ("claude-sonnet-4.5", ModelVariant::claude_sonnet_4_5()),
                ("gpt-5.2", ModelVariant::gpt_5_2()),
                ("gpt-5-codex", ModelVariant::gpt_5_codex()),
                ("gemini-3-pro", ModelVariant::gemini_3_pro()),
                ("deepseek-v3.2", ModelVariant::deepseek_v3_2()),
                ("deepseek-r1", ModelVariant::deepseek_r1()),
                ("qwen-3", ModelVariant::qwen_3()),
                ("llama-4", ModelVariant::llama_4()),
                ("grok-4.1", ModelVariant::grok_4_1()),
                ("local-default", ModelVariant::local_default()),
            ];
            for (name, variant) in variants {
                map.insert(name.to_string(), variant);
            }
            map
        })
    }

    /// 按名称查找型号
    ///
    /// 支持的名称（2026 年 7 月）：
    /// - `claude-opus-4.6` / `claude-opus-4.5` / `claude-sonnet-4.5`
    /// - `gpt-5.2` / `gpt-5-codex`
    /// - `gemini-3-pro`
    /// - `deepseek-v3.2` / `deepseek-r1`
    /// - `qwen-3` / `llama-4` / `grok-4.1`
    /// - `local-default`
    pub fn find(name: &str) -> Option<ModelVariant> {
        Self::init().get(name).cloned()
    }

    /// 获取家族的默认型号（最新版本）
    ///
    /// 每个家族返回其最新主流型号：
    /// - Claude → Opus 4.6
    /// - GPT → GPT-5.2
    /// - Gemini → Gemini 3 Pro
    /// - DeepSeek → V3.2
    /// - Qwen → Qwen 3
    /// - Llama → Llama 4
    /// - Grok → Grok 4.1
    /// - Local → local-default
    /// - Custom → custom("custom", Custom, 32K)（中性预设）
    pub fn default_variant(family: ModelFamily) -> ModelVariant {
        match family {
            ModelFamily::Claude => ModelVariant::claude_opus_4_6(),
            ModelFamily::Gpt => ModelVariant::gpt_5_2(),
            ModelFamily::Gemini => ModelVariant::gemini_3_pro(),
            ModelFamily::DeepSeek => ModelVariant::deepseek_v3_2(),
            ModelFamily::Qwen => ModelVariant::qwen_3(),
            ModelFamily::Llama => ModelVariant::llama_4(),
            ModelFamily::Grok => ModelVariant::grok_4_1(),
            ModelFamily::Local => ModelVariant::local_default(),
            ModelFamily::Custom => ModelVariant::custom("custom", ModelFamily::Custom, 32_000),
        }
    }

    /// 列出所有内置型号
    ///
    /// 返回 (型号名称, ModelVariant) 的迭代器
    pub fn all_variants() -> impl Iterator<Item = (&'static String, &'static ModelVariant)> {
        Self::init().iter()
    }

    /// 列出指定家族的所有型号
    pub fn variants_by_family(family: ModelFamily) -> Vec<(&'static String, &'static ModelVariant)> {
        Self::init()
            .iter()
            .filter(|(_, v)| v.family == family)
            .collect()
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_by_name() {
        let v = ModelRegistry::find("claude-opus-4.6");
        assert!(v.is_some());
        assert_eq!(v.unwrap().family, ModelFamily::Claude);
    }

    #[test]
    fn test_find_nonexistent() {
        let v = ModelRegistry::find("nonexistent-model");
        assert!(v.is_none());
    }

    #[test]
    fn test_default_variant_claude() {
        let v = ModelRegistry::default_variant(ModelFamily::Claude);
        assert_eq!(v.family, ModelFamily::Claude);
        assert_eq!(v.name, "claude-opus-4.6");
    }

    #[test]
    fn test_default_variant_gpt() {
        let v = ModelRegistry::default_variant(ModelFamily::Gpt);
        assert_eq!(v.name, "gpt-5.2");
    }

    #[test]
    fn test_default_variant_gemini() {
        let v = ModelRegistry::default_variant(ModelFamily::Gemini);
        assert_eq!(v.name, "gemini-3-pro");
    }

    #[test]
    fn test_all_variants_count() {
        let count = ModelRegistry::all_variants().count();
        assert_eq!(count, 12, "应内置 12 个型号");
    }

    #[test]
    fn test_variants_by_family() {
        let claude_variants = ModelRegistry::variants_by_family(ModelFamily::Claude);
        assert_eq!(claude_variants.len(), 3); // opus-4.6, opus-4.5, sonnet-4.5
    }

    #[test]
    fn test_all_families_have_default() {
        for family in ModelFamily::all() {
            let v = ModelRegistry::default_variant(family);
            assert_eq!(v.family, family, "家族 {:?} 的默认型号家族不匹配", family);
        }
    }
}
