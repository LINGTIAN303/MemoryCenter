# Trae 接入 MemoryCenter 记忆库 Onboarding 指南

> 适用版本：MemoryCenter v2.37+ / Trae 1.x
>
> 本指南教你如何在 Trae 中接入 memory-center MCP server，让 LLM 自动获得长期记忆能力。
> 支持两种传输模式：stdio（本地子进程，v2.3 起）+ Streamable HTTP（远程访问，v2.36 起）。

---

## 1. 前置准备

### 1.1 安装 memory-center-mcp 二进制

```bash
# 从源码构建（需要 Rust 1.85+，rmcp 1.8 要求 edition 2024 编译器）
git clone <memory-center-repo>
cd memory-center
cargo build --release -p memory-center-mcp

# 二进制位置
./target/release/memory-center-mcp.exe   # Windows
./target/release/memory-center-mcp       # Linux/macOS
```

或直接使用预编译二进制（如有发布）。

### 1.2 准备存储目录

```bash
# memory-center 存储记忆文件的根目录（任意可写路径）
mkdir -p D:/memory-center-data
```

---

## 2. 在 Trae 中添加 MCP server

### 2.1 通过 Trae UI 添加（stdio 模式，推荐用于本地）

1. 打开 Trae → 设置 → MCP 服务器
2. 点击「添加 MCP 服务器」
3. 选择类型：**stdio**（本地子进程）
4. 填入配置：

| 字段 | 值 |
|------|-----|
| 名称 | `memory-center` |
| 命令 | `D:/path/to/memory-center-mcp.exe`（替换为实际路径） |
| 参数 | （留空） |

### 2.2 Streamable HTTP 模式（v2.36 新增，用于远程访问）

若 memory-center 部署在远程服务器（如 Web 端 Agent 接入、多客户端共享场景），可使用 Streamable HTTP 模式：

```bash
# 在远程服务器启动 Axum 服务（同时承载 REST API + MCP Streamable HTTP）
MEMORY_CENTER_MCP_ENABLED=true MEMORY_CENTER_ROOT=./data cargo run -p memory-center-server
```

Trae 端配置：

| 字段 | 值 |
|------|-----|
| 名称 | `memory-center` |
| URL | `https://your-server/mcp` |
| 传输类型 | `streamable-http` |

> stdio 模式适合单客户端本地使用（零配置），Streamable HTTP 模式适合多客户端共享远程访问。

### 2.3 环境变量配置

在 Trae MCP 服务器的「环境变量」栏添加：

#### 必需环境变量

| 变量名 | 说明 | 示例值 |
|--------|------|--------|
| `MEMORY_CENTER_ROOT` | 存储根目录 | `D:/memory-center-data` |
| `RUST_LOG` | 日志级别 | `info` |

#### 可选：LLM 摘要生成器（推荐开启）

配置后 `archive` 工具会调用 LLM 生成结构化摘要（title + abstract + key_facts + key_entities）。
未配置时降级为启发式摘要（首条消息前 80 字符）。

| 变量名 | 说明 | 默认值 |
|--------|------|--------|
| `MEMORY_CENTER_GENERATOR_API_URL` | LLM API 地址（OpenAI 兼容 `/v1/chat/completions`） | 空（降级） |
| `MEMORY_CENTER_GENERATOR_API_KEY` | API Key | 空 |
| `MEMORY_CENTER_GENERATOR_MODEL` | 模型名 | `gpt-5.5-instant` |
| `MEMORY_CENTER_GENERATOR_TIMEOUT` | 超时秒数 | `60` |
| `MEMORY_CENTER_GENERATOR_MAX_TOKENS` | LLM 最大输出 token | `500` |

#### 可选：语义检索（推荐开启）

配置后 `semantic_search` 工具可用混合检索（关键词 + 向量）。
未配置时降级为仅关键词检索（BM25）。

| 变量名 | 说明 | 默认值 |
|--------|------|--------|
| `MEMORY_CENTER_EMBEDDER_API_URL` | Embedding API 地址（OpenAI 兼容 `/v1/embeddings`） | 空（降级） |
| `MEMORY_CENTER_EMBEDDER_API_KEY` | API Key | 空 |
| `MEMORY_CENTER_EMBEDDER_MODEL` | 模型名 | `text-embedding-3-large` |
| `MEMORY_CENTER_EMBEDDER_DIM` | 向量维度 | `3072` |

#### 可选：冲突检测器（推荐开启）

配置后 `detect_conflicts` 工具使用 LLM 语义级检测。
未配置时降级为启发式纯算法（三维度检测）。

| 变量名 | 说明 | 默认值 |
|--------|------|--------|
| `MEMORY_CENTER_DETECTOR_API_URL` | LLM API 地址 | 空（降级） |
| `MEMORY_CENTER_DETECTOR_API_KEY` | API Key | 空 |
| `MEMORY_CENTER_DETECTOR_MODEL` | 模型名 | `gpt-5.5-instant` |

#### 可选：Preset 显式声明（v2.3 新增）

memory-center 启动时会自动识别 Agent 客户端（3 层信号融合，覆盖 11 个 Agent 预设）。
若自动识别失败或需强制指定，可设置：

