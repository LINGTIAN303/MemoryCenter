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
            // 2026 年 7 月最新主流型号（已核查官方文档）
            let variants = vec![
                // Claude 家族（5 个型号，覆盖 Opus/Sonnet/Fable/Mythos 级）
                ("claude-opus-4.6", ModelVariant::claude_opus_4_6()),
                ("claude-opus-4.8", ModelVariant::claude_opus_4_8()),
                ("claude-sonnet-5", ModelVariant::claude_sonnet_5()),
                ("claude-fable-5", ModelVariant::claude_fable_5()),
                ("claude-mythos-5", ModelVariant::claude_mythos_5()),
                // 其他家族
                ("gpt-5.2", ModelVariant::gpt_5_2()),
                ("gpt-5-codex", ModelVariant::gpt_5_codex()),
                ("gemini-3.1-pro", ModelVariant::gemini_3_1_pro()),
                ("deepseek-v4-pro", ModelVariant::deepseek_v4_pro()),
                ("deepseek-v4-flash", ModelVariant::deepseek_v4_flash()),
                ("qwen-3-coder", ModelVariant::qwen_3_coder()),
                ("llama-4-scout", ModelVariant::llama_4_scout()),
                ("llama-4-maverick", ModelVariant::llama_4_maverick()),
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
    /// 支持的名称（2026 年 7 月，已核查官方文档）：
    /// - Claude 家族（5 个）：`claude-opus-4.6` / `claude-opus-4.8` / `claude-sonnet-5` / `claude-fable-5` / `claude-mythos-5`
    /// - `gpt-5.2` / `gpt-5-codex`
    /// - `gemini-3.1-pro`
    /// - `deepseek-v4-pro` / `deepseek-v4-flash`
    /// - `qwen-3-coder`
    /// - `llama-4-scout` / `llama-4-maverick`
    /// - `grok-4.1`
    /// - `local-default`
    pub fn find(name: &str) -> Option<ModelVariant> {
        Self::init().get(name).cloned()
    }

    /// 获取家族的默认型号（最新稳定版本）
    ///
    /// 每个家族返回其最新主流型号：
    /// - Claude → Opus 4.8（API 普遍可用的稳定旗舰）
    /// - GPT → GPT-5.2
    /// - Gemini → Gemini 3.1 Pro
    /// - DeepSeek → V4-Pro
    /// - Qwen → Qwen3-Coder
    /// - Llama → Llama 4 Scout
    /// - Grok → Grok 4.1
    /// - Local → local-default
    /// - Custom → custom("custom", Custom, 32K)（中性预设）
    ///
    /// **说明**：Claude 默认选 Opus 4.8 而非 Fable 5/Mythos 5，原因：
    /// - Fable 5 曾因出口管制暂停，稳定性待观察
    /// - Mythos 5 面向特定合作方，普通用户难访问
    /// - Opus 4.8 为 API 普遍可用的稳定旗舰
    pub fn default_variant(family: ModelFamily) -> ModelVariant {
        match family {
            ModelFamily::Claude => ModelVariant::claude_opus_4_8(),
            ModelFamily::Gpt => ModelVariant::gpt_5_2(),
            ModelFamily::Gemini => ModelVariant::gemini_3_1_pro(),
            ModelFamily::DeepSeek => ModelVariant::deepseek_v4_pro(),
            ModelFamily::Qwen => ModelVariant::qwen_3_coder(),
            ModelFamily::Llama => ModelVariant::llama_4_scout(),
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
        assert_eq!(v.name, "claude-opus-4.8");
    }

    #[test]
    fn test_default_variant_gpt() {
        let v = ModelRegistry::default_variant(ModelFamily::Gpt);
        assert_eq!(v.name, "gpt-5.2");
    }

    #[test]
    fn test_default_variant_gemini() {
        let v = ModelRegistry::default_variant(ModelFamily::Gemini);
        assert_eq!(v.name, "gemini-3.1-pro");
    }

    #[test]
    fn test_default_variant_deepseek_v4() {
        let v = ModelRegistry::default_variant(ModelFamily::DeepSeek);
        assert_eq!(v.name, "deepseek-v4-pro");
    }

    #[test]
    fn test_default_variant_qwen_coder() {
        let v = ModelRegistry::default_variant(ModelFamily::Qwen);
        assert_eq!(v.name, "qwen-3-coder");
    }

    #[test]
    fn test_default_variant_llama_4_scout() {
        let v = ModelRegistry::default_variant(ModelFamily::Llama);
        assert_eq!(v.name, "llama-4-scout");
    }

    #[test]
    fn test_all_variants_count() {
        let count = ModelRegistry::all_variants().count();
        assert_eq!(count, 15, "应内置 15 个型号");
    }

    #[test]
    fn test_variants_by_family() {
        let claude_variants = ModelRegistry::variants_by_family(ModelFamily::Claude);
        assert_eq!(claude_variants.len(), 5); // opus-4.6, opus-4.8, sonnet-5, fable-5, mythos-5
    }

    #[test]
    fn test_variants_by_family_deepseek() {
        let deepseek_variants = ModelRegistry::variants_by_family(ModelFamily::DeepSeek);
        assert_eq!(deepseek_variants.len(), 2); // v4-pro, v4-flash
    }

    #[test]
    fn test_variants_by_family_llama() {
        let llama_variants = ModelRegistry::variants_by_family(ModelFamily::Llama);
        assert_eq!(llama_variants.len(), 2); // scout, maverick
    }

    #[test]
    fn test_all_families_have_default() {
        for family in ModelFamily::all() {
            let v = ModelRegistry::default_variant(family);
            assert_eq!(v.family, family, "家族 {:?} 的默认型号家族不匹配", family);
        }
    }
}
