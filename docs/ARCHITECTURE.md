# Hippocampus 架构文档

本文档详细描述 Hippocampus 的架构分层、模块职责与数据流。

## 1. 架构分层

```
┌─────────────────────────────────────────────────────────────────┐
│ Layer 3: Bindings                                               │
│   ① Python 原生绑定 (v2.2 ✅, hippocampus-python, PyO3 0.29)   │
│   ② Node/Go/Java (v2.4+, 待实现)                                │
├─────────────────────────────────────────────────────────────────┤
│ Layer 2: Interface                                              │
│   ① C ABI 动态库 (MVP ✅, hippocampus-ffi)                     │
│   ② Axum HTTP REST (v2.1 ✅, hippocampus-server)               │
│   ③ MCP Server (v2.3 ✅, hippocampus-mcp, rmcp 1.7+)           │
│   ④ WASM 组件 (v2.4, 待生态成熟)                               │
├─────────────────────────────────────────────────────────────────┤
│ Layer 1: Core (hippocampus-core, 纯 Rust)                       │
│   ┌──────────┬──────────┬──────────┬──────────┬──────────┐    │
│   │ archive  │ retrieve │ compact  │ score    │ storage  │    │
│   │ 归档     │ 检索     │ 周期合并 │ 评分     │ 存储后端 │    │
│   └──────────┴──────────┴──────────┴──────────┴──────────┘    │
│                       model (数据模型)                         │
└─────────────────────────────────────────────────────────────────┘
```

### 分层原则

- **Layer 1 纯逻辑**：不依赖 IO（文件系统/网络/时钟），所有副作用通过 trait 注入
- **Layer 2 接口层**：将 Core 的异步 Rust API 转换为各语言可调用的形式（C ABI / HTTP / MCP / WASM）
- **Layer 3 绑定层**：提供各语言的原生 SDK（自动释放/类型安全/异常映射）

### 接口层对比

| 维度 | C ABI (FFI) | HTTP REST (server) | Python 原生 (python) | MCP Server (mcp) |
|------|-------------|--------------------|-----------------------|------------------|
| crate | hippocampus-ffi | hippocampus-server | hippocampus-python | hippocampus-mcp |
| 调用方式 | C 函数 + JSON 字符串 | HTTP 端点 + JSON body | Python 方法 + dict | MCP tool + JSON 参数 |
| 状态 | 有状态（handle） | 无状态（每请求独立） | 有状态（实例） | 无状态（每 tool 调用独立） |
| 并发 | 单线程，调用方加锁 | 天然并发（tokio） | GIL 约束，单实例串行 | 单线程 stdio（rmcp） |
| Runtime | current_thread | rt-multi-thread | current_thread | current_thread |
| 错误处理 | HippocampusResult | {error:{code,message}} | PyValueError | McpError（invalid_params/internal_error） |
| 适合场景 | C/C++/嵌入式 | 远程访问 / 多语言 | Python 应用 / 数据科学 | AI 编程客户端（Claude Code/Cursor/Trae） |

## 2. 模块职责

### Layer 1: hippocampus-core

| 模块 | 职责 | 关键类型 |
|------|------|----------|
| `model` | 数据模型定义 | `MemoryFile` / `IndexHook` / `IndexDocument` / `MessageTurn` / `Tag` / `ArchivePeriod` |
| `archive` | 归档触发与执行 | `Archiver`（持有 Storage，全封装 archive 流程） |
| `retrieve` | 检索机制 | `Retriever`（摘要钩子注入 + tool 主动检索） |
| `compact` | 周期任务 | `Compactor`（weekly_merge / monthly_evict） |
| `score` | 评分 | `Scorer` trait + `DefaultScorer`（3 维启发式） |
| `storage` | 存储后端 | `Storage` trait + `LocalStorage`（RwLock + 原子写入） |
| `migrator` | Schema 迁移 | `Migrator` trait（v2 实现） |

#### 索引管理职责分配（无独立 IndexManager）

| 职责 | 承担方 |
|------|--------|
| 数据模型 | `model::IndexDocument` / `model::IndexHook` |
| 持久化 | `Storage::append_hook` / `Storage::read_index` / `Storage::write_index` |
| 摘要渲染 | `Retriever::render_to_system_prompt` |
| 钩子检索 | `Retriever::retrieve_memory` |
| 周期合并 | `Compactor::weekly_merge` / `monthly_evict`（钩子迁移） |