| 变量名 | 说明 | 示例值 |
|--------|------|--------|
| `MEMORY_CENTER_PRESET_AGENT` | 强制声明 Agent family | `Trae` / `ClaudeCode` / `Cursor` / `Codex` |
| `MEMORY_CENTER_PRESET_SCENARIO` | 强制声明场景 | `coding` / `writing` / `research` / `daily` / `finance` / `design` / `officework` |

> 若不设置，memory-center 会按 Agent family 自动推导 scenario：
> - ClaudeCode / Cursor / Trae / Codex → `coding`
> - 其他 → `daily`

---

## 3. session_id 约定

memory-center 用 `session_id` 隔离不同会话的记忆。推荐约定：

```
trae-{项目名}-{日期}
```

示例：
- `trae-memory-center-20260705`
- `trae-myapp-20260705`

> 一个 session_id 对应一个独立的记忆空间。同会话内复用同一 session_id，
> 切换项目或日期时换新 session_id。

---

## 4. 验证接入

### 4.1 启动 Trae 后检查日志

memory-center 启动时会在 stderr 输出识别日志（Trae 的 MCP 日志面板可见）：

```
INFO  Agent 客户端识别：3 层信号融合完成  family=Trae source=EnvVarPrefix
INFO  应用预设：按 Agent family 推导 scenario  family=Trae scenario=coding
INFO  行为契约生成完成：usage_protocol 已就绪  archive_threshold=400000 session_prefix="trae" instructions_len=520 trigger_rules_count=4
INFO  启动 MemoryCenter MCP server (stdio 传输)  root=D:/memory-center-data has_combined_profile=true
```

若 `has_combined_profile=false`，说明未识别为主流 Agent，检查：
- 是否设置了 `MEMORY_CENTER_PRESET_AGENT=Trae`（最可靠的识别方式）
- Trae 是否传递了 `TRAE_*` 前缀的环境变量

### 4.2 验证 LLM 主动调用

在 Trae 中新建对话，输入：

```
你好，请介绍一下你自己
```

观察 LLM 是否在第一次回复前调用了 `mcp_memory-center.prompt` 工具。
若已调用并返回历史记忆摘要，说明接入成功。

### 4.3 验证 archive 触发

进行一段较长对话（或人为塞入大量文本让 token 接近 400K），
观察 LLM 是否在阈值附近调用 `mcp_memory-center.archive` 归档上下文。

---

## 5. AGENTS.md（项目级规则）

### 5.1 自动安装（v2.37 新增，推荐）

v2.37 起，`install_rules` 工具支持远程模式：在 HTTPS MCP 模式下，LLM 可主动调用 `install_rules` 工具，工具返回 AGENTS.md / Trae Rules 模板内容，LLM 用 Write 工具直接创建文件，**零配置接入**。

stdio 模式下，`install_rules` 工具会直接将模板写入本地文件（需 `MEMORY_CENTER_ROOT` 可写）。

### 5.2 手动创建（适用于所有版本）

在项目根目录创建 `AGENTS.md`，Trae / Cursor 等 IDE 会自动读取并注入 LLM 的 system prompt：

```markdown
# 本项目已接入 memory-center 记忆库

## Agent 必须遵守的记忆协议

1. 每个新会话开始 → 第一步调 mcp_memory-center.prompt(session_id) 获取历史记忆
2. 上下文接近 400K tokens → 调 mcp_memory-center.archive(session_id, turns_json) 归档
3. 用户提到「之前」「上次」「还记得」等词 → 先调 mcp_memory-center.semantic_search(query, session_id) 再回复
4. 用户陈述与记忆可能矛盾 → 先调 mcp_memory-center.detect_conflicts(session_id, statement) 检测

## session_id 约定

trae-{项目名}-{日期}

示例：trae-myapp-20260705
```

> 完整模板见仓库根目录 `AGENTS.md`。

---

## 6. 故障排查

### 6.1 MCP server 启动失败

- 检查 `MEMORY_CENTER_ROOT` 路径是否存在且可写
- 检查二进制路径是否正确
- 查看 Trae MCP 日志面板的 stderr 输出

### 6.2 LLM 不主动调用记忆工具

- 确认 `AGENTS.md` 已放在项目根目录
- 确认 memory-center 启动日志中 `has_combined_profile=true`
- 确认 `get_info` 返回的 `instructions` 字段非空（LLM 启动时应看到记忆协议）

### 6.3 semantic_search 报 501

- 未配置 `MEMORY_CENTER_EMBEDDER_API_URL`，memory-center 降级为仅关键词检索
- 若仍报 501，检查 `SessionSearchRouter` 是否注入成功（看启动日志）

### 6.4 archive 生成的摘要质量差

- 未配置 `MEMORY_CENTER_GENERATOR_API_URL`，使用启发式摘要（首条消息前 80 字符）
- 配置 LLM API 后重试

---

## 7. 下一步

- 阅读 [架构文档](../ARCHITECTURE.md) 了解 memory-center 内部设计
- 调整 `MEMORY_CENTER_PRESET_SCENARIO` 适配你的工作场景（coding/writing/research/daily/finance/design/officework 7 个内置 Scenario）
- 如部署在远程服务器，参考 [部署文档](../DEPLOY.md) 配置 MCP Streamable HTTP 模式
