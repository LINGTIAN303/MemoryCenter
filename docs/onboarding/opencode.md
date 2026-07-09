# OpenCode 接入 MemoryCenter 记忆库 Onboarding 指南

> 适用版本：MemoryCenter v2.39+ / OpenCode（dev 分支，`packages/core` schema）
>
> 本指南教你如何在 OpenCode 中接入 memory-center，让 LLM 自动获得长期记忆能力。
> OpenCode 适配采用**三路径互补架构**，覆盖完整的记忆保存与召回闭环。

---

## 架构总览：三路径互补

```
┌─────────────────────────────────────────────────────────────┐
│                    OpenCode Agent                           │
│                                                             │
│  路径一（主动召回）    路径二（行为规范）    路径三（被动保存）│
│  MCP client 接入      AGENTS.md 协议        sidecar 进程    │
│  ───────────────      ──────────────        ───────────     │
│  LLM 主动调用:        规范 LLM 行为:        自动监听压缩:   │
│  - prompt             - 何时调哪个工具      - compaction 消息│
│  - archive            - session_id 约定       新增检测      │
│  - semantic_search    - 压缩后行为协议     - 自动 pre-compress│
│                                             归档            │
└──────────────────┬──────────────────────────────────────────┘
                   │
                   ▼
         ┌─────────────────┐
         │  MemoryCenter   │
         │  (MCP + REST)   │
         └─────────────────┘
```

| 路径 | 层级 | 机制 | 自动化程度 |
|------|------|------|-----------|
| 路径一 | 主动召回层 | OpenCode 配置 MemoryCenter 为 MCP server，LLM 主动调工具 | LLM 自主决策 |
| 路径二 | 行为规范层 | install_rules 写入 `.opencode/rules/` + AGENTS.md | 协议约束 |
| 路径三 | 被动保存层 | sidecar 进程轮询 SQLite，检测压缩事件 | 全自动 |

**核心原则**：三条路径互不依赖，任一路径独立可用，组合使用效果最佳。

---

## 1. 前置准备

### 1.1 构建 memory-center-mcp 二进制

```bash
# 从源码构建（需要 Rust 1.88+）
git clone <memory-center-repo>
cd memory-center
cargo build --release -p memory-center-mcp

# 二进制位置
./target/release/memory-center-mcp.exe   # Windows
./target/release/memory-center-mcp       # Linux/macOS
```

### 1.2 构建 mc-sidecar 二进制（路径三，可选但推荐）

```bash
cargo build --release -p memory-center-sidecar

# 二进制位置
./target/release/mc-sidecar.exe          # Windows
./target/release/mc-sidecar              # Linux/macOS
```

### 1.3 准备存储目录

```bash
# MemoryCenter 存储记忆文件的根目录
mkdir -p D:/memory-center-data
```

---

## 2. 路径一：MCP client 接入（主动召回层）

OpenCode 通过 `opencode.jsonc` 的 `mcp` 字段配置 MCP server。支持两种传输模式：

### 2.1 本地 stdio 模式（推荐）

在 OpenCode 全局配置目录创建 `opencode.jsonc`：

