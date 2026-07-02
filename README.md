# Hippocampus

> Agent 记忆库依赖库 —— 跨语言可引用的持久化高效完整记忆系统

命名取自大脑海马体（Hippocampus），负责记忆巩固（短期→长期）的核心结构。本项目将「天/周/月」三级索引周期映射到工程实现，为 Agent 提供生物学节律般的记忆机制。

## 核心特性

- **完整上下文归档**（非摘要）：达到阈值时冻结完整对话上下文为记忆文件，避免信息损失
- **三级索引周期**：
  - 天级（Daily）：持续归档
  - 周级（Weekly）：无损去重合并
  - 月级（Monthly）：4 维评分淘汰（时效性 / 访问频率 / 主题相关性 / 用户显式标记）
- **混合检索机制**：摘要钩子注入 system prompt + 详细钩子 LLM 主动 tool 检索
- **17 类细粒度标签**：索引钩子支持文本/附件/图片/视频/工具调用/思考过程等多维度标注
- **跨语言引用**：Rust 核心 + C ABI 动态库，可被 Python/Node/Go/Java 等通过 FFI 调用
- **可插拔架构**：`Storage` / `Scorer` / `Migrator` 等 trait 均可替换实现

## 架构分层

```
Layer 3: Bindings       Python/Node/Go/Java FFI wrapper (v2)
Layer 2: Interface      ① C ABI 动态库 (MVP)  ② HTTP/gRPC (v2)  ③ WASM (v2)
Layer 1: Core (Rust)    纯逻辑 crate，无 IO 依赖
```

详细架构与数据流见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)。

## MVP 范围

| Crate | 说明 | 状态 |
|-------|------|------|
| `hippocampus-core` | 核心库（数据模型 / 归档 / 索引 / 检索 / 周期任务 / 评分） | ✅ 完成 |
| `hippocampus-ffi`  | C ABI 动态库 + C 头文件 | ✅ 完成 |

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

## C ABI 接口概览

| 函数 | 作用 | 返回数据 |
|------|------|----------|
| `hippocampus_new` | 创建句柄（绑定 session_id） | `HippocampusHandle*` |
| `hippocampus_free` | 释放句柄 | void |
| `hippocampus_archive` | 归档一批轮次为记忆文件 | SummaryView JSON |
| `hippocampus_retrieve` | 按钩子 ID 检索完整记忆文件 | MemoryFile JSON |
| `hippocampus_get_summaries` | 获取所有周期摘要视图 | SummaryView 数组 JSON |
| `hippocampus_render_prompt` | 渲染摘要为 system prompt 文本 | prompt 纯文本（非 JSON） |
| `hippocampus_run_compaction` | 触发周期任务（周合并/月淘汰） | CompactionResult JSON |

**线程安全**：`HippocampusHandle` 不保证线程安全，多线程访问同一 handle 需调用方自行加锁。建议每线程独立创建 handle。

完整接口定义见 [crates/hippocampus-ffi/include/hippocampus.h](crates/hippocampus-ffi/include/hippocampus.h)。

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

- Rust 1.75+（edition 2021）
- 序列化：JSON（MVP 可调试优先，v2 支持 MessagePack）
- 存储：可插拔 trait，默认本地文件树
- 异步运行时：tokio（FFI 内部 `current_thread` runtime）

## 测试

```bash
# 全部测试（单元 + 集成 + FFI）
cargo test --workspace

# Clippy 检查
cargo clippy --workspace --all-targets -- -D warnings

# 性能基准（需先添加 criterion，见 docs/BENCHMARKS.md）
cargo bench -p hippocampus-core
```

当前测试覆盖：51 单元测试 + 6 集成测试 + 17 FFI 集成测试 = **74 测试全部通过**。

## 项目状态

- ✅ **MVP（P0-P4）**：核心库 + C ABI 动态库
- ✅ **P5**：用户文档 + 示例 + 跨语言测试 + 性能基准
- 🚧 **v2 路线图**：HTTP/Axum 服务 + WASM 组件 + 多语言绑定（Python 优先）

变更历史见 [CHANGELOG.md](CHANGELOG.md)。

## License

MIT OR Apache-2.0
