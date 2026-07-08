# MemoryCenter 架构文档

本文档详细描述 MemoryCenter 的架构分层、模块职责与数据流。

## 1. 架构分层

```
┌─────────────────────────────────────────────────────────────────┐
│ Layer 3: Bindings                                               │
│   ① Python 原生绑定 (v2.2 ✅, memory-center-python, PyO3 0.29)   │
│   ② WASM 组件 (v2.35 ✅, memory-center-wasm)                     │
│   ③ Node.js (v2.14 ✅, memory-center-node, napi-rs 3.x)         │
│   ④ Go / Java (v2.4+, 计划中)                                   │
├─────────────────────────────────────────────────────────────────┤
│ Layer 2: Interface                                              │
│   ① C ABI 动态库 (MVP ✅, memory-center-ffi)                     │
│   ② Axum HTTP REST (v2.1 ✅, memory-center-server)               │
│   ③ MCP Server stdio (v2.3 ✅, memory-center-mcp, rmcp 1.8+)    │
│   ④ MCP Streamable HTTP (v2.36 ✅, memory-center-server /mcp)    │
├─────────────────────────────────────────────────────────────────┤
│ Layer 1: Core (纯 Rust)                                          │
│   ┌─────────────────────────────────────────────────────────┐  │
│   │ memory-center-core (Facade)                              │  │
│   │   重导出 core-logic + 整合原生 IO（SQLite/文件树/缓存）  │  │
│   ├─────────────────────────────────────────────────────────┤  │
│   │ memory-center-core-logic (纯逻辑)                        │  │
│   │   archive / retrieve / compact / score / storage        │  │
│   │   model / migrator / bm25 / semantic / conflict         │  │
│   │   generate / heuristic（无 IO 依赖，可编译为 WASM）      │  │
│   └─────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

### 分层原则

- **Layer 1 核心**：由 `core-logic`（纯逻辑，无 IO 依赖，可编译为 WASM）+ `core`（Facade，整合原生 IO 实现）协同组成
- **Layer 2 接口层**：将 Core 的异步 Rust API 转换为各语言可调用的形式（C ABI / HTTP / MCP stdio / MCP Streamable HTTP）
- **Layer 3 绑定层**：提供各语言的原生 SDK（自动释放/类型安全/异常映射，含 Python / WASM / Node.js）

### 接口层对比

| 维度 | C ABI (FFI) | HTTP REST (server) | Python 原生 (python) | MCP stdio (mcp) | MCP Streamable HTTP (server /mcp) | WASM (wasm) |
|------|-------------|--------------------|-----------------------|------------------|-----------------------------------|-------------|
| crate | memory-center-ffi | memory-center-server | memory-center-python | memory-center-mcp | memory-center-server | memory-center-wasm |
| 调用方式 | C 函数 + JSON 字符串 | HTTP 端点 + JSON body | Python 方法 + dict | MCP tool + JSON 参数 | MCP tool + HTTP/SSE | JS 函数 + JS 对象 |
| 状态 | 有状态（handle） | 无状态（每请求独立） | 有状态（实例） | 无状态（每 tool 调用独立） | 无状态 | 有状态（实例） |
| 并发 | 单线程，调用方加锁 | 天然并发（tokio） | GIL 约束，单实例串行 | 单线程 stdio（rmcp） | 天然并发（复用 Axum 多线程） | 单线程（WASM 限制） |
| Runtime | current_thread | rt-multi-thread | current_thread | current_thread | rt-multi-thread | 不需要 |
| 错误处理 | MemoryCenterResult | {error:{code,message}} | PyValueError | McpError（invalid_params/internal_error） | McpError | JS 异常 |
| 适合场景 | C/C++/嵌入式 | 远程访问 / 多语言 | Python 应用 / 数据科学 | AI 编程客户端本地接入（Claude Code/Cursor/Trae/Codex） | Web 端 Agent / 多客户端共享 | 浏览器 / Node.js 嵌入 |

## 2. 模块职责

### Layer 1: memory-center-core-logic（纯逻辑）+ memory-center-core（Facade）

Layer 1 由两个 crate 协同组成：`memory-center-core-logic`（纯逻辑，无 IO 依赖，可编译为 WASM）与 `memory-center-core`（Facade，重导出 core-logic + 整合原生 IO 实现）。

| 模块 | 职责 | 关键类型 |
|------|------|----------|
| `model` | 数据模型定义 | `MemoryFile` / `IndexHook` / `IndexDocument` / `MessageTurn` / `Tag` / `ArchivePeriod` |
| `archive` | 归档触发与执行 | `Archiver`（持有 Storage，全封装 archive 流程，含 `pre_compress_hook` 双轨归档） |
| `retrieve` | 检索机制 | `Retriever`（摘要钩子注入 + tool 主动检索） |
| `compact` | 周期任务 | `Compactor`（weekly_merge / monthly_evict） |
| `score` | 评分 | `Scorer` trait + `DefaultScorer`（4 维加权评分） |
| `storage` | 存储后端 | `Storage` trait + `LocalStorage` / `SqliteStorage` / `CachedStorage` |
| `migrator` | Schema 迁移 | `Migrator` trait（v2 实现） |
| `bm25` | 关键词检索 | BM25 算法实现（jieba-rs 中文分词，纯 Rust） |
| `semantic` | 语义检索 | `SearchEngine` + `Embedder` trait（含 BM25 兜底） |
| `conflict` | 冲突检测 | `ConflictDetector` trait（自我矛盾 / 直接矛盾 / 立场反转三维度） |
| `generate` | 摘要生成 | `SummaryGenerator` trait（启发式首条消息 / LLM 生成） |
| `heuristic` | 启发式降级实现 | 摘要 / 冲突检测 / 评分的降级回退实现 |

#### 索引管理职责分配（无独立 IndexManager）

| 职责 | 承担方 |
|------|--------|
| 数据模型 | `model::IndexDocument` / `model::IndexHook` |
| 持久化 | `Storage::append_hook` / `Storage::read_index` / `Storage::write_index` |
| 摘要渲染 | `Retriever::render_to_system_prompt` |
| 钩子检索 | `Retriever::retrieve_memory` |
| 周期合并 | `Compactor::weekly_merge` / `monthly_evict`（钩子迁移） |

### Layer 2: memory-center-ffi（C ABI）

| 组件 | 职责 |
|------|------|
| `MemoryCenterHandle` | 持有 storage + tokio Runtime + config + session_id + project_id |
| `MemoryCenterResult` | 统一返回包装（is_ok + data + error_message） |
| 5 个 C ABI 函数 | archive / retrieve / get_summaries / render_prompt / run_compaction |
| `memory_center.h` | C 头文件，定义所有 ABI 接口 |

### Layer 2: memory-center-server（HTTP REST + MCP Streamable HTTP）

| 组件 | 职责 |
|------|------|
| `Config` | 环境变量配置（MEMORY_CENTER_HOST / PORT / ROOT，以及 MCP 相关 MEMORY_CENTER_MCP_ENABLED 等） |
| `AppState` | 应用状态（存储根目录路径） |
| `AppError` | 统一错误响应（BadRequest 400 / NotFound 404 / Internal 500） |
| 5 个 REST handler | archive / retrieve / get_summaries / render_prompt / run_compaction |
| `/mcp` 端点 | MCP Streamable HTTP 传输（v2.36，复用同一组 21 个 tools，与 stdio 模式一致） |
| `create_router` | 路由配置（Axum 0.8 `{param}` 语法，路径前缀 `/api/v1/sessions/{sid}/...`） |
| `TraceLayer` | tower-http 请求日志中间件 |

### Layer 3: memory-center-python（PyO3 绑定）

| 组件 | 职责 |
|------|------|
| `MemoryCenter` pyclass | 持有 storage_root + tokio Runtime + session_id + project_id |
| `__enter__`/`__exit__` | 上下文管理器（自动释放资源） |
| 5 个方法 | archive / retrieve / summaries / prompt / compaction |
| `version()` / `operations()` | 模块级工具函数 |
| JSON 中间转换 | Python dict ↔ Rust struct（通过 json.dumps/loads + serde） |

### Layer 2: memory-center-mcp（MCP Server）

| 组件 | 职责 |
|------|------|
| `MemoryCenterMcp` 结构体 | 持有 storage_root（无状态，每次 tool 调用创建独立 LocalStorage） |
| 参数结构体 | 各 tool 对应 `*Params` 结构体（derive `JsonSchema` 自动生成参数 schema） |
| `#[tool_router]` 宏 | rmcp 自动注册 21 个 `#[tool]` 方法为 MCP tools |
| 21 个 MCP tools | 见下方 tools 分类表 |
| `main.rs` | stdio 传输入口（被 Claude Code / Cursor / Trae / Codex 等客户端拉起子进程） |
| 错误映射 | Core Error → `McpError::invalid_params` / `McpError::internal_error` |
| 传输方式 | stdio（默认，v2.3）+ Streamable HTTP（v2.36，通过 memory-center-server 的 `/mcp` 端点） |

