# v2.34 pre_compress_hook 工具设计文档

> **状态**：已确认（2026-07-07）
> **作者**：brainstorming 流程产出
> **前置**：v2.33 场景识别已上线，伪钩子方案已落地（AGENTS.md + Rules）
> **参考**：[v2.31 路线图](../../v2.31-roadmap-context-awareness.md) 方向 3

---

## 一、背景与问题

### 1.1 现有 archive 伪钩子的 3 个缺陷

**缺陷 1：LLM 无法感知"即将被压缩"**

```
LLM 对话中 → Trae 决定压缩 → LLM 看到 marker message → 已晚了
                          ↑
                   这里 LLM 完全不知道
```

LLM 只有看到 `This session continues a previous conversation that lost its context.` 时才知道**已经压缩完了**，此时原始轮次已被 Trae 丢弃，永远拿不回。

**缺陷 2：主动归档依赖 LLM 自觉**

现有伪钩子方案靠 AGENTS.md 规则约束 LLM：
- "对话超过 20 轮主动归档"
- "Token 反馈 ≥80% 准备归档"

但 LLM 可能忘归档、判断失误、被长任务打断——一旦没归档就被压缩，原始内容永久丢失。

**缺陷 3：archive 输入是结构化 turns，有信息丢失**

```rust
// 现有 archive 输入
archive(
    session_id,
    turns_json: [{"user_message":..., "llm_message":...}],  // 只保留轮次
)
```

LLM 手动构造 turns_json 时可能遗漏：
- System prompt 的变化
- 工具调用的元数据
- 中间思考过程
- 客户端注入的 context（如 Trae 的 Recent files、Pending todos）

### 1.2 pre_compress_hook 应对的 3 个场景

#### 场景 1：LLM 感知到压缩前兆时调用（伪钩子增强）

**触发时机**（写入 AGENTS.md 规则）：
- 用户说"上下文好长，压缩一下"
- LLM 自检对话长度（基于上次 archive 返回的 token 反馈）
- Trae 显示压缩进度条（如果 LLM 能看到）

#### 场景 2：一次性完整归档（兜底机制）

```
日常 archive:    可能归档了轮次 1-10, 12-15（轮次 11 忘了归档）
pre_compress:    压缩前一次性 dump 整个上下文,不漏任何信息
                 ↓
                 双轨处理:
                 ├─ raw_context.txt  (原样存储,完整保留)
                 └─ 解析 turns → Archiver (结构化,可检索可摘要)
```

#### 场景 3：未来 Trae feature request 的现成方案

给 Trae 提 feature request 时有现成接口规范：
```
"我们设计好了 pre_compress_hook 接口,
 Trae 只需在 onContextCompress 事件触发时调用即可,
 参数: session_id + full_context + estimated_tokens"
```

---

## 二、核心设计决策

| 决策点 | 选择 | 理由 |
|--------|------|------|
| 工具定位 | 独立 MCP 工具,与 archive 平级 | 接口清晰,各司其职,内部复用 Archiver |
| full_context 形态 | 完整字符串 | 客户端简单,保留完整信息 |
| 字符串处理 | 双轨:raw_context + 解析 turns | 完整信息不丢 + 可检索摘要 |
| 调用方定位 | 伪钩子场景增强 | 当前无客户端主动调用,LLM 通过规则引导 |
| 功能完整度 | 方案 B(完整设计) | 与 archive 对等,preset/snapshot/场景识别全支持 |

---

## 三、架构定位与工具分工

### 3.1 工具矩阵

| 工具 | 定位 | 输入 | 触发时机 |
|------|------|------|---------|
| `archive` | 日常结构化归档 | turns_json（结构化轮次） | LLM 主动判断 / AGENTS.md 规则触发 |
| `pre_compress_hook` | 压缩前一次性完整归档 | full_context（字符串）+ 可选 preset/snapshot | LLM 感知压缩前兆 / 未来客户端事件 |
| `update_project_memory` | 项目级记忆反向写入 | section + content | 完成开发阶段 / 关键决策 |