### Layer 2: hippocampus-ffi（C ABI）

| 组件 | 职责 |
|------|------|
| `HippocampusHandle` | 持有 storage + tokio Runtime + config + session_id + project_id |
| `HippocampusResult` | 统一返回包装（is_ok + data + error_message） |
| 5 个 C ABI 函数 | archive / retrieve / get_summaries / render_prompt / run_compaction |
| `hippocampus.h` | C 头文件，定义所有 ABI 接口 |

### Layer 2: hippocampus-server（HTTP REST）

| 组件 | 职责 |
|------|------|
| `Config` | 环境变量配置（HIPPOCAMPUS_HOST/PORT/ROOT） |
| `AppState` | 应用状态（存储根目录路径） |
| `AppError` | 统一错误响应（BadRequest 400 / NotFound 404 / Internal 500） |
| 5 个 handler | archive / retrieve / get_summaries / render_prompt / run_compaction |
| `create_router` | 路由配置（Axum 0.8 `{param}` 语法，路径前缀 `/api/v1/sessions/{sid}/...`） |
| `TraceLayer` | tower-http 请求日志中间件 |

### Layer 3: hippocampus-python（PyO3 绑定）

| 组件 | 职责 |
|------|------|
| `Hippocampus` pyclass | 持有 storage_root + tokio Runtime + session_id + project_id |
| `__enter__`/`__exit__` | 上下文管理器（自动释放资源） |
| 5 个方法 | archive / retrieve / summaries / prompt / compaction |
| `version()` / `operations()` | 模块级工具函数 |
| JSON 中间转换 | Python dict ↔ Rust struct（通过 json.dumps/loads + serde） |

### Layer 2: hippocampus-mcp（MCP Server）

| 组件 | 职责 |
|------|------|
| `HippocampusMcp` 结构体 | 持有 storage_root（无状态，每次 tool 调用创建独立 LocalStorage） |
| 5 个参数结构体 | `ArchiveParams` / `RetrieveParams` / `SummariesParams` / `PromptParams` / `CompactionParams`（derive `JsonSchema` 自动生成参数 schema） |
| `#[tool_router]` 宏 | rmcp 自动注册 5 个 `#[tool]` 方法为 MCP tools |
| 5 个 MCP tools | archive / retrieve / summaries / prompt / compaction |
| `main.rs` | stdio 传输入口（被 Claude Code / Cursor 等客户端拉起子进程） |
| 错误映射 | Core Error → `McpError::invalid_params` / `McpError::internal_error` |
| 传输方式 | stdio（默认），未来可在 hippocampus-server 挂 StreamableHttpService |

## 3. 数据流

### 3.1 归档流程

```
Agent 调用方                 hippocampus-ffi              hippocampus-core
     │                              │                            │
     │  hippocampus_archive(         │                            │
     │    handle, turns_json)        │                            │
     │ ────────────────────────────► │                            │
     │                              │  解析 turns_json           │
     │                              │  ──► Vec<MessageTurn>      │
     │                              │                            │
     │                              │  Archiver::new(...)         │
     │                              │  for turn in turns {        │
     │                              │    archiver.push_turn(...) │
     │                              │  }                          │
     │                              │  ────────────────────────► │
     │                              │                            │ 生成 MemoryFile
     │                              │                            │ Storage::write_memory
     │                              │                            │ 生成 IndexHook
     │                              │                            │ Storage::append_hook (daily 索引)
     │                              │  ◄──────────────────────── │
     │                              │  返回 SummaryView JSON      │
     │  ◄─────────────────────────── │                            │
     │  HippocampusResult*           │                            │
     │  (data = SummaryView JSON)   │                            │
```

> HTTP 和 Python 接口的数据流一致，仅入口形式不同：
> - HTTP：`POST /archive` body → handler → Core
> - Python：`hp.archive(turns)` → JSON 中间转换 → Core

### 3.2 检索流程