#### MCP tools 分类（21 个）

| 类别 | Tools | 引入版本 |
|------|-------|----------|
| 归档/检索 | `archive` / `pre_compress_hook` / `retrieve` / `batch_retrieve` / `batch_delete` / `batch_update` / `find_hook_by_prefix` | v2.3 / v2.3x |
| 摘要/渲染 | `summaries` / `prompt` / `get_config` | v2.3 |
| 检索增强 | `semantic_search` / `detect_conflicts` / `get_conflicts` | v2.3x |
| 周期任务 | `compaction` | v2.3 |
| 预设管理 | `preset_build` / `preset_list_agents` / `preset_list_scenarios` / `preset_list_models` | v2.3 |
| 项目记忆 | `update_project_memory` / `get_project_memory` | v2.3x |
| 规则安装 | `install_rules`（支持本地直接写入 + 远程模板模式） | v2.3x / v2.37 |

> stdio 与 Streamable HTTP 共享同一组 21 个 tools，仅传输通道不同。
> Streamable HTTP 模式下 `install_rules` 走"远程模板模式"：返回模板内容让 LLM 用 Write 工具创建文件（v2.37）。

## 3. 数据流

### 3.1 归档流程

```
Agent 调用方                 memory-center-ffi              memory-center-core
     │                              │                            │
     │  memory_center_archive(         │                            │
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
     │  MemoryCenterResult*           │                            │
     │  (data = SummaryView JSON)   │                            │
```

