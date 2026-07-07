# v2.35 WASM 组件设计

> **状态**：设计阶段（2026-07-07）
> **目标**：将 Hippocampus 核心逻辑编译为 WASM，支持浏览器/Edge/多语言嵌入场景
> **前置**：v2.34 pre_compress_hook 已完成（commit c6ea964）
> **范围**：core 拆分 + WASM 骨架 + MemoryStorage + 注入式 trait 绑定（P0+P1）

---

## 1. 背景与问题陈述

### 1.1 现状

Hippocampus 当前的 Layer 2 接口层提供：
- C ABI 动态库（hippocampus-ffi，MVP）
- HTTP/Axum REST API（hippocampus-server，v2.1）
- Python 原生绑定（hippocampus-python，v2.2，PyO3）
- MCP Server（hippocampus-mcp，v2.3，rmcp + stdio）
- Node.js 绑定（hippocampus-node，v2.14，napi-rs）
- Go 绑定（hippocampus-go，v2.15，cgo + C ABI）
- Java 绑定（hippocampus-java，v2.15，JNA + C ABI）

**缺口**：无 WASM 支持，无法在浏览器/Edge/Serverless 场景运行。

### 1.2 核心障碍

`hippocampus-core` crate 虽标注"纯逻辑"，实际包含原生 IO 实现：
- `storage.rs`：LocalStorage（依赖 `tokio::fs`）
- `sqlite.rs` / `sqlite_vector.rs`：依赖 `rusqlite` + `r2d2`
- `cache.rs`：依赖 `moka`

这些依赖不兼容 `wasm32-unknown-unknown` target。

### 1.3 目标场景

| 场景 | Storage 后端 | 用途 |
|------|-------------|------|
| 浏览器 | IndexedDB | 网页内运行记忆库，demo/教学/轻量集成 |
| Edge/Serverless | Workers KV / D1 | 全球低延迟 API，分布式部署 |
| 多语言嵌入 | 调用方注入 | WASM 组件模型生成多语言绑定 |

三场景混合范围过大，本 spec 仅覆盖 **core 拆分 + WASM 骨架 + MemoryStorage + 注入式绑定**。IndexedDB / KV 留到 v2.36 / v2.37 独立子项目。

---

## 2. 设计决策

### 2.1 拆分策略：纵向拆分（方案 A）

新建 `hippocampus-core-logic` crate（纯逻辑 + Storage trait 定义 + 业务逻辑），现有 `hippocampus-core` 改为 facade 重导出。

**选择理由**：
- 核心逻辑全复用，WASM 端只补 Storage 实现
- facade 保证向后兼容，现有代码无回归
- 业务逻辑（archive/retrieve/compact 等）不重复

### 2.2 拆分边界：trait 在 logic

- `Storage` trait 定义 → `hippocampus-core-logic`
- `LocalStorage` / `SqliteStorage` / `CachedStorage` 实现 → 留在 `hippocampus-core`
- 业务逻辑（archive/retrieve/compact/hybrid/semantic）→ `hippocampus-core-logic`（依赖 trait，不依赖实现）

### 2.3 async 模型：保持 `#[async_trait]`（Send 版）

- 原生环境：多线程 + `tokio::spawn` / `spawn_blocking` 不受影响
- WASM 环境：单线程，Send 约束自动满足
- 无需双 trait 或 feature flag，复杂度最低

### 2.4 WASM Storage 实现选型

v2.35 范围（P0+P1）：
- **MemoryStorage**（P0）：纯内存，无持久化，所有场景的 fallback
- **注入式 JsStorage**（P1）：JS 调用方实现 Storage trait，通过回调注入

v2.36 / v2.37 留待：
- IndexedDB Storage（浏览器持久化）
- KV Storage（Edge/Serverless）

---

## 3. 架构概览

### 3.1 三层架构