### 3.2 关键分工原则

- `archive` 和 `pre_compress_hook` **互不替代**，互补存在
- 日常对话只用 `archive`；压缩前调用 `pre_compress_hook` 做兜底
- `pre_compress_hook` 内部**复用 Archiver**（保证可检索可摘要），但额外存储 raw_context
- 两者共享 `session_id` 空间，归档结果都进 `sessions/{sid}/memories/`

### 3.3 双端落地

- MCP 工具（`crates/hippocampus-mcp/src/lib.rs`）— Agent 客户端调用
- HTTP 端点（`crates/hippocampus-server/src/handlers.rs`）— 外部程序调用

---

## 四、接口契约

### 4.1 MCP 工具签名

```rust
#[tool(description = r#"
压缩前一次性完整归档。当 LLM 感知到即将被压缩(用户告知 / 客户端显示压缩进度 / 预判上下文超限)时调用。
与 archive 的区别:接收完整上下文字符串而非结构化 turns,双轨存储(raw_context + 解析 turns)。
内部复用 Archiver 生成可检索的 IndexHook,并原样保留完整上下文。
"#)]
async fn pre_compress_hook(
    &self,
    #[schemars(description = "会话 ID,约定 {客户端前缀}-{项目名}-{日期}")] session_id: String,
    #[schemars(description = "完整上下文字符串。客户端 dump 整个对话或 LLM 拼接关键内容")] full_context: String,
    #[schemars(description = "可选:客户端估算的原始 token 数,用于反馈循环")] estimated_tokens: Option<usize>,
    #[schemars(description = "可选:预设配置,与 archive 的 PresetParams 结构一致")] preset: Option<PresetParams>,
    #[schemars(description = "可选:任务状态快照,与 archive 的 task_state_snapshot 一致")] task_state_snapshot: Option<TaskStateSnapshot>,
) -> Result<PreCompressResult, Error>
```

### 4.2 HTTP 端点

```
POST /api/v1/sessions/{sid}/pre-compress
Content-Type: application/json

{
  "full_context": "<完整上下文字符串>",
  "estimated_tokens": 180000,        // 可选
  "preset": { ... },                 // 可选,同 ArchiveRequest.preset
  "task_state_snapshot": { ... }     // 可选
}

Response: 200 OK
{
  "hook_id": "5b30a117-...",
  "raw_context_path": "sessions/trae-myapp-20260707/raw_contexts/5b30a117.txt",
  "parse_success": true,
  "parsed_turns_count": 15,
  "archived_tokens": 45000,
  "estimated_total_tokens": 180000,
  "threshold": 120000,
  "threshold_ratio_percent": 150,
  "suggestion": "压缩前归档完成,共 15 轮,原始 180K tokens。可安全压缩。",
  "archived_at": "2026-07-07T12:34:56Z"
}
```

### 4.3 返回结构 PreCompressResult

```rust
struct PreCompressResult {
    // 共用字段(与 archive 一致)
    hook_id: String,
    archived_tokens: usize,
    estimated_total_tokens: usize,
    threshold: usize,
    threshold_ratio_percent: u64,
    suggestion: String,
    archived_at: String,

    // pre_compress 特有
    raw_context_path: String,        // raw_context 文件路径
    parse_success: bool,             // 是否成功解析 turns
    parsed_turns_count: usize,       // 解析出的轮次数(0 表示未解析)
    archive_reason: String,          // 固定 "pre_compress"
}
```

---

## 五、数据流与双轨处理

