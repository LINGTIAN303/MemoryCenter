# Hippocampus

> Agent 记忆库依赖库 —— 跨语言可引用的持久化高效完整记忆系统

命名取自大脑海马体（Hippocampus），负责记忆巩固（短期→长期）的核心结构。本项目将「天/周/月」三级索引周期映射到工程实现，为 Agent 提供生物学节律般的记忆机制。

> ⚠️ **破坏性变更通知（2026-07-04）**
>
> 型号库已更新至 2026 年 7 月最新官方版本。**7 个旧型号构造器被删除**，已使用旧型号的代码需迁移至新型号。详见 [CHANGELOG.md](CHANGELOG.md#型号库更新2026-07-04-核查官方文档)。
>
> 快速迁移：
> - `claude_opus_4_5()` → `claude_opus_4_8()`（推荐）
> - `claude_sonnet_4_5()` → `claude_sonnet_5()`
> - `gemini_3_pro()` → `gemini_3_1_pro()`
> - `deepseek_v3_2()` / `deepseek_r1()` → `deepseek_v4_pro()` 或 `deepseek_v4_flash()`
> - `qwen_3()` → `qwen_3_coder()`
> - `llama_4()` → `llama_4_scout()` 或 `llama_4_maverick()`
>
> **紧急**：DeepSeek V3/V3.2 将于 2026-07-24 停服，请尽快迁移至 V4。

## 定位：Agent 的时序记忆基础设施

**向量库做语义检索（找"像什么"），Hippocampus 做时序归档（找"之前发生过什么"）——两者互补不替代。**

Hippocampus 不存向量、不做语义检索、不做 Agent 编排，专注一件事：**完整保存对话上下文（非摘要），通过三级周期管理记忆生命周期**。这是市场上的空白生态位——所有时序能力强的方案（Zep/Letta）部署都重，所有部署极简的（agentmemory/LlamaIndex）时序治理都不深。Hippocampus 是唯一同时占据"强时序 + 极简部署"双象限的项目。

完整竞品对标见 [docs/POSITIONING.md](docs/POSITIONING.md)。

### 四个独家护城河

1. **三级索引周期 + 4 维加权评分淘汰**——天归档/周无损去重合并/月评分淘汰，竞品要么不淘汰、要么事实级失效
2. **完整对话非摘要归档**——所有竞品都走压缩/抽取/摘要路径，Hippocampus 无损保存可追溯
3. **Rust 单二进制 + C ABI 嵌入**——唯一可嵌入宿主进程的方案，零外部依赖
4. **17 类消息级标签**——粒度最细，支持按工具调用/思考过程/代码块等维度筛选

## 核心特性

- **完整上下文归档**（非摘要）：达到阈值时冻结完整对话上下文为记忆文件，避免信息损失
- **三级索引周期**：
  - 天级（Daily）：持续归档
  - 周级（Weekly）：无损去重合并
  - 月级（Monthly）：4 维评分淘汰（时效性 / 访问频率 / 主题相关性 / 用户显式标记）
- **混合检索机制**：摘要钩子注入 system prompt + 详细钩子 LLM 主动 tool 检索
- **17 类细粒度标签**：索引钩子支持文本/附件/图片/视频/工具调用/思考过程等多维度标注
- **跨语言引用**：Rust 核心 + C ABI 动态库 + HTTP REST API + Python 原生绑定（PyO3） + MCP Server（Model Context Protocol）
- **可插拔架构**：`Storage` / `Scorer` / `Migrator` 等 trait 均可替换实现

## 架构分层

```
Layer 3: Bindings       ① Python 原生绑定 (PyO3, v2.2 ✅)  ② Node/Go/Java (v2.4+)
Layer 2: Interface      ① C ABI 动态库 (MVP ✅)  ② Axum HTTP REST (v2.1 ✅)  ③ MCP Server (v2.3 ✅)  ④ WASM (v2.4)
Layer 1: Core (Rust)    纯逻辑 crate，无 IO 依赖
```

详细架构与数据流见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)。

## Crate 矩阵

| Crate | 说明 | 状态 |
|-------|------|------|
| `hippocampus-core` | 核心库（数据模型 / 归档 / 索引 / 检索 / 周期任务 / 评分） | ✅ MVP |
| `hippocampus-ffi`  | C ABI 动态库 + C 头文件 | ✅ MVP |
| `hippocampus-server` | Axum HTTP REST API 服务（无状态，水平扩展） | ✅ v2.1 |
| `hippocampus-python` | Python 原生绑定（PyO3 + maturin） | ✅ v2.2 |
| `hippocampus-mcp` | MCP Server（Model Context Protocol，stdio 传输） | ✅ v2.3 |

## 快速开始

### 1. 构建