```
┌─────────────────────────────────────────────────────────────┐
│ Layer 3: WASM 绑定层（hippocampus-wasm crate）              │
│  - MemoryStorage（纯内存实现）                              │
│  - JsStorage（注入式 trait 绑定，JS 回调）                  │
│  - wasm-bindgen 导出（HippocampusCore + 数据类型）          │
└─────────────────────────────────────────────────────────────┘
                           │
                           ▼ 依赖
┌─────────────────────────────────────────────────────────────┐
│ Layer 2: 核心逻辑层（hippocampus-core-logic crate）         │
│  - 纯逻辑：model / context_parser / score / serialization   │
│    / migrator / bm25 / conflict / heuristic / generate /    │
│    vector                                                   │
│  - Storage trait 定义（#[async_trait] Send 版）             │
│  - 业务逻辑：archive / retrieve / compact / hybrid /        │
│    semantic（依赖 Storage trait）                           │
│  - 无 tokio::fs / rusqlite / moka 依赖                      │
│  - 可编译为 wasm32-unknown-unknown                           │
└─────────────────────────────────────────────────────────────┘
                           ▲ 重导出
┌─────────────────────────────────────────────────────────────┐
│ Layer 1: Facade 层（hippocampus-core crate，现有）          │
│  - pub use hippocampus_core_logic::*;                       │
│  - 重导出 LocalStorage / SqliteStorage / CachedStorage      │
│  - 向后兼容，现有代码无需修改                                │
└─────────────────────────────────────────────────────────────┘
```

### 3.2 关键设计原则

1. **core-logic 无 IO 依赖**：不依赖 tokio::fs / rusqlite / moka，可编译为 WASM
2. **core facade 向后兼容**：现有 `use hippocampus_core::*` 代码无需修改
3. **Storage trait 保持 Send**：原生多线程不受影响，WASM 单线程下 Send 自动满足
4. **业务逻辑全复用**：archive / retrieve / compact 等在 core-logic，WASM 端只需提供 Storage 实现

---

## 4. crate 拆分细则

### 4.1 新建 `crates/hippocampus-core-logic`

**职责**：纯逻辑 + Storage trait 定义 + 业务逻辑，无原生 IO 依赖

**模块归属**（从 `hippocampus-core/src/` 迁移）：

| 模块 | 归属 | 依赖说明 |
|------|------|---------|
| model.rs | ✅ core-logic | 纯数据结构（serde/chrono/uuid） |
| context_parser.rs | ✅ core-logic | 纯字符串解析 |
| score.rs | ✅ core-logic | Scorer trait + 启发式实现 |
| serialization.rs | ✅ core-logic | JSON/MessagePack |
| migrator.rs | ✅ core-logic | Schema 迁移逻辑 |
| bm25.rs | ✅ core-logic | 纯算法（jieba-rs 需验证 WASM 兼容） |
| conflict.rs | ✅ core-logic | ConflictDetector trait + NoopDetector |
| heuristic.rs | ✅ core-logic | 纯算法 |
| generate.rs | ✅ core-logic | LLM 生成器 trait |
| vector.rs | ✅ core-logic | 纯算法（cosine 相似度） |
| storage.rs | ⚠️ 拆分 | **trait 定义 + SessionMeta** → core-logic；**LocalStorage** → core |
| sqlite.rs | ❌ 留 core | rusqlite + r2d2 不兼容 WASM |
| sqlite_vector.rs | ❌ 留 core | 同上 |
| cache.rs | ❌ 留 core | moka 不兼容 WASM |
| archive.rs | ✅ core-logic | 依赖 Storage trait，不依赖具体实现 |
| retrieve.rs | ✅ core-logic | 同上 |
| compact.rs | ✅ core-logic | 同上 |
| hybrid.rs | ✅ core-logic | 同上 |
| semantic.rs | ✅ core-logic | 同上 |

**core-logic 依赖**：serde / serde_json / chrono / uuid / thiserror / tracing / async-trait / tokio（仅 `sync` feature，WASM 兼容）/ rmp-serde / jieba-rs / dashmap

**潜在风险与缓解**：

| 依赖 | WASM 兼容性 | 缓解措施 |
|------|------------|---------|
| jieba-rs | 未验证 | 先验证编译；不兼容则 feature flag 排除，WASM 端用简易分词 |
| dashmap | 未验证 | 先验证；不兼容则 feature flag 替换为 HashMap + RwLock |
| tokio::sync::RwLock | 可用 | 单线程非阻塞，WASM 兼容 |

### 4.2 现有 `hippocampus-core` 改为 facade

```rust
// crates/hippocampus-core/src/lib.rs
pub use hippocampus_core_logic::*;

// 重导出原生 IO 实现（模块路径不变）
pub mod storage;  // 保留 LocalStorage 实现（trait 定义已移到 core-logic）
pub mod sqlite;
pub mod sqlite_vector;
pub mod cache;

// 兼容性重导出：保持 use hippocampus_core::storage::{Storage, LocalStorage} 可用
pub use hippocampus_core_logic::storage::{Storage, SessionMeta};
pub use storage::LocalStorage;
```

**向后兼容**：
- `use hippocampus_core::storage::LocalStorage` — 不变
- `use hippocampus_core::storage::Storage` — 通过重导出可用
- `use hippocampus_core::Storage` — 通过 `pub use hippocampus_core_logic::*` 可用