- **Linux**: `~/.config/opencode/opencode.jsonc`
- **macOS**: `~/Library/Application Support/opencode/opencode.jsonc`
- **Windows**: `%APPDATA%\opencode\opencode.jsonc`

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "memory-center": {
      "type": "local",
      "command": ["D:/path/to/memory-center-mcp.exe"],
      "environment": {
        "MEMORY_CENTER_ROOT": "D:/memory-center-data"
      }
    }
  }
}
```

### 2.2 OpenCode 桌面端配置（mcpServers 格式）

OpenCode 桌面端兼容 Claude Desktop 风格的 `mcpServers` 配置格式。以下为标准模板（值已模糊处理，部署时替换为实际值）：

```jsonc
{
  "mcpServers": {
    "memory-center": {
      // MCP server 启动命令：指向编译产物 memory-center-mcp 二进制
      "command": "<MC_MCP_BINARY_PATH>",
      // 启动参数（若二进制路径含空格用数组形式，否则可省略 args）
      "args": [],
      // 环境变量：控制 MemoryCenter 运行时行为
      "env": {
        // 记忆存储根目录：所有 session/project 记忆文件的存放路径
        "MEMORY_CENTER_ROOT": "<MC_DATA_ROOT>",
        // （可选）显式声明 Agent 客户端类型，跳过自动识别
        // 支持值：Claude Code / Cursor / Trae / Codex / OpenCode 等
        "MEMORY_CENTER_PRESET_AGENT": "OpenCode",
        // （可选）API Key：若 MemoryCenter 服务端开启了鉴权则必填
        "MEMORY_CENTER_API_KEY": "<YOUR_API_KEY_IF_ENABLED>",
        // （可选）归档阈值：超过此 token 数触发归档建议（默认 120000）
        "MEMORY_CENTER_ARCHIVE_THRESHOLD": "120000"
      }
    }
  }
}
```

**部署时替换的占位符**：

| 占位符 | 含义 | 示例 |
|--------|------|------|
| `<MC_MCP_BINARY_PATH>` | memory-center-mcp 二进制绝对路径 | `D:/memory-center/target/release/memory-center-mcp.exe` |
| `<MC_DATA_ROOT>` | 记忆存储根目录 | `D:/memory-center-data` |
| `<YOUR_API_KEY_IF_ENABLED>` | API Key（仅服务端开启鉴权时需要） | `sk-mc-xxxxxxxxxxxxx` |

> **注意**：环境变量前缀必须是 `MEMORY_CENTER_*`（带下划线），不是 `MEMORYCENTER_*`。两者不通用，混用会导致配置不生效。

### 2.3 远程 Streamable HTTP 模式

若 memory-center 部署在远程服务器：

```bash
# 远程服务器启动（同时承载 REST API + MCP Streamable HTTP）
MEMORY_CENTER_MCP_ENABLED=true \
MEMORY_CENTER_ROOT=./data \
MEMORY_CENTER_API_KEY=your-secret-key \
cargo run -p memory-center-server
```

OpenCode 端配置：

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "memory-center": {
      "type": "remote",
      "url": "https://your-server:8765/mcp",
      "headers": {
        "Authorization": "Bearer your-secret-key"
      }
    }
  }
}
```

### 2.4 配置字段说明

| 字段 | 类型 | 说明 |
|------|------|------|
| `mcp` | `Record<string, Local \| Remote>` | MCP server 配置映射 |
| `mcp.<name>.type` | `"local" \| "remote"` | 连接类型 |
| `mcp.<name>.command` | `string[]` | local 模式：启动命令与参数 |
| `mcp.<name>.environment` | `Record<string, string>` | local 模式：环境变量 |
| `mcp.<name>.url` | `string` | remote 模式：服务器 URL |
| `mcp.<name>.headers` | `Record<string, string>` | remote 模式：请求头 |
| `mcp.<name>.enabled` | `boolean?` | 是否启用（默认 true） |
| `mcp.<name>.timeout` | `number?` | 请求超时（ms，默认 5000） |

> **配置文件优先级**：`config.json` < `opencode.json` < `opencode.jsonc`（后者覆盖前者）。

---

## 3. 路径二：install_rules 安装协议文件（行为规范层）

### 3.1 调用 install_rules 工具

在 OpenCode 中启动会话后，让 LLM 调用：

```
mcp_memory-center.install_rules(
    client="opencode",
    project_root="D:/your/project/path"
)
```

### 3.2 安装结果

工具会自动创建以下文件：

| 文件 | 作用 |
|------|------|
| `.opencode/rules/memory-center-archive.md` | OpenCode 专用 Rules 模板（含 MCP 配置示例） |
| `AGENTS.md`（项目根） | 通用 AGENTS.md 协议（含 session_id 约定 + 工具速查表） |

### 3.3 配置 instructions 字段（让 OpenCode 自动加载 Rules）