```bash
# 克隆仓库
git clone https://github.com/lingtian303/Hippocampus.git
cd Hippocampus

# 构建动态库（hippocampus.dll / libhippocampus.so / libhippocampus.dylib）
cargo build --release -p hippocampus-ffi

# 构建产物位于：
#   Windows: target/release/hippocampus.dll
#   Linux:   target/release/libhippocampus.so
#   macOS:   target/release/libhippocampus.dylib
```

### 2. C 调用示例

将 `crates/hippocampus-ffi/include/hippocampus.h` 与动态库一起接入项目：

```c
#include "hippocampus.h"
#include <stdio.h>

int main(void) {
    /* 1. 创建句柄（绑定一个会话） */
    HippocampusHandle* h = hippocampus_new(
        "./mem_data",       /* 存储根目录 */
        "session-001",      /* 会话 ID */
        NULL                /* project_id，NULL 表示无项目隔离 */
    );
    if (!h) { return 1; }

    /* 2. 归档一批轮次（turns_json 为 MessageTurn 数组的 JSON） */
    const char* turns_json = /* ... */;
    HippocampusResult* r = hippocampus_archive(h, turns_json);
    if (hippocampus_is_ok(r)) {
        char* data = hippocampus_get_data(r);
        printf("归档成功，摘要：%s\n", data);
        hippocampus_free_string(data);
    } else {
        char* err = hippocampus_get_error(r);
        printf("归档失败：%s\n", err);
        hippocampus_free_string(err);
    }
    hippocampus_result_free(r);

    /* 3. 渲染 system prompt（注入到下一轮 LLM 调用） */
    HippocampusResult* pr = hippocampus_render_prompt(h);
    if (hippocampus_is_ok(pr)) {
        char* prompt = hippocampus_get_data(pr);
        /* 将 prompt 拼接到 LLM system prompt 末尾 */
        hippocampus_free_string(prompt);
    }
    hippocampus_result_free(pr);

    /* 4. 释放句柄 */
    hippocampus_free(h);
    return 0;
}
```

完整示例代码见 [examples/c/demo.c](examples/c/demo.c)。

### 3. Python 通过 ctypes 调用

```python
import ctypes, json

lib = ctypes.CDLL("./libhippocampus.so")  # Windows 用 hippocampus.dll

# 配置函数签名
lib.hippocampus_new.restype = ctypes.c_void_p
lib.hippocampus_new.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p]
lib.hippocampus_archive.restype = ctypes.c_void_p
lib.hippocampus_archive.argtypes = [ctypes.c_void_p, ctypes.c_char_p]

# 创建句柄并归档
handle = lib.hippocampus_new(b"./mem_data", b"session-001", None)
turns = [{"id": "...", "user_message": {...}, "llm_message": {...}, ...}]
result = lib.hippocampus_archive(handle, json.dumps(turns).encode())
```

完整 Python 示例见 [examples/python/demo.py](examples/python/demo.py)。

### 4. Python 原生绑定（推荐，v2.2）

使用 PyO3 原生绑定，无需 ctypes 手动配置函数签名，支持上下文管理器自动释放：

```bash
# 安装 maturin（PyO3 团队开发的构建工具）
pip install maturin

# 构建并安装到当前 Python 环境
cd crates/hippocampus-python
maturin develop --release
```

```python
from hippocampus_python import Hippocampus

# 上下文管理器自动释放资源
with Hippocampus("./mem_data", "session-001", project_id="proj-a") as hp:
    # 1. 归档（turns 为 dict 列表，结构同 MessageTurn）
    summary = hp.archive([
        {
            "user_message": {"text": "你好", "attachments": [], "tool_calls": [], "thinking": None},
            "llm_message": {"text": "你好！有什么可以帮你？", "attachments": [], "tool_calls": [], "thinking": None},
            "tags": [{"kind": "Text"}],
            "token_count": 20,
        }
    ])
    print(f"归档成功，hook_id={summary['hook_id']}")

    # 2. 获取所有周期摘要（注入 system prompt 用）
    summaries = hp.summaries()
    print(f"共 {len(summaries)} 条记忆")

    # 3. 渲染 system prompt 文本（直接拼接给 LLM）
    prompt = hp.prompt()
    if prompt:
        print(prompt)  # # 可用记忆索引 ...

    # 4. 按钩子 ID 检索完整记忆（LLM tool 调用入口）
    memory = hp.retrieve(summary["hook_id"])
    print(f"检索到 {len(memory['turns'])} 轮对话")

    # 5. 周期任务
    hp.compaction("weekly")   # 周级无损去重合并
    hp.compaction("monthly")  # 月级评分淘汰
```

详细 API 见 [crates/hippocampus-python/src/lib.rs](crates/hippocampus-python/src/lib.rs)。
Python 集成测试见 [crates/hippocampus-python/tests/test_hippocampus.py](crates/hippocampus-python/tests/test_hippocampus.py)（20 个 pytest 用例）。