### 4.3 拆分流程

1. 新建 `crates/hippocampus-core-logic`，复制纯逻辑模块
2. 拆分 `storage.rs`：trait 部分移到 core-logic，LocalStorage 留在 core
3. 修改 `hippocampus-core` 依赖 `hippocampus-core-logic`，重导出
4. 验证 `cargo test -p hippocampus-core` 全量通过（无回归）
5. 验证 `cargo build -p hippocampus-core-logic --target wasm32-unknown-unknown` 通过

---

## 5. WASM 绑定层

### 5.1 新建 `crates/hippocampus-wasm`

**职责**：将 core-logic 编译为 WASM，提供 JS 调用 API + MemoryStorage + 注入式 trait 绑定

**依赖**：
- `hippocampus-core-logic`（核心逻辑）
- `wasm-bindgen`（JS 绑定）
- `js-sys` / `web-sys`（JS 类型互操作）
- `serde-wasm-bindgen`（serde ↔ JS 对象转换）
- `wasm-bindgen-futures`（JS Promise ↔ Rust Future）

**target**：`wasm32-unknown-unknown`

**crate-type**：`["cdylib", "rlib"]`

### 5.2 MemoryStorage 实现

纯内存的 Storage 实现，用 `std::collections::HashMap` 存储所有数据：

```rust
pub struct MemoryStorage {
    memories: tokio::sync::RwLock<HashMap<String, MemoryFile>>,
    hooks: tokio::sync::RwLock<HashMap<(String, ArchivePeriod), IndexDocument>>,
    project_hooks: tokio::sync::RwLock<HashMap<(String, ArchivePeriod), IndexDocument>>,
    session_meta: tokio::sync::RwLock<HashMap<String, SessionMeta>>,
    raw_contexts: tokio::sync::RwLock<HashMap<(String, String), String>>,
    root: std::path::PathBuf,  // 仅用于路径生成（不实际写文件）
}
```

**特点**：
- 所有数据进程内存储，重启丢失
- 实现 Storage trait 的所有方法（约 20 个）
- 路径方法返回相对路径字符串（不实际写文件系统）
- 用于 demo / 测试 / 无状态计算 / 其他实现的 fallback

### 5.3 注入式 trait 绑定（JsStorage）

让 JS 调用方实现 Storage trait，Rust 端通过 `js_sys::Function` 回调：

```rust
#[wasm_bindgen]
pub struct JsStorage {
    write_memory_fn: js_sys::Function,
    read_memory_fn: js_sys::Function,
    delete_memory_fn: js_sys::Function,
    // ... 所有 Storage trait 方法的 JS 回调
}

#[async_trait]
impl Storage for JsStorage {
    async fn write_memory(&self, file: &MemoryFile) -> Result<String> {
        let js_obj = serde_wasm_bindgen::to_value(file)?;
        let promise = self.write_memory_fn.call1(&JsValue::NULL, &js_obj)?;
        let result = wasm_bindgen_futures::JsFuture::from(promise.into()).await?;
        // 解析 result 为 String
    }
    // ... 其他方法
}
```

**JS 端使用**：
```javascript
const storage = new Hippocampus.JsStorage({
    writeMemory: async (memory) => { /* IndexedDB / fetch / KV */ },
    readMemory: async (id) => { /* ... */ },
    // ...
});
const hippocampus = new Hippocampus.HippocampusCore(storage);
const hookId = await hippocampus.archive("session-1", turns);
```

**优点**：调用方完全控制存储后端（IndexedDB / KV / fetch 到远程服务）
**缺点**：JS 端需实现所有 Storage 方法（约 20 个），门槛较高

### 5.4 导出的 JS API

`#[wasm_bindgen]` 导出以下类：

| 类 | 用途 |
|----|------|
| `HippocampusCore` | 主入口，接收 Storage 实例，提供 archive / retrieve / search 等方法 |
| `MemoryStorage` | 默认内存存储，构造无需参数 |
| `JsStorage` | 注入式存储，构造接收 JS 回调对象 |
| `Archiver` | 直接使用归档器（高级用法） |
| `MessageTurn` / `Tag` 等数据类型 | 序列化为 JS 对象 |

**API 示例**：
```javascript
import init, { HippocampusCore, MemoryStorage } from 'hippocampus-wasm';

await init();  // 加载 WASM
const storage = new MemoryStorage();
const core = new HippocampusCore(storage);

const hookId = await core.archive("session-1", [
    { user_message: { text: "你好" }, llm_message: { text: "你好！" } }
]);

const memories = await core.listMemories("session-1", "daily");
```