> HTTP 和 Python 接口的数据流一致，仅入口形式不同：
> - HTTP：`POST /archive` body → handler → Core
> - Python：`hp.archive(turns)` → JSON 中间转换 → Core

### 3.2 检索流程

```
LLM 通过 tool 调用 retrieve_memory(hook_id)
     │
     │  memory_center_retrieve(handle, hook_id)
     │ ────────────────────────────► memory-center-ffi
     │                                    │
     │                                    │ Retriever::new(...)
     │                                    │ block_on(retriever.retrieve_memory(hook_id))
     │                                    │  → 遍历 daily/weekly/monthly 索引文档
     │                                    │  → 找到匹配的 IndexHook
     │                                    │  → 读取 hook.memory_file_path
     │                                    │  → 返回完整 MemoryFile
     │  ◄──────────────────────────────── MemoryCenterResult*
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

### 3.4 压缩前归档流程（pre_compress_hook）

当客户端（如 Trae / Cursor）即将压缩上下文时，LLM 主动调用 `pre_compress_hook` 一次性归档完整上下文，避免压缩丢失原始内容：

```
LLM 检测到压缩前兆
（客户端提示 / 上下文接近上限 / 用户手动触发）
     │
     │  pre_compress_hook(session_id, full_context, estimated_tokens, task_state_snapshot)
     │ ─────────────────────────────────────────────────────────────────────────────►
     │
     │  双轨处理：
     │  ① raw_context 原样保存（完整字符串备份）
     │  ② 解析为 turns 复用 Archiver 流程（结构化归档）
     │
     │  ◄────────────────────────────────────────────────────────────────────────────
     │  返回归档结果（hook_id + 估算 token 数 + 阈值占比）
