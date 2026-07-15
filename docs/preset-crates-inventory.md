# MemoryCenter 特配 Crate 配置参考

> 本文档是 [GitHub Wiki: Preset Crates](https://github.com/LINGTIAN303/MemoryCenter/wiki/Preset-Crates) 的镜像，亦可通过 `docs/` 目录离线浏览。

## 概述

MemoryCenter 通过 5 个特配 crate 提供配置能力，覆盖 **Agent、场景、技能、窗口、模型** 5 个维度。这些维度最终由 `memory-center-presets` 组合层统一装配成 `CombinedProfile`，驱动归档、检索、评分等行为。

本文档列出 5 个特配 crate 的全部内置项、预设值与字段定义，供用户查阅与定制参考。

**适用读者**：

- 想了解 MemoryCenter 默认配置的**使用者**
- 想扩展或自定义特配的**开发者**
- 想为社区贡献新 Agent / 场景 / 型号的**贡献者**

**文档版本**：2026-07-15 v1（基于源码同步）

---

## 如何使用本配置

5 个维度的组合通过 `memory-center-presets::PresetBuilder` 完成：

```rust
use memory_center_presets::PresetBuilder;
use memory_center_agents::{AgentFamily, AgentProfile};
use memory_center_scenarios::{Scenario, ScenarioProfile};
use memory_center_models::ModelVariant;

let combined = PresetBuilder::new()
    .with_agent(AgentProfile::from_family(AgentFamily::Trae))
    .with_scenario(ScenarioProfile::from_scenario(Scenario::Coding))
    .with_model(ModelVariant::claude_opus_4_8())
    .build()?;
```

也可通过以下方式以字符串参数构建：

- **MCP 工具**：`preset_build`（推荐 LLM 使用）
- **HTTP 端点**：`POST /api/v1/presets/build`
- **Python 绑定**：`memory_center.presets.build_from_strings(...)`

**优先级链**（高 → 低）：

```
用户显式参数 > Scenario > Model > Window > Skill > Agent > 默认
```

**联动规则**：Agent 已设但 Window 未设时，自动从 Agent 推导 Window（如 `ClaudeCode → 180K`、`Cursor → 150K`、`Trae → 120K`、`Codex → 100K`）。

---

## 章节 1：memory-center-agents（Agent 维度）

识别当前使用 MemoryCenter 的 Agent 工具（如 Claude Code / Cursor / Trae / Codex 等），提供 Agent 维度的默认配置。

源码：`crates/memory-center-agents/`

### 表 1.1 AgentFamily 枚举（11 内置 + Custom）

`is_mainstream()` 判定 4 主流；`supports_real_hook` 与 `is_opensource` 语义一致。

| 变体名 | display_name | 是否主流(4) | supports_real_hook | is_opensource | default_session_prefix |
|---|---|---|---|---|---|
| ClaudeCode | Claude Code | 是 | true | true | claude-code |
| Cursor | Cursor | 是 | false | false | cursor |
| Trae | Trae | 是 | false | false | trae |
| Codex | Codex | 是 | false | false | codex |
| Zcode | Zcode | 否 | false | false | zcode |
| OpenCode | OpenCode | 否 | true | true | opencode |
| Qoder | Qoder | 否 | false | false | qoder |
| WorkBuddy | WorkBuddy | 否 | false | false | workbuddy |
| CatPaw | CatPaw | 否 | false | false | catpaw |
| OpenClaw | OpenClaw | 否 | false | false | openclaw |
| Marvis | Marvis | 否 | false | false | marvis |
| Custom(String) | 用户传入字符串 | 否 | false | false | custom |

> 备注：`OpenCode` 虽不在 4 主流之列（`is_mainstream()` 返回 false），但因开源属性归入 Real Hook 阵营，且有专属 AgentProfile 预设（v2.52 阶段 2 补全）。

### 表 1.2 AgentProfile 5 专属预设（4 主流 + OpenCode）

5 专属预设通过 `claude_code()` / `cursor()` / `trae()` / `codex()` / `opencode()` 构造；其他 6 个 generic family + Custom 走 `generic()` 路径（`has_native_compression=false`）。

| family | supports_tool_calls | has_native_compression | archive_to_memory_center | session_prefix | 注释里的压缩比与保留轮次 |
|---|---|---|---|---|---|
| ClaudeCode | true | true | true | claude-code | /compact 10:1 压缩 + 摘要 |
| Cursor | true | true | true | cursor | chat 5:1 压缩 + 摘要 |
| Trae | true | true | true | trae | conversation 5:1 压缩 + 摘要 |
| Codex | true | true | true | codex | rolling 3:1 压缩，无摘要 |
| OpenCode | true | true | true | opencode | compaction 事件机制 + 摘要（sidecar 监听自动归档） |

> 备注：`generic(family)` 预设 `supports_tool_calls=true`、`has_native_compression=false`、`archive_to_memory_center=true`，`session_prefix` 取 `family.default_session_prefix()`。

### 表 1.3 AgentFingerprint 指纹（仅 4 主流有专属指纹）

3 层信号融合识别：MCP `client_info` → 父进程名 → 环境变量前缀。

| family | client_info_keywords | parent_process_keywords | env_var_prefixes |
|---|---|---|---|
| ClaudeCode | claude-code, claude_code, claudecode | claude, claude-code | CLAUDE_CODE_, CLAUDE_ |
| Cursor | cursor | cursor | CURSOR_ |
| Trae | trae | trae | TRAE_ |
| Codex | codex, openai-codex | codex | CODEX_ |

> 备注：其他 7 个 generic family 与 Custom 均返回 `AgentFingerprint::generic()`，三个字段均为空数组 `&[]`，`is_empty()` 返回 true，不参与自动识别。

### 表 1.4 HookMode 分类（v2.40）

`HookModeResolver::resolve(family)` 根据 `supports_real_hook()` 决定模式。

| family | HookMode(Real/Pseudo) | 说明 |
|---|---|---|
| OpenCode | Real | 开源，sidecar 监听 compaction 消息自动归档 |
| ClaudeCode | Real | 开源 CLI，可适配 /compact 命令 |
| Cursor | Pseudo | 闭源，LLM 自感知 token 主动调 archive |
| Trae | Pseudo | 闭源，LLM 自感知 token 主动调 archive |
| Codex | Pseudo | 闭源，LLM 自感知 token 主动调 archive |
| Zcode / Qoder / WorkBuddy / CatPaw / OpenClaw / Marvis | Pseudo | 闭源，LLM 自感知 |
| Custom | Pseudo | 默认降级为伪钩子（`HookMode::default() == Pseudo`） |

> 备注：`HookMode::as_str` 返回 `"real"` / `"pseudo"`，`from_str` 反向解析；`HookModeResolver::family_from_session_id` 可从 session_id 前缀反解 family。

### 表 1.5 扩展路线图

| 扩展方向 | 说明 |
|---|---|
| 6 个 generic 预设补专属 AgentProfile | Zcode/Qoder/WorkBuddy/CatPaw/OpenClaw/Marvis 当前走 `generic(family)`，可补专属压缩比与保留轮次（OpenCode 已于 v2.52 阶段 2 补专属预设） |
| 6 个 generic family 补专属指纹 | 当前返回空指纹无法被 `detect_agent_client` 自动识别，需补 client_info / 父进程 / 环境变量前缀 |
| HookMode 分类已完善 | OpenCode/ClaudeCode → Real（通过 `supports_real_hook()` 自动分类），其他 → Pseudo，无需额外改动 |
| AgentProfile 增加 variant 约束 | variant 当前为 `Option<String>`，可对 5 专属预设版本格式加枚举或正则约束 |

---

## 章节 2：memory-center-scenarios（场景维度）

识别 Agent 工作场景，为不同场景提供针对化记忆工作流程配置。每个场景对应 5 维特配：摘要焦点、评分权重、标签优先级、检索策略、归档阈值。

源码：`crates/memory-center-scenarios/`

### 表 2.1 Scenario 枚举（10 内置 + Custom）

默认为 `Daily`。

| 变体名 | display_name | 是否内置 |
|---|---|---|
| Coding | 编码场景 | 是 |
| Writing | 写作场景 | 是 |
| Research | 科研场景 | 是 |
| Daily | 日常场景 | 是 |
| Finance | 金融场景 | 是 |
| Design | 设计场景 | 是 |
| OfficeWork | 工作场景 | 是 |
| AgentCollaboration | Agent协作场景 | 是 |
| KnowledgeBase | 知识库场景 | 是 |
| LongProject | 长项目场景 | 是 |
| Custom(String) | 用户传入字符串 | 否 |

### 表 2.2 SummaryFocus 摘要焦点维度

10 个场景一对一映射，Custom 降级为 `General`。

| scenario | SummaryFocus 变体 | focus_dimensions（用逗号分隔） |
|---|---|---|
| Coding | Coding | 代码片段, 技术决策, bug 修复, 架构变更, 依赖变更 |
| Writing | Writing | 核心观点, 论据, 素材, 文章结构, 风格 |
| Research | Research | 假设, 研究方法, 实验数据, 结论, 引用文献 |
| Daily | Daily | 事件, 地点, 人物, 时间, 情感 |
| Finance | Finance | 交易明细, 金额, 时间, 风险, 收益, 标的 |
| Design | Design | 设计决策, 用户反馈, 迭代版本, 视觉要素, 交互流程 |
| OfficeWork | OfficeWork | 会议决议, 待办事项, 文档变更, 责任人, 截止日期 |
| AgentCollaboration | AgentCollaboration | Agent决策, 工具调用, 上下文迁移, 协作流程, 会话边界 |
| KnowledgeBase | KnowledgeBase | 知识主题, 定义, 分类, 引用, 标签 |
| LongProject | LongProject | 项目阶段, 里程碑, 决策, 风险, 待办 |
| Custom | General（降级） | 主题, 关键事实, 关键实体 |

### 表 2.3 ScoreWeights 评分权重

4 维权重（recency / access_frequency / topic_relevance / user_marked）之和应为 1.0，`validate()` 容差 0.01。

| scenario | recency | access_frequency | topic_relevance | user_marked | 备注（设计原则） |
|---|---|---|---|---|---|
| Coding | 0.15 | 0.15 | 0.50 | 0.20 | 主题相关性最高（代码相关性） |
| Writing | 0.15 | 0.15 | 0.40 | 0.30 | 主题相关性 + 用户标记 |
| Research | 0.10 | 0.20 | 0.50 | 0.20 | 主题相关性最高（研究主题稳定） |
| Daily | 0.50 | 0.20 | 0.15 | 0.15 | 时效性最高（近期事件重要） |
| Finance | 0.20 | 0.15 | 0.35 | 0.30 | 主题相关性 + 用户标记（交易决策关键） |
| Design | 0.15 | 0.15 | 0.35 | 0.35 | 用户标记最高（设计迭代主观性强） |
| OfficeWork | 0.35 | 0.25 | 0.20 | 0.20 | 时效性 + 访问频率（近期待办重要） |
| AgentCollaboration | 0.15 | 0.40 | 0.30 | 0.15 | 访问频率最高（跨 Agent 频繁访问） |
| KnowledgeBase | 0.10 | 0.35 | 0.25 | 0.30 | 访问频率最高（常用知识重要） |
| LongProject | 0.35 | 0.15 | 0.20 | 0.30 | 时效性 + 用户标记（近期里程碑） |
| Custom | 0.25 | 0.25 | 0.25 | 0.25 | balanced 均衡权重（`ScoreWeights::balanced()`） |

### 表 2.4 priority_tags 标签优先级

从高到低用 `>` 连接，不在列表中的标签优先级分值为 0.0。

| scenario | 优先级标签序列（从高到低） |
|---|---|
| Coding | CodeBlock > ToolCall > Thinking > Plan > Url > FileAttachment |
| Writing | Text > Citation > Url > Plan > FileAttachment |
| Research | Citation > Url > Text > Plan > Thinking > FileAttachment |
| Daily | Text > Image > Voice > Url > Video |
| Finance | CodeBlock > Text > Url > Status > Citation |
| Design | Image > Text > FileAttachment > Ui > Video |
| OfficeWork | FileAttachment > Text > Plan > Status > Url > Citation |
| AgentCollaboration | ToolCall > Thinking > CodeBlock > Text > Plan |
| KnowledgeBase | Citation > Text > Url > CodeBlock > FileAttachment |
| LongProject | Plan > Status > FileAttachment > Text > CodeBlock |
| Custom | （空 Vec） |

> 备注：Custom 场景 `priority_tags_for` 返回空 Vec，`tag_priority_score` 直接返回 0.0，不优先任何标签。

### 表 2.5 RetrievalStrategy 检索策略

Hybrid 权重之和应为 1.0。

| scenario | 策略类型 | bm25_weight | semantic_weight | requires_embedder |
|---|---|---|---|---|
| Coding | Hybrid | 0.45 | 0.55 | true |
| Writing | BM25Only | — | — | false |
| Research | Hybrid | 0.45 | 0.55 | true |
| Daily | BM25Only | — | — | false |
| Finance | Hybrid | 0.3 | 0.7 | true |
| Design | Hybrid | 0.3 | 0.7 | true |
| OfficeWork | Hybrid（default_hybrid） | 0.4 | 0.6 | true |
| AgentCollaboration | Hybrid | 0.3 | 0.7 | true |
| KnowledgeBase | Semantic | — | — | true |
| LongProject | Hybrid（default_hybrid） | 0.4 | 0.6 | true |
| Custom | Hybrid（default_hybrid） | 0.4 | 0.6 | true |

> 备注：`default_hybrid()` 常量返回 BM25 0.4 + 语义 0.6。未配置 Embedder 时，Semantic/Hybrid 由 search 层降级为 BM25Only。

### 表 2.6 archive_threshold 归档阈值

范围 200K-500K token。

| scenario | 阈值(token 数) |
|---|---|
| Coding | 500,000 |
| Research | 500,000 |
| Writing | 400,000 |
| Design | 400,000 |
| Daily | 200,000 |
| Finance | 400,000 |
| OfficeWork | 400,000 |
| AgentCollaboration | 400,000 |
| KnowledgeBase | 500,000 |
| LongProject | 500,000 |
| Custom | 400,000 |

### 表 2.7 扩展路线图

| 扩展方向 | 说明 |
|---|---|
| Custom 场景降级链 | SummaryFocus 降级 General、ScoreWeights 用 balanced、priority_tags 返回空、RetrievalStrategy 用 default_hybrid、archive_threshold 用 400K（设计本就如此，可按需覆盖） |
| 清理冗余依赖 | `Cargo.toml` 声明 `thiserror` / `tracing` 但源码未实际使用 |
| 场景自动识别能力 | 当前由调用方决定场景，未来可补充基于对话内容的场景探测（已部分由 `presets::HybridScenarioDetector` 实现） |

---

## 章节 3：memory-center-skills（技能维度）

识别 Agent 内置技能（Read / Write / Edit / Bash 等），提供技能输出的记忆链接策略。

源码：`crates/memory-center-skills/`

### 表 3.1 BuiltinSkill 枚举（15 内置 + Custom）

`produces_artifact` 仅 Write/Edit 为 true；`is_destructive` 含 Bash。

| 变体名 | display_name | category | produces_artifact | is_destructive | default_memory_link |
|---|---|---|---|---|---|
| Read | 读取文件 | FileOps | false | false | AttachedToTurn |
| Write | 写入文件 | FileOps | true | true | AttachedToTurn |
| Edit | 编辑文件 | FileOps | true | true | AttachedToTurn |
| Glob | 文件匹配 | FileOps | false | false | AttachedToTurn |
| Grep | 内容搜索 | FileOps | false | false | AttachedToTurn |
| LS | 列出目录 | FileOps | false | false | AttachedToTurn |
| Bash | 执行命令 | Execution | false | true | AttachedToTurn |
| Task | 子 Agent | Execution | false | false | AttachedToTurn |
| WebSearch | 网页搜索 | Web | false | false | AttachedToTurn |
| WebFetch | 抓取网页 | Web | false | false | AttachedToTurn |
| SearchCodebase | 语义搜索 | Search | false | false | AttachedToTurn |
| AskUserQuestion | 询问用户 | Interaction | false | false | AttachedToTurn |
| TodoWrite | 任务列表 | Planning | false | false | AttachedToTurn |
| Schedule | 定时任务 | Planning | false | false | SkipArchive |
| Skill | 执行技能 | Meta | false | false | AttachedToTurn |
| Custom(String) | 用户传入字符串 | Custom | false | false | AttachedToTurn |

> 备注：`default_memory_link_for(Schedule)` 返回 SkipArchive，其余 14 个内置技能 + Custom 均返回 AttachedToTurn。

### 表 3.2 SkillCategory 分类

| 变体名 | 包含的 BuiltinSkill |
|---|---|
| FileOps | Read, Write, Edit, Glob, Grep, LS |
| Execution | Bash, Task |
| Web | WebSearch, WebFetch |
| Search | SearchCodebase |
| Interaction | AskUserQuestion |
| Planning | TodoWrite, Schedule |
| Meta | Skill |
| Custom | Custom(String) |

### 表 3.3 MemoryLink 枚举

v2.52 阶段 4（P7 Phase 1）扩展为 4 种变体。

| 变体名 | display_name | archives() | is_attached_to_turn() | as_str | 说明 |
|---|---|---|---|---|---|
| AttachedToTurn | 附加到轮次 | true | true | "AttachedToTurn" | 技能输出附加到 MessageTurn.tool_calls，随轮次归档（默认，唯一可追溯变体） |
| SkipArchive | 不归档 | false | false | "SkipArchive" | 技能输出仅在当前会话窗口使用，不写入记忆文件 |
| StandaloneMemory | 独立记忆 | true | false | "StandaloneMemory" | 独立记忆文件，存到 sessions/{session_id}/standalone/，不绑定轮次（v2.52 P7 Phase 1 新增） |
| LinkedToProject | 项目级记忆 | true | false | "LinkedToProject" | 项目级记忆，存到 projects/{project_id}/linked/，跨 session 共享（v2.52 P7 Phase 1 新增） |

> 备注：
> - `from_str` 同时接受 PascalCase 与 snake_case（如 "attached_to_turn"）。`Default` 为 AttachedToTurn。
> - `archives()` 判断"是否写入记忆"（3 个变体 true，仅 SkipArchive false）。
> - `is_attached_to_turn()` 判断"是否绑定到具体轮次可追溯"（仅 AttachedToTurn true，v2.52 P7 Phase 1 新增）。
> - Phase 2 已于 v2.52 实现：Storage trait 扩展 standalone/ 与 linked/ 存储路径（4 方法 + LocalStorage 实现）+ Retriever 新增 retrieve_standalone/linked_memories + MCP/Server/Python/Node retrieve 工具增加 link_type 参数，10 单测通过。
> - Phase 3 已于 v2.52 阶段 6 实现：MCP/Server/Python/Node 各新增 write_standalone_memory + write_linked_memory 主动写入工具 + build_memory_file 辅助函数；AGENTS.md 新增第 8 章触发协议；workspace 全量测试通过。

### 表 3.4 SkillProfile 字段

| 字段名 | 类型 | 说明 | 默认值 |
|---|---|---|---|
| skill | BuiltinSkill | 技能标识 | `BuiltinSkill::default()` = Custom("unknown") |
| memory_link | MemoryLink | 记忆链接策略 | 由 `default_memory_link_for(&skill)` 推导 |
| enabled | bool | 是否启用（禁用的技能不调用） | true |
| note | Option<String> | 用户自定义备注 | None |

> 备注：`SkillProfile::new(skill)` 自动按映射表设置 memory_link；`validate()` 校验 destructive 技能（Write/Edit/Bash）必须 AttachedToTurn，不允许 SkipArchive / StandaloneMemory / LinkedToProject（破坏性操作需可追溯，v2.52 阶段 3 实现，阶段 4 P7 Phase 1 扩展为 `is_attached_to_turn()` 判定，覆盖新增的 3 个非绑定变体）。

### 表 3.5 扩展路线图

| 扩展方向 | 说明 |
|---|---|
| 完善 validate() 校验逻辑 | ✅ 已于 v2.52 阶段 3 实现：destructive 技能（Write/Edit/Bash）强制 AttachedToTurn，不允许 SkipArchive |
| MemoryLink v2 扩展 | ✅ 已于 v2.52 阶段 4 P7 Phase 1+2 实现：Phase 1 新增 StandaloneMemory / LinkedToProject 变体 + archives() 语义扩展 + is_attached_to_turn() 新增 + destructive 校验升级。Phase 2 Storage trait 扩展 4 方法 + LocalStorage 实现（standalone/ + linked/ 路径）+ Retriever 新增 retrieve_standalone/linked_memories + MCP/Server/Python/Node retrieve 工具增加 link_type 参数，10 单测通过。Phase 3（阶段 6）MCP/Server/Python/Node 各新增 write_standalone_memory + write_linked_memory + build_memory_file 辅助函数 + AGENTS.md 第 8 章触发协议，workspace 全量测试通过 |
| destructive 技能强制 AttachedToTurn | ✅ 已于 v2.52 阶段 3 实现（见上一行，已合并入 validate() 校验逻辑） |
| 清理冗余依赖 | `Cargo.toml` 声明 `thiserror` / `tracing` 但源码未实际使用 |

---

## 章节 4：memory-center-windows（窗口维度）

适配不同 Agent 工具自身的上下文压缩机制（注意："windows" 指记忆窗口，非操作系统）。

源码：`crates/memory-center-windows/`

### 表 4.1 CompressionScheme 枚举（6 变体）

`Default` 为 `GenericSliding { keep_recent_turns: 5, summary_on_compress: true }`。

| 变体名 | display_name | 压缩比 | keep_recent_turns | summary_on_compress | compresses | 默认触发阈值 |
|---|---|---|---|---|---|---|
| ClaudeCodeCompact | Claude Code /compact | 10.0 | 5 | true | true | 180,000 |
| CursorChat | Cursor Chat 压缩 | 5.0 | 3 | true | true | 150,000 |
| TraeConversation | Trae 对话压缩 | 5.0 | 4 | true | true | 120,000 |
| CodexRolling | Codex 滚动窗口 | 3.0 | 10 | false | true | 100,000 |
| GenericSliding | 通用滑动窗口 | 4.0 | 可配（默认 5） | 可配（默认 true） | true | 100,000 |
| NoCompression | 无压缩 | 1.0 | usize::MAX | false | false | usize::MAX |

> 备注：GenericSliding 的 `keep_recent_turns`/`summary_on_compress` 为构造时传入字段，默认值 5/true；NoCompression 触发阈值设为 `usize::MAX` 表示永不触发。

### 表 4.2 CooperationMode 枚举

v2.53 P8 起两种模式均支持。

| 变体名 | display_name | is_supported | 说明 |
|---|---|---|---|
| Independent | 独立模式 | true | Agent 工具独立管理上下文，MemoryCenter 被动接收归档 |
| Cooperative | 协作模式 | true | Agent 工具与 MemoryCenter 协同管理上下文，双向通信，v2.53 P8 实现 |

### 表 4.3 WindowProfile 预设方法

所有预设均通过 `from_scheme` 构造，`archive_to_memory_center` 默认 true。

| 预设方法 | scheme | trigger_threshold | archive_to_memory_center | 说明 |
|---|---|---|---|---|
| claude_code() | ClaudeCodeCompact | 180,000 | true | Claude Code 200K 窗口的 90% |
| cursor() | CursorChat | 150,000 | true | Cursor 约 150K |
| trae() | TraeConversation | 120,000 | true | Trae 约 120K |
| codex() | CodexRolling | 100,000 | true | Codex 约 100K |
| no_compression() | NoCompression | usize::MAX | true | 由 MemoryCenter 归档阈值控制 |
| from_scheme(default) | GenericSliding(5, true) | 100,000 | true | 默认通用滑动窗口 |

> 备注：`WindowProfile::default()` 走 `from_scheme(CompressionScheme::default())`，即 GenericSliding 默认配置。

### 表 4.4 WindowProfile 字段

| 字段名 | 类型 | 说明 |
|---|---|---|
| scheme | CompressionScheme | 压缩方式 |
| cooperation_mode | CooperationMode | 协作模式（MVP 仅 Independent） |
| trigger_threshold | usize | 触发压缩的阈值（token 数） |
| archive_to_memory_center | bool | 压缩时是否归档到 MemoryCenter |

> 备注：`validate()` 校验 `cooperation_mode.is_supported()` 与 `trigger_threshold != 0`（NoCompression 除外）。

### 表 4.5 扩展路线图

| 扩展方向 | 说明 |
|---|---|
| ✅ Cooperative 协作模式实现 | 已于 v2.53 P8 实现（cooperative.rs trait + 6 状态有限状态机 + retention.rs RetentionBuilder + CooperativeService + MCP 2 工具 + HTTP 2 端点 + windows is_supported() → true；详见 [cooperative-design.md](cooperative-design.md)） |
| ✅ 清理冗余依赖 | 已于 v2.52 阶段 1 清理（`Cargo.toml` 声明的 `memory-center-core` / `thiserror` / `tracing` 冗余依赖已删除） |
| 动态探测触发阈值 | 当前阈值静态硬编码，未来可根据 Agent 工具窗口大小动态调整 |

---

## 章节 5：memory-center-models（模型维度）

模型家族识别、型号参数描述、Tokenizer 抽象与实现；重依赖 `tiktoken-rs` 隔离于此 crate，不污染其他轻量 crate。

源码：`crates/memory-center-models/`

### 表 5.1 ModelFamily 枚举（9 家族）

家族稳定，新型号只需新增 ModelVariant 构造器。

| 变体名 | display_name | default_tokenizer |
|---|---|---|
| Claude | Anthropic Claude | ClaudeApprox |
| Gpt | OpenAI GPT | O200kBase |
| Gemini | Google Gemini | CharacterBased |
| DeepSeek | DeepSeek | DeepSeekApprox |
| Qwen | 阿里 Qwen | CharacterBased |
| Llama | Meta Llama | CharacterBased |
| Grok | xAI Grok | O200kBase |
| Local | 本地模型 | CharacterBased |
| Custom | 自定义模型 | CharacterBased |

> 备注：Gemini/Qwen/Llama 默认用 `TokenizerKind::spm_or_char()` 智能选择 —— 启用 `tokenizer-sentencepiece` feature 时用 SentencePiece 真实分词器，未启用时降级 CharacterBased 兜底（v2.53 P9 实现，详见 [sentencepiece-guide.md](sentencepiece-guide.md)）。

### 表 5.2 ModelVariant 内置型号（15 个）

15 个构造器逐个提取，数据 100% 来自源码。

| 构造器名 | name | family | context_window | tokenizer | supports_thinking | supports_vision | supports_audio | tool_call_format | archive_strategy | summary_max_tokens |
|---|---|---|---|---|---|---|---|---|---|---|
| claude_opus_4_6 | claude-opus-4.6 | Claude | 1,000,000 | ClaudeApprox | true | true | false | Anthropic | LargeWindow(400,000) | 1024 |
| claude_opus_4_8 | claude-opus-4.8 | Claude | 200,000 | ClaudeApprox | true | true | false | Anthropic | Standard(80,000) | 1024 |
| claude_sonnet_5 | claude-sonnet-5 | Claude | 200,000 | ClaudeApprox | true | true | false | Anthropic | Standard(80,000) | 1024 |
| claude_fable_5 | claude-fable-5 | Claude | 200,000 | ClaudeApprox | true | true | false | Anthropic | Standard(80,000) | 1024 |
| claude_mythos_5 | claude-mythos-5 | Claude | 200,000 | ClaudeApprox | true | true | false | Anthropic | Standard(80,000) | 1024 |
| gpt_5_2 | gpt-5.2 | Gpt | 128,000 | O200kBase | false | true | false | OpenAI | Standard(60,000) | 1024 |
| gpt_5_codex | gpt-5-codex | Gpt | 128,000 | O200kBase | false | true | false | OpenAI | Standard(60,000) | 1024 |
| gemini_3_1_pro | gemini-3.1-pro | Gemini | 1,000,000 | CharacterBased | true | true | true | Gemini | LargeWindow(400,000) | 1024 |
| deepseek_v4_pro | deepseek-v4-pro | DeepSeek | 1,000,000 | DeepSeekApprox | true | false | false | OpenAI | LargeWindow(200,000) | 1024 |
| deepseek_v4_flash | deepseek-v4-flash | DeepSeek | 1,000,000 | DeepSeekApprox | false | false | false | OpenAI | LargeWindow(200,000) | 1024 |
| qwen_3_coder | qwen-3-coder | Qwen | 256,000 | CharacterBased | false | false | false | OpenAI | Standard(100,000) | 1024 |
| llama_4_scout | llama-4-scout | Llama | 1,000,000 | CharacterBased | false | true | false | OpenAI | LargeWindow(200,000) | 1024 |
| llama_4_maverick | llama-4-maverick | Llama | 1,000,000 | CharacterBased | false | true | false | OpenAI | LargeWindow(200,000) | 1024 |
| grok_4_1 | grok-4.1 | Grok | 128,000 | O200kBase | false | true | false | OpenAI | Standard(60,000) | 1024 |
| local_default | local-default | Local | 8,000 | CharacterBased | false | false | false | None | SmallWindow(4,000) | 512 |

> 备注：
> - `claude_opus_4_6` 上下文 1M 为 Beta 版，正式版 200K。
> - `llama_4_scout` 理论支持 10M，保守取 1M（API 实际部署多为 1M）。
> - `local_default` 是唯一 `summary_max_tokens=512` 的型号，且 `tool_call_format=None`。
> - `gemini_3_1_pro` 是唯一 `supports_audio=true` 的型号。
> - `ModelVariant::custom` 不在上表中，其 `archive_strategy` 按 `context_window` 动态推导：≥200K → LargeWindow(ctx/5)，≥32K → Standard(ctx/4)，否则 SmallWindow(ctx/4)。

### 表 5.3 ArchiveStrategy 枚举

`hard_limit` = threshold × 1.5。

| 变体名 | threshold 示例 | hard_limit(1.5x) | 说明 |
|---|---|---|---|
| LargeWindow | 400,000 | 600,000 | 长窗口模型（≥200K），阈值高，单次归档多内容 |
| Standard | 80,000 | 120,000 | 标准窗口（32K-128K），标准归档 |
| SmallWindow | 4,000 | 6,000 | 小窗口（≤16K），频繁归档，摘要更精炼 |

### 表 5.4 ToolCallFormat 枚举

| 变体名 | 说明 |
|---|---|
| OpenAI | OpenAI function calling 格式（JSON），GPT/DeepSeek/Qwen/Llama/Grok 等使用 |
| Anthropic | Anthropic tool_use content block 格式，Claude 系列使用 |
| Gemini | Gemini function call 格式，Gemini 系列使用 |
| Xml | XML 标签格式，部分开源模型使用 |
| None | 无工具调用能力，local_default 使用 |

### 表 5.5 TokenizerKind 枚举

序列化时只存类型名称。

| 变体名 | type_name | 说明 | 系数 |
|---|---|---|---|
| O200kBase | "o200k_base" | GPT-4o/5 系列分词器 | 1.0 |
| Cl100kBase | "cl100k_base" | GPT-4/3.5 系列分词器（向后兼容） | 1.0 |
| ClaudeApprox | "claude_approx" | Claude 近似（cl100k + 系数 1.05） | 1.05 |
| DeepSeekApprox | "deepseek_approx" | DeepSeek 近似（cl100k + 系数 1.1） | 1.1 |
| CharacterBased | "character_based" | 字符级兜底（无依赖） | — |
| SentencePiece | "sentencepiece" | SentencePiece 真实分词器（v2.53 P9，仅启用 `tokenizer-sentencepiece` feature 时可用，需 `MEMORY_CENTER_SPM_MODEL_PATH` 环境变量） | 真实分词 |
| Custom(Arc<dyn Tokenizer>) | "custom" | 用户注入实现，不可序列化（反序列化回退 CharacterBased） | — |

> 备注：`build()` 在 tiktoken-rs 初始化失败时降级为 CharTokenizer；`from_type_name("custom")` 也回退为 CharacterBased。`SentencePiece` 变体仅在启用 `tokenizer-sentencepiece` feature 时编译存在；未启用 feature 时 `from_type_name("sentencepiece")` 自动降级为 CharacterBased（带 warn 日志）。

### 表 5.6 ModelRegistry 默认型号映射

`default_variant(family)` 返回家族最新主流型号。

| family | default_variant 名称 |
|---|---|
| Claude | claude-opus-4.8 |
| Gpt | gpt-5.2 |
| Gemini | gemini-3.1-pro |
| DeepSeek | deepseek-v4-pro |
| Qwen | qwen-3-coder |
| Llama | llama-4-scout |
| Grok | grok-4.1 |
| Local | local-default |
| Custom | custom（`custom("custom", Custom, 32_000)`） |

> 备注：Claude 默认选 Opus 4.8 而非 Fable 5/Mythos 5，原因：Fable 5 曾因出口管制暂停，Mythos 5 面向特定合作方，Opus 4.8 为 API 普遍可用的稳定旗舰。

### 表 5.7 CharTokenizer 系数

无依赖兜底实现。

| 字符类型 | 系数 | 说明 |
|---|---|---|
| CJK（中文/日文/韩文） | 1.5 | BPE 通常将 1 个 CJK 字符拆为 1-2 token |
| 拉丁文（英文/拉丁字母数字） | 1.3 | 按空格分词，1 词 ≈ 1.3 token |
| 标点/数字/其他非空白 | 0.5 | 1 字符 ≈ 0.5 token |

> 备注：CJK 判定覆盖 U+4E00-U+9FFF、CJK 扩展 A (U+3400-U+4DBF)、日文平假名 (U+3040-U+309F)、日文片假名 (U+30A0-U+30FF)、韩文谚文音节 (U+AC00-U+D7AF)。空白字符不计 token。`CharTokenizer::with_coefficients` 支持自定义系数。

### 表 5.8 扩展路线图

| 扩展方向 | 说明 |
|---|---|
| ✅ Tokenizer 接入 archive-core 主链路（v2.52 阶段 4） | 已实现：`ArchiveEngine::with_token_estimator` 闭包注入（避免 archive-core 依赖 models），server/mcp 3 处初始化点通过 `build_token_estimator_from_env` 注入，默认 `deepseek-v4-flash`（DeepSeekApprox），支持 `MEMORY_CENTER_TOKENIZER_MODEL` 环境变量覆盖；sidecar 保持 `chars/3` 兜底 |
| ✅ 集成 sentencepiece（v2.53 P9） | 已实现：feature gating（`tokenizer-sentencepiece` 默认禁用）+ `TokenizerKind::spm_or_char()` helper 智能切换 + `MEMORY_CENTER_SPM_MODEL_PATH` 环境变量驱动 + 自动降级链（feature 未启用 / 环境变量未设置 / 模型加载失败 → CharTokenizer）；Gemini/Qwen/Llama 家族默认 tokenizer 已切换为 `spm_or_char()`。详见 [sentencepiece-guide.md](sentencepiece-guide.md) |
| Custom Tokenizer 序列化保留 | 当前 `TokenizerKind::Custom` 序列化为 "custom"，反序列化回退为 CharacterBased，丢失原始实现 |
| 启用 Cl100kBase | 当前仅作为 ClaudeApprox/DeepSeekApprox 的基础，无独立型号引用 |

---

## 章节 6：5 个特配 crate 依赖关系汇总

5 个特配 crate 均依赖 `memory-center-core`；models 额外引入 `tiktoken-rs` 重依赖。被依赖次数基于 Cargo.toml 实际引用统计。

### 表 6.1 依赖关系矩阵

| crate | 依赖 core | 依赖其他特配 | 被哪些 crate 依赖 |
|---|---|---|---|
| memory-center-agents | 是 | 无 | adapter, archive-core, mcp, presets, sidecar, server, python（7 个） |
| memory-center-scenarios | 是 | 无 | mcp, presets, server, python（4 个） |
| memory-center-skills | 是 | 无 | presets, server, python（3 个） |
| memory-center-windows | 是 | 无 | presets, server, python（3 个） |
| memory-center-models | 是 | 无（tiktoken-rs 为外部依赖） | mcp, presets, server, python（4 个） |

> 备注：所有特配 crate 均仅依赖 `memory-center-core`，相互之间无直接依赖（通过 presets 层组合）。models 是唯一引入重依赖（tiktoken-rs 0.6）的特配 crate。

---

## 章节 7：跨 crate 扩展路线图汇总

汇总前 5 个章节的扩展方向，便于贡献者按优先级规划工作。

### 表 7.1 跨 crate 扩展路线图

| crate | 扩展方向 | 说明 | 建议优先级 |
|---|---|---|---|
| agents | 7 个 generic family 补专属指纹 | 当前返回空指纹无法被 `detect_agent_client` 自动识别 | 高（影响 MCP 启动自动识别） |
| agents | OpenCode 补专属 AgentProfile | generic 预设未体现其开源 / 可适配 sidecar 的特性 | 高 |
| models | Tokenizer 接入 archive-core 主链路 | ✅ 已于 v2.52 阶段 4 实现（闭包注入，server/mcp 注入 estimator，sidecar 保持 chars/3 兜底） | 高（影响归档阈值精度） |
| agents | 7 个 generic 预设补专属 AgentProfile | 走 generic 路径，has_native_compression 统一为 false | 中（按使用频率逐步补） |
| agents | 7 个 generic family 补 HookMode 分类 | 全部走 Pseudo 默认，OpenCode 已支持 Real 但未体现 | 中 |
| skills | 完善 validate() 校验逻辑 | ✅ 已于 v2.52 阶段 3 实现（destructive 强制 AttachedToTurn） | 中 |
| skills | MemoryLink v2 扩展 | ✅ 已于 v2.52 阶段 4 P7 Phase 1+2 实现（Phase 1 enum 扩展 + 校验升级；Phase 2 Storage trait 4 方法 + LocalStorage + Retriever + MCP/Server/Python/Node retrieve 工具 link_type 参数，10 单测通过）。Phase 3（阶段 6）4 个入口层新增 write_standalone/linked_memory 主动写入工具 + AGENTS.md 第 8 章触发协议，workspace 全量测试通过 | 中 |
| windows | Cooperative 协作模式实现 | ✅ 已于 v2.53 P8 Phase 1-6 实现（cooperative.rs trait + 6 状态有限状态机 + retention.rs RetentionBuilder + CooperativeService + MCP 2 工具 + HTTP 2 端点 + windows is_supported() → true，workspace 221+ 测试通过，详见 [cooperative-design.md](cooperative-design.md)） | 中（已完成） |
| models | 集成 sentencepiece | ✅ 已于 v2.53 P9 实现（feature gating + spm_or_char() helper + 环境变量驱动降级链，详见 [sentencepiece-guide.md](sentencepiece-guide.md)） | 中（已完成） |
| scenarios | 场景自动识别能力 | ✅ 已于 v2.50 archive-core 接入主链路（pre_compress + archive 两处调用 `resolve_effective_scenario`，server/mcp 3 处初始化点注入 `scenario_detector`） | 低（已部分由 HybridScenarioDetector 实现） |
| scenarios | 清理冗余依赖 | ✅ 已于 v2.52 阶段 1 清理（thiserror / tracing 声明但未使用，已删除） | 低 |
| windows | 清理冗余依赖 | ✅ 已于 v2.52 阶段 1 清理（memory-center-core / thiserror / tracing 声明但未使用，已删除） | 低 |
| models | Custom Tokenizer 序列化保留 | 反序列化回退为 CharacterBased | 低 |

### 表 7.2 潜在扩展点（v2+）

| crate | 扩展点 | 说明 |
|---|---|---|
| skills | MemoryLink v2 扩展 | ✅ 已于 v2.52 阶段 4 P7 Phase 1+2 实现（Phase 1 enum 扩展 + 校验升级；Phase 2 Storage trait 4 方法 + LocalStorage 实现 + Retriever retrieve_standalone/linked + MCP/Server/Python/Node link_type 参数，10 单测通过）。Phase 3（阶段 6）4 个入口层新增 write_standalone/linked_memory 主动写入工具 + AGENTS.md 第 8 章触发协议，workspace 全量测试通过 |
| windows | Cooperative 模式实现 | ✅ 已于 v2.53 P8 Phase 1-6 实现（cooperative.rs trait + 6 状态有限状态机 + retention.rs RetentionBuilder + CooperativeService + MCP 2 工具 pre_compress_hint/post_compress_ack + HTTP 2 端点 /api/v1/cooperative/* + windows is_supported() → true；详见 [cooperative-design.md](cooperative-design.md)） |
| models | Tokenizer 接入主链路 | ✅ 已于 v2.52 阶段 4 实现（`ArchiveEngine::with_token_estimator` 闭包注入 + `build_token_estimator_from_env` 环境变量驱动） |
| agents | 6 个 generic 预设补专属配置 | 为 Qoder/Zcode 等待补 Agent 补 AgentProfile + Fingerprint（OpenCode 已于 v2.52 阶段 2 补 AgentProfile，HookMode 已通过 supports_real_hook 自动分类） |
| agents | AgentProfile 增加 variant 约束 | 对 5 专属预设的版本格式做枚举或正则约束 |
| scenarios | 场景自动识别 | 当前由调用方决定场景，可补充基于对话内容的场景探测能力 |
| models | sentencepiece 集成 | ✅ 已于 v2.53 P9 实现（feature gating 默认禁用 + `spm_or_char()` helper + `MEMORY_CENTER_SPM_MODEL_PATH` 环境变量驱动降级链，详见 [sentencepiece-guide.md](sentencepiece-guide.md)） |
| skills | destructive 技能强制 AttachedToTurn | ✅ 已于 v2.52 阶段 3 实现（在 validate() 中约束 Write/Edit/Bash 不可设为 SkipArchive） |

---

## 贡献指南

如需新增 Agent family、场景、技能、压缩方式或模型型号，请按以下步骤贡献：

1. **修改对应 crate 的枚举定义**：在 `BuiltinSkill` / `Scenario` / `AgentFamily` / `CompressionScheme` / `ModelFamily` 中新增变体
2. **添加预设构造器**：如 `AgentProfile::xxx()` / `ModelVariant::xxx()` / `WindowProfile::xxx()`
3. **更新映射表**：如 `from_scenario()` / `default_memory_link_for()` / `default_archive_threshold()` 等场景映射
4. **更新本文档对应表格**：保持文档与源码同步
5. **提交 PR**：标题使用 `feat(preset-xxx): 新增 YYY` 格式（如 `feat(preset-agents): 新增 Windsurf family`）

详细贡献流程参见 [CONTRIBUTING.md](https://github.com/LINGTIAN303/MemoryCenter/blob/main/CONTRIBUTING.md)。

---

## 相关文档

- [Crate 选择指南](https://github.com/LINGTIAN303/MemoryCenter/wiki/Crate-Guide) — 各 Crate 的整体定位与选择决策
- [Architecture](https://github.com/LINGTIAN303/MemoryCenter/wiki/Architecture) — 整体架构设计
- [MCP Integration](https://github.com/LINGTIAN303/MemoryCenter/wiki/MCP-Integration) — MCP 工具列表（含 `preset_build` / `preset_list_*`）
- [API Reference](https://github.com/LINGTIAN303/MemoryCenter/wiki/API-Reference) — REST API 文档（含 `POST /api/v1/presets/build`）

---

> 本文档由源码同步，如配置变更请同步更新。最后更新：2026-07-15。
