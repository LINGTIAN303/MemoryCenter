# Hippocampus 定位与竞品对比

> Agent 记忆库赛道全景对标，明确 Hippocampus 的差异化定位与护城河。
> 调研时间：2026-07-02 | 数据来源：GitHub、arXiv、横评文章

## 一句话定位

**向量库做语义检索（找"像什么"），Hippocampus 做时序归档（找"之前发生过什么"）——两者互补不替代。**

Hippocampus 是 Agent 的时序记忆基础设施：完整保存对话上下文（非摘要），通过天/周/月三级周期管理记忆生命周期，提供混合检索（摘要钩子注入 + LLM tool 主动检索）。

## 蓝海象限：唯一双占位项目

```
纵轴：部署复杂度（高↑ = 越简单）
    ↑
高  │                          ★ Hippocampus（蓝海）
    │                       ★ agentmemory          ★ LlamaIndex Memory
    │
    │                  ★ Supermemory(SaaS)
    │              ★ A-MEM
    │
中  │         ★ Memary        ★ Mem0(Cloud)
    │                    ★ Cognee
    │
低  │    ★ Zep CE                  ★ Letta
    │ ★ Zep Cloud                 ★ Mem0(Self-host)
    │
    └──────────────────────────────────────────────→
    弱                    时序能力                   强
```

**所有时序能力强的（Zep/Letta）部署都重；所有部署极简的（agentmemory/LlamaIndex）时序治理都不深。Hippocampus 是唯一同时占据"强时序 + 极简部署"双象限的项目。**

## 竞品全景对标表

| 竞品 | 核心架构 | 归档/淘汰 | 检索机制 | 标签粒度 | 接口 | 部署 | stars | 评测分数 | 目标场景 |
|---|---|---|---|---|---|---|---|---|---|
| **agentmemory** | SQLite + iii-engine（压缩式） | 自动会话归档 + iii 压缩 | BM25 + 向量 + 图谱 RRF 融合 | Observation 级（单层） | MCP + REST + Python + Hooks | npm 单二进制 | ~23k | LongMemEval R@5 **95.2%** | AI 编程 Agent |
| **Zep / Graphiti** | 时序知识图谱（Neo4j/FalkorDB） | bi-temporal 事实失效（不删除） | 语义 + BM25 + 图遍历 | 实体/关系级 | Python + MCP + REST | Python + 图数据库 + Docker | - | LongMemEval **63.8%** | 企业级 Agent |
| **Letta (MemGPT)** | Memory Blocks + Archival + Sleep-time | Sleep-time Agent 后台重写 | archival_memory_search tool | Block 命名空间 | Python/TS + REST + MCP | Python + Postgres + Docker | 8.9k | LongMemEval R@5 **83.2%** | 通用 Agent 平台 |
| **Mem0** | ADD-only 提取 + 实体链接 | ADD-only 不删除 | 语义 + BM25 + 实体匹配 | 实体/偏好级 | Python/TS + REST + MCP | Python + Qdrant + Docker | ~57k | LongMemEval **94.8%** | 通用 LLM 应用 |
| **LangMem** | LangGraph BaseStore + Memory Tools | 后台 manager consolidate | create_search_memory_tool | namespace 级 | Python（绑定 LangGraph） | Python + LangGraph | - | - | LangGraph 生态 |
| **Supermemory** | Memory Engine + 自动遗忘 | 自动遗忘 + 矛盾解决 | Hybrid Search（RAG+Memory） | containerTag 级 | Python/TS + REST + MCP | TypeScript + Cloud SaaS | 24.6k | LongMemEval **81.6%** | 个人+商业 AI |
| **Cognee** | ECL 三阶段 + 知识图谱 | forget API 手动 | 图遍历多跳推理 | 实体/三元组级 | Python + CLI + MCP | Python + 图数据库 | 16.6k | 关系推理 **92.5%** | 知识图谱推理 |
| **OpenAI Responses** | previous_response_id 链式 | 服务端托管（黑盒） | 内置不可控 | 会话级 | OpenAI SDK | OpenAI 云 | - | - | OpenAI 生态 |
| **Hippocampus** | **时序归档（完整非摘要）+ 三级周期** | **天归档/周去重/月评分淘汰** | **摘要钩子注入 + tool 主动检索** | **17 类消息级标签** | **C ABI + HTTP + Python** | **Rust 单二进制 + SQLite** | 新项目 | 待测 | 编程助手/通用 |

## 三大直接竞品深度对比

### 1. agentmemory（🔴 高威胁，同字段直接竞争）

**威胁点**：定位高度重叠（AI Coding Agent 记忆）、23k stars、12+ Agent 原生插件（Claude Code/Cursor/Codex 等）、MCP 生态先发、Recall@5 95.2%。

**Hippocampus 差异化卖点**：
1. **无损归档 vs iii 压缩摘要式**：agentmemory 用 iii-engine 压缩后存储，丢失原始对话保真度；Hippocampus 完整保存非摘要，可追溯
2. **17 类标签 vs 单层 Observation**：检索粒度更细，支持按"工具调用/思考过程/代码块/图片"等维度筛选
3. **三级周期淘汰 vs 无显式周期**：agentmemory 无周级合并/月级淘汰机制，记忆只增不减
4. **Rust 单二进制 vs npm 工具链**：部署更轻，适合本地化/隐私场景
5. **C ABI 嵌入式接口**：可嵌入宿主进程，agentmemory 无此能力

