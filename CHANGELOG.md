# 变更历史

本项目遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。变更格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)。

## [Unreleased]

### 计划中
- v2：HTTP/Axum 服务 + WASM 组件 + 多语言绑定（Python 优先）

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