```
LLM/客户端 调用 pre_compress_hook
    │
    ▼
┌─────────────────────────────────────────┐
│ 1. 原样存储 raw_context                  │
│    路径: sessions/{sid}/raw_contexts/   │
│         {hook_id}.txt                    │
│    内容: full_context 原文               │
└─────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────┐
│ 2. 尝试解析 turns                         │
│    策略(按优先级):                        │
│    a. JSON 数组识别(若 full_context 是   │
│       [{user_message, llm_message}] 格式)│
│    b. 分隔符识别(按 "---" 或 "User:"/    │
│       "Assistant:" 分隔)                  │
│    c. 兜底:解析失败,parse_success=false  │
└─────────────────────────────────────────┘
    │
    ├─ 解析成功 ─→┐
    │             ▼
    │   ┌─────────────────────────────────┐
    │   │ 3a. 复用 Archiver 归档           │
    │   │   - 场景识别(若首次)             │
    │   │   - 应用 preset(若有)            │
    │   │   - 写 task_state_snapshot(若有) │
    │   │   - 生成 IndexHook(archive_reason│
    │   │     = "pre_compress")            │
    │   │   - 返回 token 反馈              │
    │   └─────────────────────────────────┘
    │
    └─ 解析失败 ─→┐
                  ▼
        ┌─────────────────────────────────┐
        │ 3b. 仅存 raw_context,不调 Archiver│
        │   - 生成最小 IndexHook:           │
        │     * turns: Vec::new() (空)      │
        │     * summary: 占位摘要(含 raw_   │
        │       context_path + archived_at) │
        │     * archive_reason="pre_compress"│
        │     * raw_context_path=Some(...)  │
        │   - parse_success = false         │
        │   - archived_tokens = 估算值       │
        │   - 返回 token 反馈               │
        └─────────────────────────────────┘
                  │
                  ▼
        ┌─────────────────────────────────┐
        │ 4. 返回 PreCompressResult         │
        └─────────────────────────────────┘
```

### 5.1 关键设计点

1. **raw_context 永远先存**：即使后续解析失败,完整上下文已保存在磁盘
2. **解析策略渐进**：JSON → 分隔符 → 失败,不暴力解析
3. **解析失败不阻塞**：仍返回 hook_id + token 反馈,LLM 可继续操作
4. **场景识别只触发一次**：若 session_meta 已存在,跳过(与 archive 一致)
5. **archive_reason 标记**：区分日常 archive 和压缩前 pre_compress,便于后续分析

---

## 六、数据模型变更

### 6.1 IndexHook 扩展

**文件**：`crates/hippocampus-models/src/lib.rs`

```rust
pub struct IndexHook {
    // 现有字段保持不变
    pub id: String,
    pub session_id: String,
    pub summary: MemorySummary,
    pub turns: Vec<Turn>,
    pub created_at: DateTime<Utc>,
    // ...

    // 新增字段
    /// 归档来源:archive(日常) / pre_compress(压缩前) / manual(手动)
    pub archive_reason: Option<String>,

    /// raw_context 文件相对路径(仅 pre_compress_hook 生成)
    pub raw_context_path: Option<String>,
}
```

**向后兼容策略**：
- `Option<T>` 字段，默认 `None`
- 旧 IndexHook 反序列化时这两个字段为 `None`，等价于 `archive_reason: "archive"`
- 读取时若 `archive_reason` 为 `None`，按 `"archive"` 处理

### 6.2 Storage trait 扩展

**文件**：`crates/hippocampus-core/src/storage.rs`

```rust
pub trait Storage: Send + Sync {
    // 现有方法保持不变
    // ...

    /// 写入 raw_context 文件(仅 pre_compress_hook 调用)
    /// 返回相对路径
    async fn write_raw_context(
        &self,
        session_id: &str,
        hook_id: &str,
        content: &str,
    ) -> Result<String, StorageError>;

    /// 读取 raw_context 文件内容
    async fn read_raw_context(
        &self,
        session_id: &str,
        hook_id: &str,
    ) -> Result<String, StorageError>;

    /// 删除 raw_context 文件(随记忆删除级联)
    async fn delete_raw_context(
        &self,
        session_id: &str,
        hook_id: &str,
    ) -> Result<(), StorageError>;
}
```

### 6.3 三实现

| 实现 | 存储位置 | 备注 |
|------|---------|------|
| `LocalStorage` | `sessions/{sid}/raw_contexts/{hook_id}.txt` | 文件系统 |
| `SqliteStorage` | `raw_contexts` 表（`hook_id` + `content TEXT`） | 数据库 |
| `CachedStorage` | 透传给底层 | 不缓存（raw_context 通常较大） |

