# 变更历史

本项目遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。变更格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)。

## [Unreleased]

### 计划中
- v2.4：WASM 组件（待生态成熟）+ Node/Go/Java 绑定

## [0.3.0] - 2026-07-03

### v2.3 接口层扩展 + 差异化定位。新增 MCP server + 明确市场定位文档。

### 新增

#### v2.3 - MCP Server（Model Context Protocol 接口）
- 新增 `hippocampus-mcp` crate（rmcp 1.8 + tokio，stdio 传输）
- 5 个 MCP tools（供 Claude Code / Cursor / Trae / Codex CLI 调用）：
  - `archive`：归档对话轮次为记忆文件，返回摘要（含 hook_id）
  - `retrieve`：按 hook_id 检索完整记忆文件
  - `summaries`：获取所有周期摘要列表
  - `prompt`：渲染 system prompt 文本
  - `compaction`：触发周期任务（period: "weekly"/"monthly"）
- 每个 tool 内部创建 LocalStorage，无状态设计
- 错误映射：Core Error → `McpError::invalid_params` / `McpError::internal_error`
- stdio 传输入口（main.rs），通过环境变量 `HIPPOCAMPUS_ROOT` 配置存储根目录
- 11 个 MCP 集成测试（archive/retrieve/summaries/prompt/compaction 全链路 + 会话隔离 + 完整工作流 + 错误处理）
- CI 新增 `mcp-integration-test` job

#### 差异化定位文档（动作 3）
- 新增 `docs/POSITIONING.md`：竞品对比矩阵 + 蓝海象限图 + 四大护城河分析
- 覆盖 12 个主流竞品全景对标（agentmemory/Zep/Letta/Mem0 等）
- 三个直接竞品深度对比（agentmemory ~23k stars / Zep-Graphiti / Letta-MemGPT）
- 明确放弃方向：不做 RAG 向量库、不做角色记忆、不做知识图谱
- README.md 首屏新增定位章节，突出"强时序+极简部署"蓝海象限

### 变更
- workspace `rust-version` 从 1.83 升至 1.85（rmcp 1.7+ 要求 edition 2024）
- workspace 新增依赖：`rmcp = { version = "1.7", features = ["schemars", "transport-io"] }`
- workspace members 新增 `crates/hippocampus-mcp`

### 测试
- 总计 120 测试全部通过（51 单元 + 6 集成 + 17 FFI + 14 HTTP + 1 server 单元 + 11 MCP + 20 Python）
- clippy 0 警告

## [0.2.0] - 2026-07-03

### v2 接口层扩展。在 MVP 基础上新增 HTTP REST API 服务 + Python 原生绑定。

### 新增

#### v2.1 - HTTP/Axum REST API 服务（commit 7f333b0）
- 新增 `hippocampus-server` crate（Axum 0.8 + tower-http 0.7）
- 5 个 REST 端点（路径前缀 `/api/v1/sessions/{sid}/...`）：
  - `POST /archive`：归档一批轮次
  - `GET /memories/{hook_id}`：按钩子 ID 检索
  - `GET /summaries`：获取所有周期摘要
  - `GET /prompt`：渲染 system prompt 文本
  - `POST /compaction`：触发周期任务（period: "weekly"/"monthly"）
- 无状态设计：每次请求创建 LocalStorage，天然支持水平扩展
- 统一错误响应：`{error:{code,message}}`，code 为 `BAD_REQUEST`/`NOT_FOUND`/`INTERNAL_ERROR`
- 环境变量配置：`HIPPOCAMPUS_HOST`（默认 127.0.0.1）/ `HIPPOCAMPUS_PORT`（默认 8765）/ `HIPPOCAMPUS_ROOT`（默认 ./data）
- 14 个 HTTP 集成测试（reqwest 客户端 + 随机端口 TestServer）
- CI 新增 `http-integration-test` job

#### v2.2 - Python 原生绑定（commit a3ed611）
- 新增 `hippocampus-python` crate（PyO3 0.29 + maturin build backend，cdylib）
- `Hippocampus` pyclass：OOP 风格 + `__enter__`/`__exit__` 上下文管理器 + `__repr__`
- 5 个方法（与 FFI/HTTP 一一对应）：`archive` / `retrieve` / `summaries` / `prompt` / `compaction`
- 数据类型映射：dict 字典（JSON 中间转换，零样板代码，无需额外 PyClass）
- 错误映射：Core Error → `PyValueError`
- 模块级函数：`version()` / `operations()`
- 内部 tokio Runtime（`current_thread`，block_on 执行 Core 异步方法）
- 20 个 pytest 集成测试（模块级/生命周期/archive/summaries/retrieve/prompt/compaction/隔离性/完整工作流）
- CI 新增 `python-integration-test` job（maturin build + pip install + pytest）