### 5. HTTP REST API（v2.1）

启动 Axum 服务（适合远程访问 / 多语言客户端共用）：

```bash
# 启动服务（默认 127.0.0.1:8765）
HIPPOCAMPUS_HOST=0.0.0.0 HIPPOCAMPUS_PORT=8765 HIPPOCAMPUS_ROOT=./data cargo run -p hippocampus-server
```

```bash
# 归档
curl -X POST http://localhost:8765/api/v1/sessions/sess-001/archive \
  -H "Content-Type: application/json" \
  -d '{"turns": [...], "project_id": "proj-a"}'

# 获取摘要
curl http://localhost:8765/api/v1/sessions/sess-001/summaries

# 渲染 prompt
curl http://localhost:8765/api/v1/sessions/sess-001/prompt

# 检索记忆
curl http://localhost:8765/api/v1/sessions/sess-001/memories/<hook_id>

# 周期任务
curl -X POST http://localhost:8765/api/v1/sessions/sess-001/compaction \
  -H "Content-Type: application/json" -d '{"period": "weekly"}'
```

详细 HTTP API 见 [crates/hippocampus-server/src/handlers.rs](crates/hippocampus-server/src/handlers.rs)。

### 6. MCP Server（v2.3，推荐用于 Claude Code / Cursor / Trae / Codex CLI）

MCP（Model Context Protocol）是 Anthropic 推出的 Agent 工具调用协议，主流 AI 编程客户端全支持。Hippocampus MCP server 让 Agent 通过标准协议调用记忆库能力，无需自己实现归档/检索逻辑。

```bash
# 构建 MCP server 二进制
cargo build --release -p hippocampus-mcp
# 产物：target/release/hippocampus-mcp
```

在 Claude Code / Cursor / Trae 等客户端的 MCP 配置中添加：

```json
{
  "mcpServers": {
    "hippocampus": {
      "command": "/path/to/hippocampus-mcp",
      "env": {
        "HIPPOCAMPUS_ROOT": "/path/to/memory/data"
      }
    }
  }
}
```

启动后，Agent 会自动发现 5 个 tools：`archive` / `retrieve` / `summaries` / `prompt` / `compaction`，并按需调用以管理上下文记忆。