**默认实现**（trait 提供）：
```rust
async fn write_raw_context(&self, ...) -> Result<String, StorageError> {
    Err(StorageError::NotSupported("raw_context"))
}
```
- 不支持的方法返回 `NotSupported`，向后兼容旧实现

### 6.4 SqliteStorage 迁移

**文件**：`crates/hippocampus-core/src/sqlite.rs`

```sql
-- 新增表
CREATE TABLE IF NOT EXISTS raw_contexts (
    session_id TEXT NOT NULL,
    hook_id TEXT NOT NULL,
    content TEXT NOT NULL,
    stored_at TEXT NOT NULL,
    PRIMARY KEY (session_id, hook_id)
);

-- IndexHook 表新增字段(若已存在则跳过)
ALTER TABLE memories ADD COLUMN archive_reason TEXT;
ALTER TABLE memories ADD COLUMN raw_context_path TEXT;
```

**迁移原则**（遵循项目约束第 6 条）：
- 新增 migration 文件，不修改旧的
- 服务启动时自动执行
- `ALTER TABLE ... ADD COLUMN` 在 SQLite 中是安全操作（字段默认 NULL）

---

## 七、错误处理与降级

### 7.1 错误分级

| 错误类型 | HTTP 状态 | 处理方式 | 是否阻塞返回 |
|---------|----------|---------|------------|
| raw_context 写入失败 | 500 | 返回错误，不继续后续流程 | 是 |
| turns 解析失败 | 200 | `parse_success=false`，继续生成空 IndexHook | 否 |
| Archiver 归档失败 | 200 | `parse_success=false`，仅保留 raw_context | 否 |
| 场景识别失败 | 200 | 降级到 Agent 默认场景（与 archive 一致） | 否 |
| preset 构建失败 | 400 | 返回错误，提示 preset 参数问题 | 是 |
| session_id 格式错误 | 400 | 返回错误 | 是 |

### 7.2 核心原则

**raw_context 永远先存**，后续任何失败都不影响 raw_context 的保存。

### 7.3 降级路径

```
full_context 传入
    │
    ▼
1. 写 raw_context  ← 永远执行,失败才返回 500
    │
    ├─ 成功 ─→ 继续
    └─ 失败 ─→ 返回 500(唯一阻塞错误)
    │
    ▼
2. 尝试解析 turns
    │
    ├─ 成功 ─→ 3a. 复用 Archiver
    │           │
    │           ├─ 成功 ─→ 完整 IndexHook + 反馈
    │           └─ 失败 ─→ 3b. 空 IndexHook(标记 parse_success=false)
    │
    └─ 失败 ─→ 3b. 空 IndexHook(标记 parse_success=false)
    │
    ▼
3. 返回 PreCompressResult
```

### 7.4 与现有降级机制对齐

| 组件 | 降级模式 | pre_compress_hook 行为 |
|------|---------|----------------------|
| SummaryGenerator | heuristic（无 LLM） | 用启发式摘要，token_count 仍准确 |
| ScenarioDetector | keyword_only（无 LLM） | 关键词识别场景，失败用 Agent 默认 |
| SessionSearch | keyword_only（无 Embedder） | 不影响 pre_compress_hook |

---

## 八、测试策略

### 8.1 单元测试

**raw_context 存取**（`crates/hippocampus-core/src/storage.rs` 附近）：
- `test_write_raw_context_creates_file`
- `test_read_raw_context_returns_content`
- `test_delete_raw_context_removes_file`
- `test_write_raw_context_overwrites_existing`（同 hook_id 重复写入）

**IndexHook 新字段**：
- `test_index_hook_with_archive_reason`
- `test_index_hook_with_raw_context_path`
- `test_index_hook_deserialize_legacy_without_new_fields`（向后兼容）