### 5.5 构建与发布

- `wasm-pack build --target web` → 生成 `pkg/` 目录
- `pkg/` 包含：`hippocampus_wasm.js` + `hippocampus_wasm_bg.wasm` + TypeScript 类型定义
- 可发布到 npm（`hippocampus-wasm` 包名）

---

## 6. 错误处理

### 6.1 WASM 端错误传播链

```
core-logic Error 枚举 → wasm-bindgen JsValue → JS try/catch
```

**转换策略**：
- core-logic 的 `Error` 枚举（Storage/Serialize/Index/Score/Migrate）实现 `From<Error> for JsValue`
- WASM 绑定方法返回 `Result<JsValue, JsValue>`，JS 端用 `try/catch` 捕获
- 错误对象结构：`{ code: "STORAGE_ERROR", message: "..." }`（便于 JS 端分类处理）

### 6.2 JsStorage 回调错误处理

- JS 回调返回 rejected Promise → Rust 端转为 `Error::Storage(js_error_message)`
- JS 回调抛异常 → `JsFuture` 返回 rejected → 同上处理
- 序列化失败（JS 返回非法对象）→ `Error::Serialize`

---

## 7. 测试策略

### 7.1 三层测试

| 层级 | 工具 | 范围 |
|------|------|------|
| core-logic 单元测试 | `cargo test` | 所有纯逻辑模块 + 业务逻辑（用 MockStorage） |
| core-logic WASM 编译验证 | `cargo build --target wasm32-unknown-unknown` | 确保零原生依赖 |
| WASM 绑定测试 | `wasm-pack test --node`（或 `--headless`） | MemoryStorage + JsStorage + HippocampusCore API |

### 7.2 core-logic 单元测试复用

- 从 `hippocampus-core` 迁移所有不依赖 LocalStorage/SqliteStorage 的测试
- 新增 `MockStorage`（用 `Vec<MemoryFile>` + `Vec<IndexHook>`）供业务逻辑测试用
- 验证 archive/retrieve/compact/hybrid/semantic 在 MockStorage 上的行为

### 7.3 WASM 绑定测试

- `MemoryStorage` 基础 CRUD（write/read/delete memory + index + raw_context）
- `HippocampusCore.archive()` 端到端（传入 turns → 返回 hook_id → retrieve 能找到）
- `JsStorage` 回调机制（JS 实现简单 MemoryStorage → Rust 通过 trait 调用 → 验证回调正确）
- 序列化互操作（Rust 结构体 ↔ JS 对象）

### 7.4 回归测试

- `hippocampus-core` facade 层全量测试通过（验证拆分无回归）
- `hippocampus-server` / `hippocampus-mcp` 等下游 crate 编译 + 测试通过

---

## 8. 已知风险与缓解

| 风险 | 影响 | 缓解措施 |
|------|------|---------|
| jieba-rs 不兼容 WASM | BM25 中文分词不可用 | 先验证编译；不兼容则 feature flag 排除，WASM 端用简易分词 |
| dashmap 不兼容 WASM | 业务逻辑编译失败 | 先验证；不兼容则 feature flag 替换为 HashMap + RwLock |
| wasm-bindgen-futures 与 async_trait 的 Send 约束 | JsStorage 编译失败 | 验证 `#[async_trait]`（Send 版）在 WASM 中可用；不行则降级为 `?Send` 版 |
| Storage trait 方法约 20 个 | JsStorage 回调门槛高 | 提供 `MemoryStorage` 作为默认选项 + 文档示例 |

---

## 9. 文件清单

### 9.1 新建文件

