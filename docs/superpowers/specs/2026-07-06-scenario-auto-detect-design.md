# 场景识别功能设计（v2.33）

**状态**：设计已确认，待写实施计划
**创建日期**：2026-07-06
**所属 crate**：hippocampus-presets / hippocampus-core
**版本**：v2.33

## 1. 背景与动机

### 1.1 现状痛点

当前场景识别机制（[detect.rs:305-308](../../../crates/hippocampus-presets/src/detect.rs#L305-L308)）：

- 启动时 `resolve_scenario_name(family)` 仅根据 Agent family 推导场景
- 或读环境变量 `HIPPOCAMPUS_PRESET_SCENARIO`
- **一次性识别，运行时不可变**

**痛点**：用户在 Trae 里做写作 / 研究 / 金融分析时，场景永远是 coding，导致：

| 维度 | 错配示例 |
|------|---------|
| 摘要 focus | 写作场景应关注"观点/论据"，实际用"代码片段/技术决策" |
| 评分权重 | 写作应 user_marked + topic_relevance 高，实际用 coding 的 topic_relevance=0.50 |
| 检索策略 | 写作应 BM25Only，实际用 coding 的 Hybrid（需 Embedder） |
| 归档阈值 | coding 500K，写作应是 400K |
| 标签优先级 | coding 优先 CodeBlock，写作应优先 Text/Citation |

### 1.2 目标

在**首次 archive 时**从对话内容识别场景，写入 session 元数据，后续该 session 的 archive 调用读取元数据应用识别场景。

## 2. 核心设计决策

| 决策点 | 选择 | 理由 |
|--------|------|------|
| 触发时机 | 首次 archive 时识别一次 | 捕捉会话主体场景，无需可变 CombinedProfile |
| 输入信号 | 仅对话内容 | 专注会话语义，避免环境信号噪音 |
| 识别算法 | 关键词规则 + LLM 兜底 | 平衡精度与成本，复用 HybridDetector 模式 |
| 输出回写 | 写入 session 元数据 | 跨进程持久，per-session 隔离 |
| 架构位置 | hippocampus-presets | 与现有 detect.rs 同位置，CombinedProfile 在此构建 |
| 实现方案 | 方案 A（轻量规范型） | 扩展 Storage trait + 独立 LLM 调用 + 不重建索引 |

## 3. 架构概览

```
┌─────────────────────────────────────────────────────────┐
│ hippocampus-presets                                     │
│                                                         │
│  ┌─────────────────────┐    ┌────────────────────────┐ │
│  │ KeywordScenarioDetect│    │ HttpScenarioDetector   │ │
│  │ (纯算法，零依赖)     │    │ (LLM 推断，复用        │ │
│  │                     │    │  HttpLlmDetector 模式)  │ │
│  │ 7 场景 × ~15 关键词  │    │                        │ │
│  │ 返回 (Scenario, f32)│    │ 输入对话摘要           │ │
│  └──────────┬──────────┘    └───────────┬────────────┘ │
│             │                           │              │
│             └──────────┬────────────────┘              │
│                        ▼                               │
│         ┌──────────────────────────────┐              │
│         │ HybridScenarioDetector       │              │
│         │ (串联关键词 + LLM 兜底)       │              │
│         │ 置信度 < 0.6 时调 LLM         │              │
│         └──────────────┬───────────────┘              │
│                        │                              │
│                        ▼ return DetectionResult       │
│  ┌─────────────────────────────────────────────────┐  │
│  │ resolve_effective_scenario (编排函数)           │  │
│  │ 1. 用户显式（preset 参数）最高                  │  │
│  │ 2. read_session_meta（已识别则跳过）            │  │
│  │ 3. 首次：调 detector 识别 + write_session_meta  │  │
│  │ 4. 降级：Agent 默认场景                         │  │
│  └─────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────┘
                        │
                        ▼
┌─────────────────────────────────────────────────────────┐
│ hippocampus-core (Storage trait 扩展)                   │
│  + async fn write_session_meta(sid, meta) -> Result<()> │
│  + async fn read_session_meta(sid) -> Result<Option<...>│
│                                                          │
│  LocalStorage: sessions/{sid}/meta.json                 │
│  SqliteStorage: session_meta 表                         │
└─────────────────────────────────────────────────────────┘
```

## 4. 组件设计

### 4.1 关键词规则字典

**文件**：`crates/hippocampus-presets/src/scenario_detect.rs`（新增）

**关键词集**（每场景约 15 个，含中英文）：

| 场景 | 关键词示例 |
|------|-----------|
| Coding | `fn`, `class`, `def`, `function`, `bug`, `compile`, `commit`, `refactor`, `API`, `函数`, `编译`, `重构`, `报错`, `调试`, `架构` |
| Writing | `文章`, `论点`, `论据`, `素材`, `风格`, `段落`, `开头`, `结尾`, `修辞`, `article`, `essay`, `draft`, `outline`, `narrative`, `tone` |
| Research | `假设`, `方法`, `数据`, `结论`, `引用`, `文献`, `实验`, `样本`, `论文`, `hypothesis`, `methodology`, `data`, `conclusion`, `citation`, `abstract` |
| Daily | `今天`, `昨天`, `吃饭`, `天气`, `心情`, `朋友`, `周末`, `电影`, `购物`, `约会`, `family`, `dinner`, `weather`, `mood`, `weekend` |
| Finance | `交易`, `金额`, `收益`, `风险`, `投资`, `股票`, `基金`, `利率`, `止损`, `portfolio`, `stock`, `bond`, `dividend`, `volatility`, `hedge` |
| Design | `设计`, `原型`, `用户`, `界面`, `交互`, `迭代`, `视觉`, `反馈`, `mockup`, `wireframe`, `UI`, `UX`, `persona`, `design`, `iteration` |
| OfficeWork | `会议`, `待办`, `文档`, `决议`, `项目`, `截止`, `参会`, `纪要`, `meeting`, `todo`, `memo`, `deadline`, `agenda`, `minutes`, `project` |

**匹配方式**：
- 子串匹配（大小写不敏感）
- 统计每个场景命中数
- 零命中场景不参与排名

**置信度计算**：
```rust
// top_score = 最高场景命中数
// second_score = 次高场景命中数
// confidence = top_score / (top_score + second_score)
// > 0.6 算高置信，直接采用
// ≤ 0.6 触发 LLM 兜底
// 全部零命中 → 直接 LLM 兜底
```

### 4.2 HttpScenarioDetector（LLM 推断器）

**文件**：`crates/hippocampus-presets/src/scenario_detect.rs`（同文件）

```rust
pub struct HttpScenarioDetector {
    config: LlmDetectorConfig,  // 复用 hippocampus-llm 现有配置
}

impl HttpScenarioDetector {
    pub fn new(config: LlmDetectorConfig) -> Self { ... }

    /// 输入对话前 N 轮（默认 10 轮）摘要，LLM 返回场景标签
    pub async fn detect(&self, turns: &[MessageTurn]) -> Option<Scenario>;
}
```

**Prompt 设计**：

```
你是一个场景识别器。请分析以下对话内容，判断属于哪个场景。

可选场景标签：
- coding: 编码场景（编程/调试/架构设计/code review）
- writing: 写作场景（文章/文档/创意写作）
- research: 科研场景（论文/实验/数据分析）
- daily: 日常场景（闲聊/咨询/生活）
- finance: 金融场景（交易/投资/风险分析）
- design: 设计场景（UI/UX/视觉/产品设计）
- officework: 工作场景（会议/文档/项目协作）

对话摘要（前 10 轮）：
{conversation_summary}

请以严格 JSON 格式返回（不要包含其他文本）：
{"scenario": "coding", "reason": "对话涉及 Rust 代码实现"}
```

**输出解析**：
- JSON 解析失败 → 返回 `None`
- 场景标签不在 7 个内置场景中 → 视为 `Custom(s)`
- LLM 网络错误 / 超时 → 返回 `None`

**配置**：
- 复用 `HIPPOCAMPUS_DETECTOR_API_URL` / `API_KEY` / `MODEL` / `TIMEOUT` / `MAX_TOKENS`
- 或新增独立前缀 `HIPPOCAMPUS_SCENARIO_DETECTOR_*`（默认回退到 DETECTOR 配置）
- **决策**：复用 DETECTOR 配置，避免用户配置负担（v2.33 简化）

### 4.3 HybridScenarioDetector（编排器）

```rust
pub struct HybridScenarioDetector {
    keyword: KeywordScenarioDetector,
    llm: Option<Arc<HttpScenarioDetector>>,  // None 时仅关键词
}

impl HybridScenarioDetector {
    pub fn new(llm: Option<Arc<HttpScenarioDetector>>) -> Self { ... }

    pub async fn detect(&self, turns: &[MessageTurn]) -> DetectionResult {
        // 1. 关键词规则优先
        if let Some((scenario, conf)) = self.keyword.detect(turns) {
            if conf >= 0.6 {
                return DetectionResult {
                    scenario,
                    confidence: conf,
                    method: "keyword",
                };
            }
        }
        // 2. LLM 兜底（若配置）
        if let Some(llm) = &self.llm {
            if let Some(scenario) = llm.detect(turns).await {
                return DetectionResult {
                    scenario,
                    confidence: 0.8,  // LLM 结果默认 0.8
                    method: "llm",
                };
            }
        }
        // 3. 全部失败
        DetectionResult::failed()
    }
}

pub struct DetectionResult {
    pub scenario: Option<Scenario>,
    pub confidence: f32,
    pub method: &'static str,  // "keyword" / "llm" / "failed"
}

impl DetectionResult {
    pub fn failed() -> Self {
        Self { scenario: None, confidence: 0.0, method: "failed" }
    }
    pub fn is_failed(&self) -> bool { self.scenario.is_none() }
}
```

### 4.4 SessionMeta + Storage trait 扩展

**文件**：`crates/hippocampus-core/src/storage.rs`（修改）

```rust
/// Session 元数据（v2.33 新增）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    /// 识别的场景标签（如 "coding" / "writing"）
    pub scenario: String,
    /// 置信度 0-1
    pub confidence: f32,
    /// 识别方法："keyword" / "llm" / "agent_default"
    pub method: &'static str,
    /// 识别时间（UTC）
    pub detected_at: chrono::DateTime<chrono::Utc>,
}

pub trait Storage: Send + Sync {
    // ... 现有方法 ...

    /// 写入 session 元数据（v2.33 新增）
    ///
    /// 覆盖写入（若已存在则替换）
    async fn write_session_meta(
        &self,
        session_id: &str,
        meta: &SessionMeta,
    ) -> crate::Result<()>;

    /// 读取 session 元数据（v2.33 新增）
    ///
    /// 未识别时返回 Ok(None)
    async fn read_session_meta(
        &self,
        session_id: &str,
    ) -> crate::Result<Option<SessionMeta>>;
}
```

**LocalStorage 实现**：
- 路径：`{root}/sessions/{sid}/meta.json`
- 写入：`tokio::fs::write` 序列化 JSON
- 读取：`tokio::fs::read_to_string` + 反序列化，文件不存在返回 `Ok(None)`

**SqliteStorage 实现**：
- 新增表：`session_meta(session_id TEXT PK, scenario TEXT, confidence REAL, method TEXT, detected_at TEXT)`
- 写入：`INSERT OR REPLACE`
- 读取：`SELECT ... WHERE session_id = ?`

**CachedStorage 实现**：
- 透传到 inner（不单独缓存，避免一致性复杂度）
- 文档注明后续 batch 优化时一并处理（session_meta 读取频率低，无需独立缓存）

### 4.5 优先级链（编排函数）

**文件**：`crates/hippocampus-presets/src/scenario_detect.rs`（同文件）

**依赖辅助函数 `scenario_to_str`**（与现有 `scenario_from_str` 对称，新增）：

```rust
/// Scenario 枚举 → 稳定字符串（用于 session_meta 持久化）
///
/// 与 `scenario_from_str` 互逆：
/// - Coding → "coding"
/// - Writing → "writing"
/// - ...
/// - OfficeWork → "officework"
/// - Custom(s) → "custom:s"（带前缀避免与内置场景冲突）
pub fn scenario_to_str(scenario: &Scenario) -> String {
    match scenario {
        Scenario::Coding => "coding".to_string(),
        Scenario::Writing => "writing".to_string(),
        Scenario::Research => "research".to_string(),
        Scenario::Daily => "daily".to_string(),
        Scenario::Finance => "finance".to_string(),
        Scenario::Design => "design".to_string(),
        Scenario::OfficeWork => "officework".to_string(),
        Scenario::Custom(s) => format!("custom:{}", s),
    }
}
```

`scenario_from_str` 需同步扩展支持 `custom:` 前缀解析（向后兼容，已有实现不识别 `custom:` 前缀时返回 `Custom(s)` 兜底）。

```rust
/// 解析生效的场景（v2.33 核心 API）
///
/// 优先级链：
/// 1. 用户显式（preset.scenario 参数）最高
/// 2. session 元数据（已识别）
/// 3. 首次 archive：触发识别 + 写入元数据
/// 4. 识别失败：Agent 默认场景
///
/// ## 参数
/// - storage: 存储 trait（读写 session_meta）
/// - session_id: 会话 ID
/// - user_explicit: 用户显式指定的场景（来自 preset.scenario）
/// - agent_family: Agent family（用于降级时推导默认场景）
/// - detector: 场景识别器
/// - turns: 对话内容（首次识别时用）
pub async fn resolve_effective_scenario(
    storage: &dyn Storage,
    session_id: &str,
    user_explicit: Option<&str>,
    agent_family: &AgentFamily,
    detector: &HybridScenarioDetector,
    turns: &[MessageTurn],
) -> Scenario {
    // 1. 用户显式最高
    if let Some(s) = user_explicit {
        tracing::debug!(scenario = %s, "场景识别：用户显式指定");
        return scenario_from_str(s);
    }

    // 2. session 元数据（已识别）
    match storage.read_session_meta(session_id).await {
        Ok(Some(meta)) => {
            tracing::debug!(
                scenario = %meta.scenario,
                confidence = meta.confidence,
                method = %meta.method,
                "场景识别：命中 session 元数据"
            );
            return scenario_from_str(&meta.scenario);
        }
        Ok(None) => { /* 首次识别，继续 */ }
        Err(e) => {
            tracing::warn!(error = %e, "读取 session_meta 失败，触发重新识别");
        }
    }

    // 3. 首次识别
    let result = detector.detect(turns).await;
    if let Some(scenario) = result.scenario {
        let meta = SessionMeta {
            scenario: scenario_to_str(&scenario),  // 稳定的字符串序列化
            confidence: result.confidence,
            method: result.method,
            detected_at: chrono::Utc::now(),
        };
        // 写入元数据（失败不阻塞）
        if let Err(e) = storage.write_session_meta(session_id, &meta).await {
            tracing::warn!(error = %e, "写入 session_meta 失败（不阻塞 archive）");
        }
        tracing::info!(
            scenario = ?scenario,
            confidence = result.confidence,
            method = %result.method,
            "场景识别完成"
        );
        return scenario;
    }

    // 4. 降级：Agent 默认场景
    let default = scenario_from_str(&resolve_scenario_name(agent_family));
    tracing::info!(
        default = ?default,
        "场景识别失败，降级到 Agent 默认场景"
    );
    default
}
```

## 5. 数据流

### 5.1 首次 archive 识别流程

```
LLM 调用 archive(session_id, turns_json, preset?)
   │
   ▼
archive handler 解析 turns + preset
   │
   ▼
resolve_effective_scenario(storage, sid, preset.scenario, agent_family, detector, turns)
   │
   ├─ preset.scenario 存在? → 用 preset.scenario（用户显式最高）
   │
   ├─ read_session_meta(sid) 存在? → 用 meta.scenario（已识别）
   │
   ├─ 首次 archive：detector.detect(turns)
   │     │
   │     ├─ KeywordScenarioDetector.detect(turns)
   │     │    → 命中且置信度 ≥ 0.6 → 返回
   │     │
   │     └─ HttpScenarioDetector.detect(turns)（LLM 兜底）
   │          → 返回 Scenario
   │
   ├─ write_session_meta(sid, meta)（持久化识别结果）
   │
   ▼
用识别的 Scenario 重新 build CombinedProfile
   │
   ▼
应用 summary_template / score_weights / priority_tags / retrieval_strategy
   │
   ▼
执行 archive（Archiver 用新 CombinedProfile 的参数）
```

### 5.2 后续该 session 的 archive

- `read_session_meta` 直接命中，跳过识别
- 无需再次调用 LLM
- 行为与首次识别后一致

### 5.3 后续该 session 的 semantic_search

- **当前设计不应用识别场景**（仅影响 archive）
- 原因：search 路径不经过 archive handler，且重建索引成本高
- 若需扩展，后续可在 search handler 也读 session_meta

## 6. 错误处理与降级

| 失败点 | 降级策略 | 行为 |
|--------|---------|------|
| 关键词零命中 | fallback 到 LLM | LLM 推断 |
| 关键词置信度 < 0.6 | fallback 到 LLM | LLM 推断 |
| LLM 未配置（无 API_URL） | 跳过 LLM | 用 Agent 默认场景 |
| LLM 调用失败（网络/超时） | 返回 None | 用 Agent 默认场景 |
| LLM 返回无法解析 | 返回 None | 用 Agent 默认场景 |
| write_session_meta 失败 | 日志 warn，不阻塞 | archive 仍执行，下次重新识别 |
| read_session_meta 失败 | 当作 None | 触发重新识别 |

**核心原则**：识别失败永远不阻塞 archive 主流程，降级到 Agent 默认场景。

## 7. 影响范围

### 7.1 修改的文件

| 文件 | 改动类型 | 说明 |
|------|---------|------|
| `crates/hippocampus-core/src/storage.rs` | 修改 | 新增 SessionMeta struct + Storage trait 2 个方法 + LocalStorage/SqliteStorage/CachedStorage 实现 |
| `crates/hippocampus-presets/src/scenario_detect.rs` | 新增 | KeywordScenarioDetector + HttpScenarioDetector + HybridScenarioDetector + resolve_effective_scenario |
| `crates/hippocampus-presets/src/lib.rs` | 修改 | 导出新模块 |
| `crates/hippocampus-presets/Cargo.toml` | 修改 | 新增 hippocampus-llm 依赖（HttpScenarioDetector 用） |
| `crates/hippocampus-mcp/src/lib.rs` | 修改 | archive handler 调用 resolve_effective_scenario，注入 HybridScenarioDetector |
| `crates/hippocampus-mcp/src/main.rs` | 修改 | build_scenario_detector 函数 + 注入 HippocampusMcp |
| `crates/hippocampus-server/src/handlers.rs` | 修改 | archive handler 调用 resolve_effective_scenario |

### 7.2 不修改的部分

- `crates/hippocampus-scenarios/*` — 场景数据 crate 保持纯数据，不引入 LLM 依赖
- `crates/hippocampus-core/src/archive.rs` — Archiver 本身不改，由 handler 注入识别后的 CombinedProfile
- 现有 `detect_agent_client` — Agent 识别逻辑保持不变，作为降级 fallback

## 8. 测试策略

| 测试类型 | 覆盖点 | 预期数量 |
|---------|--------|---------|
| 关键词检测单元测试 | 7 场景 × 明显对话样本 → 正确识别 + 置信度 > 0.6 | 7 |
| 关键词边界测试 | 空对话 / 混合场景 / 零命中 → 正确返回 None | 3 |
| 关键词置信度测试 | 单场景命中（高置信）vs 双场景命中（低置信） | 2 |
| LLM 检测单元测试 | Mock LLM 客户端，验证 prompt 构造 + 响应解析 | 3 |
| LLM 失败降级测试 | 网络错误 / 超时 / JSON 解析失败 → 返回 None | 3 |
| Hybrid 串联测试 | 关键词高置信跳过 LLM / 关键词低置信触发 LLM / LLM 失败降级 | 3 |
| 优先级链测试 | 用户显式 > session_meta > 识别 > Agent 默认 | 4 |
| Storage trait 测试 | LocalStorage + SqliteStorage 读写 session_meta | 4 |
| 集成测试 | 完整 archive 流程，验证识别 → 写元数据 → 应用场景 | 2 |
| 降级测试 | LLM 未配置 / LLM 失败 / write 失败 → 不阻塞 archive | 3 |

**预期总测试数**：约 34 个新增测试

## 9. MCP 工具影响

### 9.1 archive 工具

- **内部自动识别，对 LLM 透明**（无需新增参数）
- LLM 调用 `archive(session_id, turns_json)` 不变
- handler 内部调用 `resolve_effective_scenario`

### 9.2 get_config 工具扩展（可选，v2.33.1）

- `scope=session` 时返回各 session 的识别场景
- 让 LLM 可查询当前 session 识别的场景

### 9.3 新增可选 MCP 工具（可选，v2.33.1）

- `get_session_scenario(session_id)` — 查询当前 session 识别的场景
- 非必需，留待后续观察必要性

## 10. 不做的事（YAGNI）

- ❌ 不做运行时动态切换（用户已选首次 archive 识别）
- ❌ 不做环境信号输入（用户已选仅对话内容）
- ❌ 不做 Embedding 相似度识别（关键词 + LLM 已够）
- ❌ 不重建已有索引（影响范围仅 archive）
- ❌ 不做 Top-K 候选场景（单一场景足够）
- ❌ 不做场景配置文件（硬编码关键词足够，配置化留待后续）
- ❌ 不做 LLM 调用合并到 SummaryGenerator（保持职责单一）

## 11. 版本与向后兼容

### 11.1 版本号

- **v2.33**：场景识别功能

### 11.2 向后兼容

- Storage trait 新增方法：LocalStorage/SqliteStorage/CachedStorage 同步实现，无破坏
- archive handler：识别逻辑在 handler 内部，对外 API 签名不变
- 未配置 LLM 时：仅用关键词规则，仍能识别明显场景
- 识别失败：降级到 Agent 默认场景（与 v2.32 行为一致）

### 11.3 配置

- 复用现有 `HIPPOCAMPUS_DETECTOR_*` 环境变量（HttpScenarioDetector 共用 LLM 配置）
- 不新增环境变量（简化用户配置负担）
- 可选环境变量 `HIPPOCAMPUS_SCENARIO_DETECT=off` 关闭识别（v2.33.1 考虑）

## 12. 风险与缓解

| 风险 | 影响 | 缓解 |
|------|------|------|
| 关键词规则误识别 | 场景错配 | 置信度阈值 + LLM 兜底 |
| LLM 调用增加延迟 | archive 变慢 | 仅首次调用，后续命中元数据跳过 |
| session_meta 文件冲突 | 多进程并发写 | 单写多读模型，archive 串行化 |
| 识别结果不稳定 | 同一 session 不同结果 | 首次识别后写入元数据，后续不再变 |
| Storage trait 扩展破坏第三方实现 | 编译错误 | 提供默认实现（返回 Ok(())/Ok(None)） |