```

**与 `archive` 的区别**：

| 维度 | `archive` | `pre_compress_hook` |
|------|-----------|---------------------|
| 触发时机 | 日常归档（达阈值） | 压缩前一次性归档 |
| 输入 | 结构化 turns 数组 | 完整上下文字符串 + 可选 task_state_snapshot |
| 处理方式 | 单轨（结构化 turns） | 双轨（raw_context 原样 + 解析 turns） |
| 核心价值 | 日常记忆生命周期管理 | 即使客户端压缩丢弃原始轮次，MemoryCenter 仍保留完整备份 |

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

- **单线程模型**：`MemoryCenterHandle` 不保证线程安全
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
- **上下文管理器**：`with MemoryCenter(...) as hp:` 自动释放资源
- **建议**：多会话用多实例（每会话一个 MemoryCenter 对象）

### Layer 2 - MCP (mcp)

- **无状态设计**：每次 tool 调用创建独立 LocalStorage，无共享状态
- **stdio 传输**（v2.3）：rmcp 单线程 stdio 模型，被客户端（Claude Code/Cursor/Trae/Codex）作为子进程拉起
- **Streamable HTTP 传输**（v2.36）：通过 memory-center-server 的 `/mcp` 端点支持多客户端共享，复用 Axum 的 `rt-multi-thread`
- **内部 tokio Runtime**：stdio 模式 `current_thread`（与 FFI/Python 一致，轻量）；HTTP 模式复用 server 的 `rt-multi-thread`
- **会话隔离**：通过 tool 参数 `session_id` / `project_id` 区分不同会话

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
| `Storage` | `LocalStorage`（文件树）/ `SqliteStorage`（SQLite WAL）/ `CachedStorage`（moka 缓存） | S3 / Redis / PostgreSQL / 自定义云存储 |
| `Scorer` | `DefaultScorer`（4 维加权启发式） | LLM 评分 / 自定义加权策略 |
| `Migrator` | （v2 默认实现） | Schema 升级 / 历史数据迁移 |
| `Embedder` | （需配置 API，未配置时降级为纯 BM25） | 本地 Embedding 模型 / 其他云服务 |
| `SummaryGenerator` | 启发式摘要（首条消息前 80 字符） | LLM 摘要生成 / 自定义模板 |
| `ConflictDetector` | 启发式纯算法（三维度检测） | LLM 冲突检测 / 自定义规则 |

### 评分维度扩展

`DefaultScorer` 实现了 4 维加权评分（已接入 LLM 主题相关性维度）：

| 维度 | 权重 | 计算方式 | 说明 |
|------|------|----------|------|
| **时效性（Recency）** | 半衰期 7 天 | 时间衰减函数 | 越新分数越高 |
| **访问频率（Access Frequency）** | 10 次满分 | `access_count` 封顶 | 越常被检索分数越高 |
| **主题相关性（Topic Relevance）** | LLM 评分 | LLM 判断与当前主题相关度 | 需 LLM 配置；未配置时降级为 0 |
| **用户显式标记（User Marking）** | 0-100 | `importance` 字段 | 用户显式标记重要性 |

> 未配置 LLM API 时，主题相关性维度降级为 0，等价于 3 维启发式评分。

## 8. 错误处理

### Layer 1 (Core)

`memory_center_core::Error` 枚举：

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

所有错误通过 `MemoryCenterResult` 包装：

- `memory_center_is_ok(result)` → 检查是否成功
- `memory_center_get_error(result)` → 获取错误消息（需 free）
- `memory_center_get_data(result)` → 获取成功数据（需 free）

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

## 9. 高级功能（v2.3x ~ v2.37）

### 9.1 冲突检测（detect_conflicts）

当用户陈述与记忆中已记录的事实可能冲突时，LLM 主动调用 `detect_conflicts` 检测三维度冲突：

| 维度 | 说明 | 示例 |
|------|------|------|
| 自我矛盾 | 用户前后陈述互相矛盾 | 先说"用 Python"，后说"我用 Rust" |
| 直接矛盾 | 用户陈述与记忆记录直接冲突 | 记忆记录"项目 A"，用户说"项目 B" |
| 立场反转 | 用户立场发生反转 | 之前支持方案 X，现在反对方案 X |

- 配置 LLM API 时使用 LLM 检测（精度高）
- 未配置时降级为启发式纯算法（精度略低但仍可工作）
- 检测到的冲突通过 `get_conflicts` tool 查询历史记录

### 9.2 project_memory 反向写入

MemoryCenter 维护一份 `project_memory.md` 副本（`projects/{project_id}/project_memory.md`），通过 `update_project_memory` 工具更新副本后，LLM 用 Write 工具将内容写入 IDE 客户端的 memory 文件夹，完成"反向写入"闭环——让 MemoryCenter 记忆主动流入 IDE 的 Memory Context 注入层。

固定章节覆盖策略（章节用 HTML 注释标记界定，不影响用户手动写入的内容）：

| section | 用途 | 更新时机 |
|---------|------|---------|
| `task_state` | 当前任务状态 + 下一步 | 每个开发阶段完成时 |
| `decisions` | 架构决策记录 | 新增/修改 crate、数据模型变更时 |
| `progress` | 进度跟踪 | 里程碑达成时 |
| `risks` | 风险点记录 | 发现潜在问题时 |
| `conventions` | 项目约定 | 确立新约定时 |

### 9.3 Agent 自识别（11 个预设）

内置 11 个 Agent 预设（ClaudeCode / Cursor / Trae / Codex / Cline / Continue / Aider / Copilot / Zed / Windsurf / Roo），启动时自动识别客户端并注入对应的 `usage_protocol` 到 MCP `server_info.instructions` 字段。未识别时降级为依赖 `AGENTS.md` 规则文件。

通过 `preset_list_agents` tool 可查询所有预设；通过 `preset_build` 可构建自定义 `CombinedProfile`。

### 9.4 场景自适应（7 个 Scenario）

内置 7 个 Scenario（coding / writing / research / chat / review / debug / refactor），每个场景有不同的归档阈值、检索策略、标签权重配置。根据场景自动调整：

- **coding**：高阈值（400K），保留代码块和工具调用标签
- **writing**：低阈值（100K），保留文本和引用标签
- **research**：中阈值（200K），保留 URL 和引用标签
- 其他场景各有定制化策略

通过 `preset_list_scenarios` tool 可查询所有场景。

### 9.5 install_rules 远程模式（v2.37）

`install_rules` tool 支持两种模式：

| 模式 | 适用传输方式 | 行为 |
|------|--------------|------|
| 本地直接写入 | stdio | 直接在客户端本地路径创建规则文件（如 `.trae/rules/memory-center-archive.md`，由 install_rules 工具自动生成） |
| 远程模板模式 | Streamable HTTP | 返回模板内容，LLM 用 Write 工具创建文件 |

远程模式解决了 Web 端 Agent 无法直接访问客户端本地文件系统的问题。
