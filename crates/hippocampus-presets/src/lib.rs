//! # Hippocampus 预设组合层
//!
//! 5 个特配 crate 的组合层，提供 Builder + 叠加引擎 + 联动机制。
//!
//! ## 架构定位
//!
//! ```text
//!                ┌──────────────────────┐
//!                │ hippocampus-presets  │ ← 本 crate（组合层）
//!                └──────────┬───────────┘
//!                           │
//!    ┌──────────┬───────────┼───────────┬──────────┐
//!    ▼          ▼           ▼           ▼          ▼
//! ┌────────┐┌──────────┐┌─────────┐┌──────────┐┌────────┐
//! │ models ││scenarios ││windows  ││ agents   ││skills  │ ← 5 个特配 crate（平行）
//! └────┬───┘└────┬─────┘└────┬────┘└────┬─────┘└───┬────┘
//!      │         │           │          │          │
//!      ▼         ▼           ▼          ▼          ▼
//!    ┌──────────────────────────────────────────────────┐
//!    │                  hippocampus-core                │ ← 核心依赖
//!    └──────────────────────────────────────────────────┘
//! ```
//!
//! ## 核心职责
//!
//! 1. **Builder**：链式收集 5 个可选 Profile + 用户覆盖参数
//! 2. **联动机制**：Agent → Window 自动推导（Claude Code → ClaudeCodeCompact 等）
//! 3. **叠加引擎**：解析字段优先级，生成最终生效值
//!
//! ## 优先级链
//!
//! 字段冲突时的解析顺序（高 → 低）：
//!
//! ```text
//! 用户显式参数 > 场景（Scenario）> 模型（Model）> 窗口（Window）> 技能（Skill）> Agent > 默认
//! ```
//!
//! ### 摘要模板优先级
//!
//! ```text
//! 用户 custom > ScenarioProfile.custom_summary_template > SummaryFocus 预设 > 默认硬编码
//! ```
//!
//! ### 归档阈值优先级
//!
//! ```text
//! 用户 > ScenarioProfile.archive_threshold > ModelVariant.archive_strategy.threshold() > 默认 400K
//! ```
//!
//! ## 联动规则
//!
//! 当 Agent 已设置但 Window 未设置时，自动推导 Window：
//!
//! | Agent | 推导 Window |
//! |---|---|
//! | ClaudeCode | WindowProfile::claude_code()（ClaudeCodeCompact, 180K） |
//! | Cursor | WindowProfile::cursor()（CursorChat, 150K） |
//! | Trae | WindowProfile::trae()（TraeConversation, 120K） |
//! | Codex | WindowProfile::codex()（CodexRolling, 100K） |
//! | 其他 | WindowProfile::default()（GenericSliding, 100K） |
//!
//! ## 使用示例
//!
//! ```rust
//! use hippocampus_presets::PresetBuilder;
//! use hippocampus_agents::AgentProfile;
//! use hippocampus_scenarios::{Scenario, ScenarioProfile};
//!
//! let combined = PresetBuilder::new()
//!     .with_agent(AgentProfile::claude_code())
//!     .with_scenario(ScenarioProfile::from_scenario(Scenario::Coding))
//!     .build()
//!     .unwrap();
//!
//! // combined.archive_threshold() 返回解析后的归档阈值
//! // combined.summary_template() 返回解析后的摘要模板
//! ```

pub mod builder;
pub mod combined;
pub mod linkage;

pub use builder::{build_from_strings, scenario_from_str, PresetBuilder};
pub use combined::CombinedProfile;
pub use linkage::derive_window_from_agent;