**SqliteStorage 迁移**：
- `test_raw_contexts_table_creation`
- `test_alter_memories_add_archive_reason_column`
- `test_raw_contexts_crud`

### 8.2 解析器测试

**新模块**：`crates/hippocampus-core/src/context_parser.rs`

**JSON 格式识别**：
- `test_parse_json_array_of_turns`（标准 `[{user_message, llm_message}]`）
- `test_parse_json_with_extra_fields`（含 id/timestamp 等额外字段）
- `test_parse_json_invalid_returns_none`

**分隔符识别**：
- `test_parse_user_assistant_markers`（`User: ... \n Assistant: ...`）
- `test_parse_dash_separator`（`---` 分隔）
- `test_parse_mixed_format`（JSON + 分隔符混合）

**兜底**：
- `test_parse_unrecognized_format_returns_none`
- `test_parse_empty_string_returns_none`

### 8.3 集成测试

**MCP 端**（`crates/hippocampus-mcp/tests/pre_compress_integration.rs`，新增）：

**完整流程**：
- `test_pre_compress_hook_with_json_context`（JSON 输入完整归档）
- `test_pre_compress_hook_with_plain_text_context`（纯文本输入,解析失败但 raw_context 已存）
- `test_pre_compress_hook_with_preset`（preset 应用）
- `test_pre_compress_hook_with_task_state_snapshot`（snapshot 写入）
- `test_pre_compress_hook_first_call_triggers_scenario_detect`（场景识别触发）

**降级路径**：
- `test_pre_compress_hook_archiver_failure_falls_back_to_raw_only`
- `test_pre_compress_hook_scenario_detect_failure_does_not_block`

**HTTP 端**（`crates/hippocampus-server/tests/http_integration.rs`，修改）：
- `test_http_pre_compress_endpoint`
- `test_http_pre_compress_with_invalid_session_id_returns_400`
- `test_http_pre_compress_with_invalid_preset_returns_400`

### 8.4 测试覆盖目标

| 模块 | 新增测试数 | 覆盖率目标 |
|------|----------|----------|
| storage（raw_context） | ~6 | ≥90% |
| IndexHook 新字段 | ~3 | 100% |
| SqliteStorage 迁移 | ~3 | 100% |
| context_parser | ~8 | ≥85% |
| MCP 集成 | ~6 | 完整流程 |
| HTTP 集成 | ~3 | 端到端 |
| **合计** | **~29** | - |

---

## 九、实现文件清单

| 文件 | 变更类型 | 说明 |
|------|---------|------|
| `crates/hippocampus-models/src/lib.rs` | 修改 | IndexHook 新增 2 字段 |
| `crates/hippocampus-core/src/storage.rs` | 修改 | Storage trait 新增 3 方法 + 默认实现 |
| `crates/hippocampus-core/src/local.rs` | 修改 | LocalStorage 实现 3 方法 |
| `crates/hippocampus-core/src/sqlite.rs` | 修改 | SqliteStorage 实现 3 方法 + 迁移 |
| `crates/hippocampus-core/src/cache.rs` | 修改 | CachedStorage 透传 |
| `crates/hippocampus-core/src/context_parser.rs` | **新增** | 解析器（JSON / 分隔符识别） |
| `crates/hippocampus-mcp/src/lib.rs` | 修改 | 新增 pre_compress_hook 工具 |
| `crates/hippocampus-server/src/handlers.rs` | 修改 | 新增 HTTP 端点 |
| `crates/hippocampus-mcp/tests/pre_compress_integration.rs` | **新增** | MCP 集成测试 |
| `crates/hippocampus-server/tests/http_integration.rs` | 修改 | HTTP 集成测试 |

### 9.1 不涉及（YAGNI）

- ❌ Python 绑定（`crates/hippocampus-python`）— Python 端无压缩前场景需求
- ❌ compaction / batch_* 工具 — pre_compress_hook 只新增工具,不修改现有

---

## 十、AGENTS.md 规则更新

### 10.1 新增规则片段

在 AGENTS.md 的「记忆协议」章节新增：