```
LLM 通过 tool 调用 retrieve_memory(hook_id)
     │
     │  hippocampus_retrieve(handle, hook_id)
     │ ────────────────────────────► hippocampus-ffi
     │                                    │
     │                                    │ Retriever::new(...)
     │                                    │ block_on(retriever.retrieve_memory(hook_id))
     │                                    │  → 遍历 daily/weekly/monthly 索引文档
     │                                    │  → 找到匹配的 IndexHook
     │                                    │  → 读取 hook.memory_file_path
     │                                    │  → 返回完整 MemoryFile
     │  ◄──────────────────────────────── HippocampusResult*
     │  (data = MemoryFile JSON)
```

### 3.3 周期任务流程

```
每周触发 weekly_merge                  每月触发 monthly_evict
        │                                      │
        ▼                                      ▼
  Compactor::weekly_merge              Compactor::monthly_evict
  1. 读取本周 daily 文件               1. 读取本月 weekly 文件
  2. 寒暄剥离（3 条规则）              2. DefaultScorer 评分（4 维加权）
  3. 去重 + 原样合并                  3. 选最高分 weekly 为主记忆
  4. 写入 weekly 记忆文件              4. 其余 weekly 挑高价值 Turn 保留
  5. 索引同步合并到 weekly             5. 写入 monthly 记忆文件
  6. 返回 CompactionResult            6. 索引同步合并到 monthly
                                       7. 返回 CompactionResult
```

## 4. 存储布局

```
<root_path>/
└── sessions/
    └── <session_id>/
        └── [projects/<project_id>/]    # 可选，project_id 存在时
            ├── daily/
            │   ├── index.json           # IndexDocument（钩子集合）
            │   ├── 2026-07-02_143052_123.json   # MemoryFile
            │   └── 2026-07-02_150000_456.json
            ├── weekly/
            │   ├── index.json
            │   └── 2026-W27.json
            └── monthly/
                ├── index.json
                └── 2026-07.json
```

### 文件命名规则

| 周期 | 格式 | 示例 |
|------|------|------|
| Daily | `YYYY-MM-DD_HHMMSS_mmm.json`（毫秒级时间戳，避免并发冲突） | `2026-07-02_143052_123.json` |
| Weekly | `YYYY-Www.json`（ISO 周编号） | `2026-W27.json` |
| Monthly | `YYYY-MM.json` | `2026-07.json` |

## 5. 并发模型

### Layer 1 (Core)

- **单写多读**：`LocalStorage` 内部 `RwLock`，读无锁，写串行化
- **原子写入**：temp 文件 + rename（防崩溃损坏）
- **读-改-写**：索引更新采用 read → modify → write back 模式

### Layer 2 - FFI (C ABI)

- **单线程模型**：`HippocampusHandle` 不保证线程安全
- **内部 tokio Runtime**：`current_thread`（轻量，适合 FFI 单线程模型）
- **调用方串行化**：多线程访问同一 handle 需调用方自行加锁
- **建议**：每线程独立创建 handle

### Layer 2 - HTTP (server)

- **无状态设计**：每次请求创建 LocalStorage，无内存会话池
- **tokio Runtime**：`rt-multi-thread`（支持并发请求）
- **天然水平扩展**：无状态 + 文件存储，可多实例部署

### Layer 3 - Python (python)

- **GIL 约束**：单实例串行调用（PyO3 同步 API）
- **内部 tokio Runtime**：`current_thread`（与 FFI 一致）
- **上下文管理器**：`with Hippocampus(...) as hp:` 自动释放资源
- **建议**：多会话用多实例（每会话一个 Hippocampus 对象）

### Layer 2 - MCP (mcp)

- **无状态设计**：每次 tool 调用创建独立 LocalStorage，无共享状态
- **stdio 传输**：rmcp 单线程 stdio 模型，被客户端（Claude Code/Cursor）作为子进程拉起
- **内部 tokio Runtime**：`current_thread`（与 FFI/Python 一致，轻量）
- **会话隔离**：通过 tool 参数 `session_id` / `project_id` 区分不同会话
- **未来扩展**：可在 hippocampus-server 上挂 `StreamableHttpService` 支持 HTTP 传输

## 6. 数据格式