| 路径 | 职责 |
|------|------|
| `crates/hippocampus-core-logic/Cargo.toml` | core-logic crate 配置（无原生 IO 依赖） |
| `crates/hippocampus-core-logic/src/lib.rs` | 模块导出 |
| `crates/hippocampus-core-logic/src/model.rs` | 从 core 迁移 |
| `crates/hippocampus-core-logic/src/context_parser.rs` | 从 core 迁移 |
| `crates/hippocampus-core-logic/src/score.rs` | 从 core 迁移 |
| `crates/hippocampus-core-logic/src/serialization.rs` | 从 core 迁移 |
| `crates/hippocampus-core-logic/src/migrator.rs` | 从 core 迁移 |
| `crates/hippocampus-core-logic/src/bm25.rs` | 从 core 迁移（验证 jieba-rs WASM 兼容） |
| `crates/hippocampus-core-logic/src/conflict.rs` | 从 core 迁移 |
| `crates/hippocampus-core-logic/src/heuristic.rs` | 从 core 迁移 |
| `crates/hippocampus-core-logic/src/generate.rs` | 从 core 迁移 |
| `crates/hippocampus-core-logic/src/vector.rs` | 从 core 迁移 |
| `crates/hippocampus-core-logic/src/storage.rs` | **仅 Storage trait + SessionMeta 定义**（从 core 拆分） |
| `crates/hippocampus-core-logic/src/archive.rs` | 从 core 迁移 |
| `crates/hippocampus-core-logic/src/retrieve.rs` | 从 core 迁移 |
| `crates/hippocampus-core-logic/src/compact.rs` | 从 core 迁移 |
| `crates/hippocampus-core-logic/src/hybrid.rs` | 从 core 迁移 |
| `crates/hippocampus-core-logic/src/semantic.rs` | 从 core 迁移 |
| `crates/hippocampus-core-logic/tests/mock_storage.rs` | MockStorage 测试辅助 |
| `crates/hippocampus-wasm/Cargo.toml` | WASM crate 配置（wasm-bindgen + serde-wasm-bindgen） |
| `crates/hippocampus-wasm/src/lib.rs` | WASM 入口 + 导出类 |
| `crates/hippocampus-wasm/src/memory_storage.rs` | MemoryStorage 实现 |
| `crates/hippocampus-wasm/src/js_storage.rs` | JsStorage 注入式实现 |
| `crates/hippocampus-wasm/src/error.rs` | Error → JsValue 转换 |
| `crates/hippocampus-wasm/src/bindings.rs` | HippocampusCore + 数据类型绑定 |
| `crates/hippocampus-wasm/tests/memory_storage.rs` | wasm-pack 测试 |
| `crates/hippocampus-wasm/tests/js_storage.rs` | wasm-pack 测试 |
| `crates/hippocampus-wasm/tests/api.rs` | HippocampusCore API 测试 |

### 9.2 修改文件

| 路径 | 改动 |
|------|------|
| `Cargo.toml`（workspace） | members 新增 `crates/hippocampus-core-logic` + `crates/hippocampus-wasm`；新增 workspace.dependencies：wasm-bindgen / js-sys / web-sys / serde-wasm-bindgen / wasm-bindgen-futures |
| `crates/hippocampus-core/Cargo.toml` | 新增 `hippocampus-core-logic = { path = "../hippocampus-core-logic" }` 依赖 |
| `crates/hippocampus-core/src/lib.rs` | 改为 facade：`pub use hippocampus_core_logic::*;` + 重导出原生实现 |
| `crates/hippocampus-core/src/storage.rs` | 删除 trait 定义（已移到 core-logic），保留 LocalStorage 实现 |
| `CHANGELOG.md` | 新增 v2.35 条目 |

---

## 10. 验证标准

- `cargo test -p hippocampus-core-logic` 全量通过
- `cargo test -p hippocampus-core` 全量通过（facade 无回归）
- `cargo build -p hippocampus-core-logic --target wasm32-unknown-unknown` 成功
- `cargo build -p hippocampus-wasm --target wasm32-unknown-unknown` 成功
- `wasm-pack test -p hippocampus-wasm --node` 通过
- `cargo test --workspace` 全量通过（除 WASM crate）

---

## 11. 后续演进

| 版本 | 范围 |
|------|------|
| v2.36 | IndexedDB Storage（浏览器持久化）+ demo 页面 |
| v2.37 | KV Storage（Cloudflare Workers / Vercel Edge / Deno Deploy） |
| v2.38 | WASM 组件模型（多语言绑定生成，替代部分 C ABI + PyO3 + napi） |

---

## 12. 决策记录

| 决策 | 选择 | 理由 |
|------|------|------|
| WASM 目标场景 | 混合（三场景都支持） | 长期统一架构 |
| v2.35 范围 | P0+P1（core 拆分 + WASM 骨架 + MemoryStorage + 注入式） | 范围可控，IndexedDB/KV 留后续 |
| 拆分策略 | 纵向拆分（方案 A） | 核心逻辑全复用，facade 向后兼容 |
| 拆分边界 | trait 在 logic | 业务逻辑依赖 trait 不依赖实现，WASM 端只补实现 |
| async 模型 | 保持 `#[async_trait]`（Send 版） | 原生不受影响，WASM 单线程自动满足 |
| WASM Storage | MemoryStorage + JsStorage | 默认内存 + 调用方注入，覆盖 P0+P1 |
