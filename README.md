# Hippocampus

> Agent 记忆库依赖库 —— 跨语言可引用的持久化高效完整记忆系统

命名取自大脑海马体（Hippocampus），负责记忆巩固（短期→长期）的核心结构。本项目将「天/周/月」三级索引周期映射到工程实现，为 Agent 提供生物学节律般的记忆机制。

## 核心特性

- **完整上下文归档**（非摘要）：达到阈值时冻结完整对话上下文为记忆文件，避免信息损失
- **三级索引周期**：
  - 天级：持续归档
  - 周级：无损去重合并
  - 月级：4 维评分淘汰（时效性 / 访问频率 / 主题相关性 / 用户显式标记）
- **混合检索机制**：摘要钩子注入 system prompt + 详细钩子 LLM 主动 tool 检索
- **17 类细粒度标签**：索引钩子支持文本/附件/图片/视频/工具调用/思考过程等多维度标注
- **跨语言引用**：Rust 核心 + C ABI 动态库，可被 Python/Node/Go/Java 等通过 FFI 调用
- **可插拔架构**：Storage / Scorer / Migrator 等 trait 均可替换实现

## 架构分层

```
Layer 3: Bindings       Python/Node/Go/Java FFI wrapper (v2)
Layer 2: Interface      ① C ABI 动态库 (MVP)  ② HTTP/gRPC (v2)  ③ WASM (v2)
Layer 1: Core (Rust)    纯逻辑 crate，无 IO 依赖
```

## MVP 范围

- `hippocampus-core`：核心库（数据模型 / 归档 / 索引 / 检索 / 周期任务 / 评分）
- `hippocampus-ffi`：C ABI 动态库 + C 头文件

## 技术栈

- Rust 1.75+ (edition 2021)
- 序列化：JSON（MVP 可调试优先，v2 支持 MessagePack）
- 存储：可插拔 trait，默认本地文件树

## License

MIT OR Apache-2.0