详细 MCP tools 实现见 [crates/hippocampus-mcp/src/lib.rs](crates/hippocampus-mcp/src/lib.rs)。
MCP 集成测试见 [crates/hippocampus-mcp/src/lib.rs](crates/hippocampus-mcp/src/lib.rs#L279)（11 个测试用例）。

## 接口概览

四种接口形态对应同一组核心操作（archive / retrieve / summaries / prompt / compaction）：

| 操作 | C ABI | HTTP REST | Python 原生 | MCP Server |
|------|-------|-----------|-------------|------------|
| 创建句柄 | `hippocampus_new(root, sid, pid)` | （URL path 含 sid） | `Hippocampus(root, sid, project_id=...)` | （每次 tool 调用传 sid） |
| 归档 | `hippocampus_archive(h, turns_json)` | `POST /archive` | `hp.archive(turns)` | `archive` tool（params: sid/turns_json/project_id） |
| 检索 | `hippocampus_retrieve(h, hook_id)` | `GET /memories/{hook_id}` | `hp.retrieve(hook_id)` | `retrieve` tool（params: sid/hook_id/project_id） |
| 摘要 | `hippocampus_get_summaries(h)` | `GET /summaries` | `hp.summaries()` | `summaries` tool（params: sid/project_id） |
| Prompt | `hippocampus_render_prompt(h)` | `GET /prompt` | `hp.prompt()` | `prompt` tool（params: sid/project_id） |
| 周期任务 | `hippocampus_run_compaction(h, 0/1)` | `POST /compaction` | `hp.compaction("weekly"/"monthly")` | `compaction` tool（params: sid/period/project_id） |
| 释放 | `hippocampus_free(h)` | （无状态） | `with` 上下文管理器 / `hp.close()` | （无状态） |

**线程安全**：FFI 的 `HippocampusHandle` 不保证线程安全（建议每线程独立 handle）。HTTP 服务无状态，天然支持并发。Python 绑定受 GIL 约束，单实例串行调用。MCP server 每次 tool 调用独立 Storage，无共享状态。

完整接口定义：
- C ABI: [crates/hippocampus-ffi/include/hippocampus.h](crates/hippocampus-ffi/include/hippocampus.h)
- HTTP: [crates/hippocampus-server/src/handlers.rs](crates/hippocampus-server/src/handlers.rs)
- Python: [crates/hippocampus-python/src/lib.rs](crates/hippocampus-python/src/lib.rs)
- MCP: [crates/hippocampus-mcp/src/lib.rs](crates/hippocampus-mcp/src/lib.rs)

## 核心概念

### 归档（Archive / Freeze）

达到 token 阈值时，将完整上下文（用户消息 + LLM 消息）冻结为记忆文件，**非摘要**。

- **软阈值**：达到 `token_threshold`（如 400K）后，若当前轮次未完成则等待
- **硬上限**：达到 1.5 倍阈值（如 600K）强制截断，标记 `truncated=true`

### 索引钩子（Index Hook）

指向记忆文件的指针，带 17 类细粒度标签。分层设计：

- **摘要钩子**：注入 system prompt，包含标题+标签+时间戳（轻量）
- **详细钩子**：通过 tool 调用按需检索（含完整信息）

### 三级周期

| 周期 | 操作 | 说明 |
|------|------|------|
| 天级（Daily） | 持续归档 | 会话窗口达阈值 → 冻结为记忆文件 → 生成索引钩子 → 从 LLM 上下文丢弃 |
| 周级（Weekly） | 无损去重合并 | 7 天内的记忆文件去重 + 原样合并为 1 个，索引同步合并 |
| 月级（Monthly） | 评分淘汰 | 4 个周记忆文件按 4 维加权评分，选最高分为主记忆，其余高价值片段保留 |

### 17 类标签

文本消息 / 文件附件 / 图片 / 视频 / 工具调用 / 思考过程 / 会话 ID / 项目 ID / URL / 引用 / 状态 / UI / 代码块 / 语音 / 计划 / 使用的 Agent 工具 / 其他（`Other(String)` 兜底扩展）

## 工作流（典型 Agent 接入）

```
┌─────────────────────────────────────────────────────────────┐
│ 1. Agent 会话开始                                            │
│    - 调用 hippocampus_new() 创建 handle（绑定 session_id）    │
│    - 调用 hippocampus_render_prompt() 获取历史记忆摘要        │
│    - 将摘要拼接到 system prompt 末尾                          │
├─────────────────────────────────────────────────────────────┤
│ 2. Agent 持续对话                                             │
│    - 每轮结束后调用 hippocampus_archive() 归档（携带 turns）   │
│    - 当 LLM 需要历史细节时，通过 tool 调用 retrieve_memory   │
├─────────────────────────────────────────────────────────────┤
│ 3. 周期维护（按需触发）                                       │
│    - 每周：hippocampus_run_compaction(WEEKLY) 去重合并        │
│    - 每月：hippocampus_run_compaction(MONTHLY) 评分淘汰      │
├─────────────────────────────────────────────────────────────┤
│ 4. 会话结束                                                  │
│    - 调用 hippocampus_free() 释放 handle                     │
└─────────────────────────────────────────────────────────────┘
```

## 技术栈

- Rust 1.85+（edition 2021，rmcp 1.7+ 要求 edition 2024 编译器）
- 序列化：JSON（MVP 可调试优先，v2 支持 MessagePack）
- 存储：可插拔 trait，默认本地文件树
- 异步运行时：tokio（FFI/Python/MCP 内部 `current_thread` runtime，HTTP 服务 `rt-multi-thread`）
- HTTP 框架：Axum 0.8 + tower-http 0.7
- Python 绑定：PyO3 0.29 + maturin（cdylib）
- MCP 协议：rmcp 1.7+（stdio 传输，未来可扩展 Streamable HTTP）

## 测试

```bash
# Rust 全部测试（单元 + 集成 + FFI + HTTP + MCP）
cargo test --workspace

# Clippy 检查
cargo clippy --workspace --all-targets -- -D warnings

# 性能基准（见 docs/BENCHMARKS.md）
cargo bench -p hippocampus-core

# Python 集成测试（需先 maturin develop 安装）
cd crates/hippocampus-python
pip install maturin pytest
maturin develop --release
pytest tests/test_hippocampus.py -v
```

当前测试覆盖：51 单元 + 6 集成 + 17 FFI + 14 HTTP + 1 server 单元 + 11 MCP + 20 Python = **120 测试全部通过**，clippy 0 警告。

## 项目状态

- ✅ **MVP（P0-P5）**：核心库 + C ABI 动态库 + 文档 + 示例 + 跨语言测试 + 性能基准
- ✅ **v2.1**：HTTP/Axum REST API 服务（无状态，水平扩展）
- ✅ **v2.2**：Python 原生绑定（PyO3 + maturin，OOP 风格 + 上下文管理器）
- ✅ **v2.3**：MCP Server（rmcp，stdio 传输，5 个 MCP tools）+ 差异化定位文档
- 🚧 **v2.4 路线图**：WASM 组件（待生态成熟）+ Node/Go/Java 绑定

变更历史见 [CHANGELOG.md](CHANGELOG.md)。

## License

MIT OR Apache-2.0