```markdown
### pre_compress_hook 调用时机

当感知到即将被压缩时,优先调用 pre_compress_hook 而非 archive:

- 用户明确说"压缩上下文" / "上下文好长"
- 上次 archive 返回的 threshold_ratio_percent >= 90
- 长任务执行中预判上下文即将超限

调用方式:
hippocampus.pre_compress_hook(
    session_id="trae-myapp-20260707",
    full_context="<完整对话上下文>",
    estimated_tokens=180000,  # 可选
)

pre_compress_hook 与 archive 的区别:
- archive: 日常归档,输入结构化 turns
- pre_compress_hook: 压缩前一次性完整归档,输入完整上下文字符串,双轨存储
```

### 10.2 Rules 文件同步

- `.trae/rules/hippocampus-archive.md` — 新增「pre_compress_hook 调用时机」章节
- `.catpaw/rules/hippocampus-archive.md` — 同步
- `docs/onboarding/rules/*.md` — 同步

---

## 十一、成功标准

### 11.1 功能验证

1. **MCP 工具调用成功**：`pre_compress_hook` 在 CatPaw/Trae 中可被 LLM 调用
2. **双轨存储生效**：raw_context 文件 + IndexHook 同时生成
3. **解析器健壮**：JSON / 分隔符 / 纯文本三种输入均能处理
4. **降级正确**：解析失败时仍返回 hook_id + token 反馈
5. **HTTP 端点可用**：`POST /api/v1/sessions/{sid}/pre-compress` 端到端通

### 11.2 兼容性验证

1. **向后兼容**：旧 IndexHook 反序列化不报错,新字段为 None
2. **现有 archive 不受影响**：archive 行为完全不变
3. **Storage trait 实现兼容**：默认实现返回 NotSupported,旧实现不破坏

### 11.3 测试验证

1. **单元测试**：~29 个新增测试全部通过
2. **集成测试**：MCP + HTTP 端到端流程通
3. **生产环境验证**：部署后 curl 调用 HTTP 端点,确认返回结构正确

---

## 十二、风险与缓解

| 风险 | 等级 | 缓解策略 |
|------|------|---------|
| full_context 过大导致内存问题 | 中 | 限制 max_size(如 10MB),超过返回 413 |
| 解析器误识别（把纯文本当 JSON 解析失败） | 低 | 解析失败不阻塞,有 raw_context 兜底 |
| SqliteStorage 迁移失败 | 低 | ALTER TABLE ADD COLUMN 是安全操作,失败时服务无法启动(符合预期) |
| LLM 不主动调用 pre_compress_hook | 中 | AGENTS.md 规则引导 + 与 archive 形成互补 |
| raw_context 文件累积占用磁盘 | 低 | 随记忆删除级联清理,后续可在 compaction 中处理 |

---

## 十三、后续演进方向（不在本次范围）

1. **compaction 整合**：在 compaction 时清理旧 raw_context 文件
2. **检索增强**：让 semantic_search 能检索 raw_context 内容
3. **客户端原生集成**：给 Trae 提 feature request,让 onContextCompress 事件自动调用
4. **raw_context 压缩存储**：大文件用 gzip 压缩
5. **Python 绑定**：若 Python 端有需求,再补 PyHippocampus.pre_compress_hook

---

## 十四、决策记录

| 决策 | 选择 | 理由 | 日期 |
|------|------|------|------|
| 工具定位 | 独立 MCP 工具 | 接口清晰,与 archive 平级 | 2026-07-07 |
| full_context 形态 | 完整字符串 | 客户端简单,保留完整信息 | 2026-07-07 |
| 字符串处理 | 双轨(raw + 解析) | 完整信息不丢 + 可检索摘要 | 2026-07-07 |
| 调用方定位 | 伪钩子场景增强 | 当前无客户端主动调用,LLM 通过规则引导 | 2026-07-07 |
| 功能完整度 | 方案 B(完整设计) | 与 archive 对等,preset/snapshot/场景识别全支持 | 2026-07-07 |