### 变更
- workspace `rust-version` 从 1.75 升至 1.83（PyO3 0.29 要求）
- workspace 新增依赖：`pyo3 = { version = "0.29", features = ["extension-module"] }`
- `.gitignore` 新增 Python 相关忽略项（`.venv/` / `__pycache__/` / `*.pyc` 等）

### 测试
- 总计 109 测试全部通过（51 单元 + 6 集成 + 17 FFI + 14 HTTP + 1 server 单元 + 20 Python）
- clippy 0 警告

## [0.1.0] - 2026-07-02

### MVP 首个可用版本。核心库 + C ABI 动态库完整实现。

### 新增

#### P0 - 项目骨架
- Cargo workspace 双 crate 架构（`hippocampus-core` + `hippocampus-ffi`）
- GitHub Actions CI 矩阵（Windows/Linux/macOS × x86_64/aarch64）

#### P1 - 核心数据模型 + 存储层
- `MemoryFile` / `IndexHook` / `IndexDocument` / `MessageTurn` / `Tag` 等核心数据结构
- 17 类细粒度标签（`Tag` enum，含 `Other(String)` 兜底扩展）
- `Storage` trait + `LocalStorage` 默认实现（RwLock + 原子写入 temp+rename）
- 文件命名：daily=日期_时间戳（毫秒级）/ weekly=YYYY-Www / monthly=YYYY-MM
- 14 个单元测试通过

#### P2 - 核心逻辑（归档/索引/检索）
- `Archiver`：归档触发检测 + 全封装 `archive()`（写入 Storage + 追加 daily 索引）
- `Retriever`：3 个核心方法（`get_summaries` / `render_to_system_prompt` / `retrieve_memory`）
- 混合检索机制：摘要钩子注入 system prompt + tool 主动检索
- `Tag` Display 中文输出（17 类标签）
- 跨模块集成测试 `full_flow.rs`（6 个场景）
- 26 单元测试 + 6 集成测试通过

#### P3 - 周期任务
- `DefaultScorer`：3 维启发式评分（时效性半衰期 7 天 + 访问频率 10 次满分 + importance）
- `Compactor::weekly_merge()`：周级无损去重合并（寒暄剥离 3 条规则）
- `Compactor::monthly_evict()`：月级评分淘汰（高价值 Turn 保留）
- 移除 `IndexManager`（职责已被 Storage/Retriever/Compactor 完全覆盖）
- 51 单元测试 + 6 集成测试通过

#### P4 - C ABI 动态库
- 5 个核心 C ABI 操作：
  - `hippocampus_archive`：归档一批轮次，返回 SummaryView JSON
  - `hippocampus_retrieve`：按钩子 ID 检索完整记忆文件
  - `hippocampus_get_summaries`：获取所有周期摘要视图
  - `hippocampus_render_prompt`：渲染摘要为 system prompt 文本
  - `hippocampus_run_compaction`：触发周期任务（周合并/月淘汰）
- 内部 tokio Runtime（current_thread，block_on 异步 Core 方法）
- C 头文件 `hippocampus.h` 完整定义
- 17 个 FFI 集成测试（句柄生命周期 / null 参数 / 全链路 / 内存安全）
- 74 测试全部通过（51 单元 + 6 集成 + 17 FFI）

#### P5 - 文档与示例
- 完整 README（含快速开始 / C 调用示例 / Python ctypes 示例）
- ARCHITECTURE.md 架构文档（数据流 / 模块职责）
- examples/ 示例项目（C + Python ctypes）
- 跨语言集成测试（C 程序实际加载动态库验证）
- 性能基准测试（criterion）

### 关键设计决策
- **完整上下文归档**（非摘要）：避免信息损失
- **单写多读 + RwLock**：读无锁，写串行化
- **可插拔 trait**：Storage / Scorer / Migrator 均可替换
- **JSON 序列化**：MVP 可调试优先，v2 支持 MessagePack
- **单线程 FFI 模型**：handle 不保证线程安全，调用方串行化