在 `opencode.jsonc` 中添加 `instructions` 字段：

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "memory-center": {
      "type": "local",
      "command": ["memory-center-mcp"],
      "environment": { "MEMORY_CENTER_ROOT": "D:/memory-center-data" }
    }
  },
  "instructions": [".opencode/rules/memory-center-archive.md"]
}
```

> `instructions` 是 `string[]`，OpenCode 会加载这些文件内容注入 LLM system prompt。
> 若未配置，AGENTS.md 仍会被通用约定加载（OpenCode 会读取项目根的 AGENTS.md）。

### 3.4 远程模式（MCP server 无法访问本地路径时）

若 install_rules 返回 `action=remote_template`，表示 MCP server 在远程无法写入本地文件。
此时 LLM 会收到 `files` 数组，需用 Write 工具自行创建：

1. 创建 `.opencode/rules/memory-center-archive.md`（create 模式）
2. 创建/追加 `AGENTS.md`（append_with_markers 模式）

---

## 4. 路径三：sidecar 被动保存层（全自动归档）

### 4.1 sidecar 工作原理（v2.39 重构）

`mc-sidecar` 是一个独立进程，轮询 OpenCode 的 SQLite 数据库，检测压缩事件。

**检测原理（v2.39 重构）**：

- **旧策略（v2.36，已废弃）**：监控 `session.time_compacting` 字段变化。该字段在 OpenCode 源码（`compaction.ts`）中从未被写入，检测基础不成立。
- **新策略（v2.39）**：轮询 `session_message` 表中 `type='compaction'` 的新消息。压缩完成后 OpenCode 会往该表插入一条 compaction 消息（含 `summary`/`recent`/`reason`），sidecar 用 `message_id` 去重，发现新消息即触发增量归档。

```
OpenCode 压缩流程          sidecar 检测               MemoryCenter 归档
─────────────────          ───────────               ────────────────
会话消息累积到达阈值
  ↓
触发 compactAfterOverflow
  ↓
LLM 生成压缩摘要
  ↓
Compaction.Ended
  ↓
插入 session_message      →   检测到新的 compaction    →  读取增量上下文
type='compaction' 消息        消息（message_id 去重）     (上次 compaction, 本次)
  ↓                                                    ↓
                                                    POST /pre-compress
                                                    归档 + 附加 summary 标签
```

**增量归档范围**：`(上次 compaction seq, 本次 compaction seq)` 之间的消息，天然不重复。compaction 消息的 `summary` 作为高价值标签附加到归档内容末尾。

### 4.2 启动 sidecar

```bash
# 基本启动
mc-sidecar \
  --opencode-db "D:/path/to/opencode.db" \
  --memorycenter-url "http://127.0.0.1:8765" \
  --project-id "myproject"

# 完整参数
mc-sidecar \
  --opencode-db "D:/path/to/opencode.db" \
  --memorycenter-url "http://127.0.0.1:8765" \
  --memorycenter-api-key "your-secret-key" \
  --project-id "myproject" \
  --poll-interval 5 \
  --backfill

# 查看帮助
mc-sidecar --help
```

### 4.3 参数说明

| 参数 | 环境变量 | 默认值 | 说明 |
|------|---------|--------|------|
| `--opencode-db` | `OPENCODE_DB` | 平台默认路径 | OpenCode SQLite 数据库路径 |
| `--memorycenter-url` | `MEMORYCENTER_URL` | `http://127.0.0.1:8765` | MemoryCenter 服务地址 |
| `--memorycenter-api-key` | `MEMORYCENTER_API_KEY` | （无） | API Key（若服务端配置了鉴权） |
| `--project-id` | `PROJECT_ID` | `default` | 项目 ID（影响存储路径） |
| `--poll-interval` | `POLL_INTERVAL` | `5` | 轮询间隔（秒） |
| `--backfill` | - | `false` | 启动时回填历史已压缩 session |
| `--max-turns` | `MAX_TURNS` | `100` | 读取每个 session 的最大轮次数 |

### 4.4 OpenCode SQLite 默认路径

| 平台 | 默认路径 |
|------|---------|
| Linux | `~/.local/share/opencode/opencode.db` |
| macOS | `~/Library/Application Support/opencode/opencode.db` |
| Windows | `%APPDATA%\opencode\opencode.db` |

### 4.5 作为系统服务运行

**Linux（systemd）**：

```ini
# /etc/systemd/system/mc-sidecar.service
[Unit]
Description=MemoryCenter Sidecar for OpenCode
After=network.target

[Service]
ExecStart=/usr/local/bin/mc-sidecar \
  --opencode-db /home/user/.local/share/opencode/opencode.db \
  --memorycenter-url http://127.0.0.1:8765 \
  --project-id myproject
Restart=always
RestartSec=3
User=user

[Install]
WantedBy=multi-user.target
```

**Windows（计划任务或 NSSM）**：

```powershell
# 使用 NSSM 安装为服务
nssm install mc-sidecar "D:\path\to\mc-sidecar.exe" `
  --opencode-db "%APPDATA%\opencode\opencode.db" `
  --memorycenter-url http://127.0.0.1:8765 `
  --project-id myproject
nssm start mc-sidecar
```

---

## 5. 完整记忆恢复闭环

三路径组合后，完整的记忆保存与召回流程：

```
用户与 OpenCode Agent 交互
    ↓