### 2. Zep / Graphiti（🟡 中威胁，路线不同）

**威胁点**：时序图谱路线 + arXiv 2501.13956 论文背书、bi-temporal 事实失效是独家护城河、企业级生态。

**Hippocampus 差异化卖点**：
1. **零外部依赖部署**：Zep 需 Postgres + 图数据库 + Docker（GB 级）；Hippocampus 一个二进制 + SQLite
2. **无损时序回溯**：Zep 抽取三元组后丢失原始对话逐字保真；Hippocampus 保留全量
3. **C ABI 嵌入**：可嵌入 Edge/IoT/桌面应用，Zep 仅 REST/SDK
4. **月度 4 维加权淘汰**：Zep 是事实级失效（不删除），Hippocampus 是周期性全局淘汰

### 3. Letta / MemGPT（🟡 中威胁，定位错位）

**威胁点**：Sleep-time Compute 论文级理论（arxiv:2504.13171）、伯克利背书、Memory Blocks 自我编辑。

**Hippocampus 差异化卖点**：
1. **库 vs 平台**：Letta 是 Agent 平台（必须用其 Agent SDK）；Hippocampus 是记忆库（任何 Agent 通过 C ABI/REST 接入）
2. **轻量嵌入**：Letta 部署需 Python + Postgres + sandbox；Hippocampus 一个二进制
3. **完整对话保真**：Letta Memory Blocks 是 LLM 自我整理的摘要；Hippocampus 无损归档
4. **确定性周期 vs LLM 即兴整理**：Hippocampus 是确定性天/周/月周期，Letta 是 Sleep-time Agent 即兴

## 护城河：难以复制的能力

### 1. 三级索引周期 + 4 维加权评分淘汰（独家）
所有竞品要么不淘汰（Mem0 ADD-only、agentmemory 无周期），要么事实级失效（Zep）——**周期性全局淘汰**是 Hippocampus 独家。

### 2. 完整对话非摘要归档（独家）
所有竞品都走"压缩/抽取/摘要"路径——**保真度**是 Hippocampus 独家，适合合规/审计场景。

### 3. Rust 单二进制 + C ABI 嵌入（独家）
所有竞品都需要 Python/Node 运行时 + 外部数据库；Hippocampus 是唯一可嵌入宿主进程的方案。

### 4. 17 类消息级标签体系（最细粒度）
竞品要么无标签（Mem0）、要么单层 Observation（agentmemory）、要么实体级（Zep/Cognee）——**消息级 17 类标签**粒度独家。

## 明确放弃的方向（聚焦）

- ❌ **不做 Agent 平台**：Letta 路线太重，会绑定生态
- ❌ **不做 SaaS**：本地化是核心卖点，不被融资叙事带偏
- ❌ **不做用户画像/事实提取**：Mem0 $24M 融资 + 94.8%，正面对抗无胜算
- ❌ **不做知识图谱推理**：Cognee/Zep 在多跳推理 92.5%+，不擅长

## 市场机会窗口

截至 2026 年 7 月：
1. **没有任何主流向量库原生支持时序记忆**——Hippocampus 是向量库的时序前置层
2. **8 个多 Agent 编排平台无一家同时具备"时序归档+三级周期+17类标签+三层接口"**——差异化机会明确
3. **大厂内建的都是"轻量偏好记忆"**（Cursor 评分筛选/Trae 20 条/Claude CLAUDE.md）——重基础设施是空白生态位
4. **RAG 框架 Memory 模块在生产环境普遍翻车**（LangChain 旧 Memory 多用户并发串号）——Hippocampus 正好补齐

## 目标集成场景优先级

| 优先级 | 场景 | 集成路径 |
|---|---|---|
| ★★★★★ | AI 编程助手 MCP 记忆层 | MCP server → Claude Code/Cursor/Trae |
| ★★★★ | RAG 框架时序记忆后端 | LlamaIndex ChatStore / LangChain Memory 适配器 |
| ★★★ | 多 Agent 编排统一记忆层 | LangGraph Store / AutoGen Memory protocol 适配器 |

## 参考来源

- [agentmemory GitHub](https://github.com/rohitg00/agentmemory)
- [Zep 论文 arXiv:2501.13956](https://arxiv.org/abs/2501.13956)
- [Letta Sleep-time Compute arXiv:2504.13171](https://arxiv.org/abs/2504.13171)
- [Mem0 论文 arXiv:2504.19413](https://arxiv.org/abs/2504.19413)
- [Letta Memory Blocks 文档](https://docs.letta.com/guides/agents/memory-blocks)
- [LangGraph Persistence 文档](https://docs.langchain.com/oss/python/langgraph/persistence)
- [2025 AI 记忆系统大横评](http://m.toutiao.com/group/7578151050135044643/)