### MemoryFile JSON 示例

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "schema_version": 1,
  "archived_at": "2026-07-02T14:30:52.123Z",
  "session_id": "session-001",
  "project_id": null,
  "turns": [
    {
      "id": "550e8400-e29b-41d4-a716-446655440001",
      "user_message": { "text": "用户消息", "attachments": [], "tool_calls": [], "thinking": null },
      "llm_message": { "text": "LLM 回复", "attachments": [], "tool_calls": [], "thinking": null },
      "tags": [{"kind": "Text"}, {"kind": "CodeBlock"}],
      "timestamp": "2026-07-02T14:30:00Z",
      "token_count": 110
    }
  ],
  "tags": [{"kind": "Text"}, {"kind": "CodeBlock"}],
  "total_tokens": 110,
  "truncated": false,
  "period": "Daily",
  "access_count": 0,
  "importance": 0
}
```

### SummaryView JSON 示例

```json
{
  "hook_id": "550e8400-e29b-41d4-a716-446655440002",
  "memory_file_id": "550e8400-e29b-41d4-a716-446655440000",
  "summary_title": "用户消息",
  "tags": ["文本消息", "代码块"],
  "archived_at": "2026-07-02T14:30:52.123Z",
  "period": "daily",
  "token_count": 110
}
```

### render_prompt 输出示例

```markdown
# 可用记忆索引

以下是可用的历史记忆摘要，可通过记忆检索工具获取详细内容：

## 近期记忆（daily）

- **用户消息**[文本消息, 代码块]（110 tokens, at 2026-07-02T14:30:52Z）
  - 记忆 ID: `550e8400-e29b-41d4-a716-446655440002`
```

## 7. 可扩展性

### 可插拔 trait

| Trait | 默认实现 | 替换场景 |
|-------|----------|----------|
| `Storage` | `LocalStorage`（文件树） | S3 / Redis / PostgreSQL |
| `Scorer` | `DefaultScorer`（启发式） | LLM 评分（v2） |
| `Migrator` | （v2） | Schema 升级 |

### 评分维度扩展

`DefaultScorer` 实现了 3 维启发式：

1. **时效性**（半衰期 7 天，时间衰减）
2. **访问频率**（10 次满分，封顶）
3. **importance**（用户显式标记，0-100）

v2 计划接入 LLM 实现「主题相关性」维度（需语义理解）。

## 8. 错误处理

### Layer 1 (Core)

`hippocampus_core::Error` 枚举：

```rust
pub enum Error {
    Storage(String),    // 存储错误（IO/路径/序列化）
    Serialize(String),  // 序列化错误
    Index(String),      // 索引错误（钩子未找到等）
    Score(String),      // 评分错误
    Migrate(String),    // 迁移错误
}
```

### Layer 2 - FFI (C ABI)

所有错误通过 `HippocampusResult` 包装：

- `hippocampus_is_ok(result)` → 检查是否成功
- `hippocampus_get_error(result)` → 获取错误消息（需 free）
- `hippocampus_get_data(result)` → 获取成功数据（需 free）

错误消息为 UTF-8 字符串，可直接展示给用户。

### Layer 2 - HTTP (server)

统一 JSON 错误响应：

```json
{ "error": { "code": "NOT_FOUND", "message": "未找到钩子 ID: xxx" } }
```

HTTP 状态码映射：

| Core Error | HTTP Status | code |
|------------|-------------|------|
| `Error::Index`（含"未找到"） | 404 | `NOT_FOUND` |
| `Error::Serialize` | 400 | `BAD_REQUEST` |
| 其他 | 500 | `INTERNAL_ERROR` |

### Layer 3 - Python (python)

所有 Core Error 统一映射为 `PyValueError`，错误消息含上下文：

```python
try:
    hp.retrieve("nonexistent-id")
except ValueError as e:
    print(e)  # 检索失败: 未找到钩子 ID: nonexistent-id
```

### Layer 2 - MCP (mcp)

所有错误通过 `McpError`（rmcp `ErrorData`）返回，客户端可在 tool 调用响应中读取 `isError` 字段：

| 错误来源 | McpError 类型 | code | 触发条件 |
|---------|--------------|------|---------|
| 参数解析失败 | `invalid_params` | -32602 | `turns_json` 不是合法 JSON / turns 为空 / period 不是 weekly/monthly |
| Core 错误 | `internal_error` | -32603 | 归档/检索/周期任务失败 |
| 序列化失败 | `internal_error` | -32603 | Core 返回的结果序列化为 JSON 失败 |

错误消息格式：`{功能描述}失败: {Core Error 详情}`，例如：`归档失败: 索引错误: 未找到钩子 ID: xxx`。