到达阈值（20 轮 / token 反馈 >= 80%）
    ↓
触发压缩（/compact 手动 或 compactIfNeeded 自动）
    ↓
【路径三】sidecar 检测 compaction 新消息 → 自动调 pre-compress 归档增量上下文
    ↓
OpenCode 清空 LLM 上下文，保留摘要
    ↓
【路径二】AGENTS.md 协议触发：系统消息出现 "This session continues..."
    ↓
【路径一】LLM 主动调 mcp_memory-center.prompt(session_id) 拉取历史记忆
    ↓
LLM 获得钩子（高价值记忆摘要）+ 短记忆（OpenCode 摘要）
    ↓
需要更深入信息时 → 调 semantic_search(query, session_id) 检索
    ↓
恢复完整上下文，继续与用户交互
```

---

## 6. 实测验证

### 6.1 验证 MCP 接入（路径一）

1. 启动 OpenCode
2. 在会话中输入：`请调用 mcp_memory-center 的 prompt 工具，session_id 用 opencode-test-20260709`
3. 预期：LLM 调用工具，返回空列表（首次）或历史记忆

### 6.2 验证 install_rules（路径二）

1. 在 OpenCode 会话中：`请调用 install_rules，client=opencode，project_root=当前项目路径`
2. 预期：生成 `.opencode/rules/memory-center-archive.md` 和 `AGENTS.md`

### 6.3 验证 sidecar（路径三）

1. 启动 MemoryCenter 服务：`cargo run -p memory-center-server`
2. 启动 sidecar：`mc-sidecar --opencode-db <path> --memorycenter-url http://127.0.0.1:8765`
3. 在 OpenCode 中触发压缩（输入 `/compact`）
4. 观察 sidecar 日志：应显示 `检测到压缩事件` + `归档成功`
5. 访问 `GET http://127.0.0.1:8765/api/v1/sessions/opencode-<project>-<date>/summaries` 确认记忆已保存

---

## 7. 故障排查

### 7.1 MCP server 未被发现

- 检查 `opencode.jsonc` 的 `mcp.memory-center` 配置
- 确认 `command` 路径正确（Windows 用 `\\` 或 `/`，不能用 `\`）
- 查看 OpenCode 日志（`~/.config/opencode/log/` 或对应平台路径）

### 7.2 sidecar 检测不到压缩事件

- 确认 `--opencode-db` 路径指向正确的 SQLite 文件
- 用 SQLite 工具检查 `session_message` 表是否有 `type='compaction'` 的记录：
  ```sql
  SELECT id, session_id, seq, time_created FROM session_message WHERE type='compaction' ORDER BY time_created DESC LIMIT 5;
  ```
- 确认 OpenCode 版本支持 V2 `session_message` 表（老版本只有 V1 `message`+`part` 表）
- 检查 sidecar 是否有读权限（sidecar 以只读模式打开 SQLite）
- 确认 MemoryCenter 服务已启动且 URL 可达
- **注意**：v2.36 旧策略监控 `session.time_compacting` 字段，但该字段在 OpenCode 源码中从未被写入，v2.39 已改为监控 compaction 消息

### 7.3 归档失败（HTTP 401）

- 若 MemoryCenter 配置了 `MEMORY_CENTER_API_KEY`，sidecar 需传 `--memorycenter-api-key`
- 检查 API Key 是否匹配

### 7.4 install_rules 返回 remote_template

- 这是因为 MCP server 在远程，无法访问本地路径
- 按 `files` 数组的内容，用 Write 工具自行创建文件
- `mode=create`：创建新文件
- `mode=append_with_markers`：文件已存在则在末尾追加，不存在则创建

### 7.5 session_id 格式错误

**正确格式**：`opencode-{项目名}-{日期}`，如 `opencode-myapp-20260709`

**错误格式**：
- `myapp-session`（无前缀 + 用了 "session" 关键词）
- `opencode_myapp_20260709`（用了下划线）
- `OpenCode-myapp-20260709`（大小写错误，应为小写）

---

## 8. 参考

- [OpenCode 配置文档](https://opencode.ai/docs/config)
- [OpenCode MCP 文档](https://opencode.ai/docs/mcp)
- [MemoryCenter 架构文档](../ARCHITECTURE.md)
- [MemoryCenter 部署文档](../DEPLOY.md)
- [Trae 接入指南](./trae.md)（其他客户端接入参考）
- sidecar 源码：`crates/memory-center-sidecar/`
