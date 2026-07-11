# DeepSeek 网页端接入 MemoryCenter 记忆库 Onboarding 指南

> 适用版本：MemoryCenter v2.36+ / DeepSeek 网页端 + DeepSeek++ 浏览器扩展
>
> 本指南教你如何在 DeepSeek 网页端中接入 memory-center MCP server，让 LLM 自动获得长期记忆能力。
> 传输模式：Streamable HTTP（远程访问，v2.36 起）。

---

## 架构概览

```
┌──────────────────────────────────────────────────────┐
│  DeepSeek 网页端（浏览器）                             │
│                                                      │
│  ┌──────────────┐    ┌──────────────────────────┐   │
│  │ DeepSeek LLM │◄──►│ DeepSeek++ 浏览器扩展     │   │
│  │              │    │  - MCP 客户端            │   │
│  │              │    │  - 长期记忆（有限）       │   │
│  │              │    │  - 工具执行 / 浏览器控制   │   │
│  └──────────────┘    └──────────┬───────────────┘   │
│                                 │                    │
└─────────────────────────────────┼────────────────────┘
                                  │ MCP Streamable HTTP
                                  │（HTTPS）
                                  ▼
┌──────────────────────────────────────────────────────┐
│  远程服务器                                            │
│                                                       │
│  memory-center-server（Axum HTTP）                    │
│    ├─ /mcp       → MCP Streamable HTTP 端点           │
│    ├─ /api/v1/*  → REST API（归档/检索/摘要等）       │
│    └─ 读写本地存储 .mcp-data/                         │
│                                                       │
│  memory-center-mcp（可选，供本地 IDE 客户端使用）      │
└───────────────────────────────────────────────────────┘
```

### 为什么需要这种模式

DeepSeek 网页端本身是一个闭源 Web 应用，无法直接安装本地二进制。DeepSeek++ 浏览器扩展为其增加了 MCP 客户端能力，但浏览器扩展无法启动本地子进程（stdio 模式不可用）。因此必须使用 **Streamable HTTP 模式**，将 memory-center 部署在远程服务器上，浏览器扩展通过 HTTPS 连接。

### 与本地 IDE 接入的区别

| 维度 | 本地 IDE（Trae/Cursor/Claude Code） | DeepSeek 网页端 + DeepSeek++ |
|------|-------------------------------------|------------------------------|
| 传输模式 | stdio（本地子进程） | Streamable HTTP（远程） |
| 部署位置 | 本地机器 | 远程服务器（需 HTTPS） |
| MCP 客户端 | IDE 内置 | DeepSeek++ 浏览器扩展 |
| 被动归档（sidecar） | 不适用（闭源 IDE 无 compaction 事件） | 不适用（网页端无本地 DB） |
| 主动归档 | LLM 调 `archive` / `pre_compress_hook` | LLM 调 `archive` / `pre_compress_hook` |
| 记忆召回 | LLM 调 `prompt` / `semantic_search` | LLM 调 `prompt` / `semantic_search` |

> **关键区别**：DeepSeek 网页端没有本地 SQLite 数据库，也没有 compaction 事件，因此 **sidecar 被动归档层不适用**。所有归档都由 LLM 主动调用 `archive` 工具完成（伪钩子方案）。

---

## 1. 前置准备

### 1.1 部署 memory-center-server 到远程服务器

DeepSeek 网页端需要通过 HTTPS 连接，因此 memory-center-server 必须部署在具有公网 IP + 域名 + SSL 证书的服务器上。

```bash
# 在远程服务器上构建（需要 Rust 1.85+）
git clone <memory-center-repo>
cd memory-center
cargo build --release -p memory-center-server

# 二进制位置
./target/release/memory-center-server   # Linux
./target/release/memory-center-server.exe   # Windows
```

### 1.2 准备存储目录

```bash
mkdir -p /var/lib/memory-center-data
```

### 1.3 配置 SSL 反向代理（必需）

DeepSeek++ 浏览器扩展要求 MCP 端点为 HTTPS。使用 Nginx/Caddy 反向代理：

```nginx
# Nginx 配置示例
server {
    listen 443 ssl;
    server_name memory.your-domain.com;

    ssl_certificate     /path/to/cert.pem;
    ssl_certificate_key /path/to/key.pem;

    location / {
        proxy_pass http://127.0.0.1:8765;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_http_version 1.1;
        proxy_set_header Connection "";  # 支持 SSE 长连接
        proxy_buffering off;  # SSE 需要关闭缓冲
    }
}
```

---

## 2. 启动 memory-center-server

### 2.1 环境变量配置

```bash
# 必需
export MEMORY_CENTER_ROOT=/var/lib/memory-center-data
export MEMORY_CENTER_MCP_ENABLED=true        # 启用 /mcp 端点
export MEMORY_CENTER_MCP_ALLOWED_HOSTS=memory.your-domain.com  # DNS rebinding 防护
export MEMORY_CENTER_API_KEY=your-secret-key  # 鉴权（强烈建议设置）

# 可选：LLM 摘要生成器（推荐开启）
export MEMORY_CENTER_GENERATOR_API_URL=https://api.siliconflow.cn/v1/chat/completions
export MEMORY_CENTER_GENERATOR_API_KEY=sk-xxx
export MEMORY_CENTER_GENERATOR_MODEL=Qwen/Qwen2.5-7B-Instruct

# 可选：语义检索（推荐开启）
export MEMORY_CENTER_EMBEDDER_API_URL=https://api.siliconflow.cn/v1/embeddings
export MEMORY_CENTER_EMBEDDER_API_KEY=sk-xxx
export MEMORY_CENTER_EMBEDDER_MODEL=BAAI/bge-m3

# 可选：冲突检测器
export MEMORY_CENTER_DETECTOR_API_URL=https://api.deepseek.com/chat/completions
export MEMORY_CENTER_DETECTOR_API_KEY=sk-xxx
export MEMORY_CENTER_DETECTOR_MODEL=deepseek-chat

# 可选：Preset 显式声明（DeepSeek 网页端不在 11 个预设内，建议手动指定）
export MEMORY_CENTER_PRESET_AGENT=DeepSeek
export MEMORY_CENTER_PRESET_SCENARIO=daily
```

### 2.2 启动服务

```bash
# 前台启动（调试用）
RUST_LOG=memory_center_server=info,tower_http=info \
  ./target/release/memory-center-server

# 后台启动（生产环境，推荐用 systemd）
sudo systemctl start memory-center
```

### 2.3 验证服务可用

```bash
# 健康检查
curl https://memory.your-domain.com/api/v1/presets/agents \
  -H "Authorization: Bearer your-secret-key"

# MCP 端点检查（应返回 4xx 表示端点存在但需要正确的 MCP 请求）
curl -X POST https://memory.your-domain.com/mcp \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer your-secret-key" \
  -d '{"jsonrpc":"2.0","method":"initialize","params":{},"id":1}'
```

---

## 3. 配置 DeepSeek++ 浏览器扩展

### 3.1 安装 DeepSeek++

DeepSeek++ 是社区开发的开源浏览器扩展（非 DeepSeek 官方产品），支持 Chrome / Edge / Firefox。

1. 在浏览器扩展商店搜索 "DeepSeek++" 并安装
2. 或从开源仓库下载 .crx / .xpi 文件手动安装

### 3.2 配置 MCP server 连接

打开 DeepSeek++ 扩展设置，找到 MCP server 配置项：

| 字段 | 值 |
|------|-----|
| 名称 | `memory-center` |
| URL | `https://memory.your-domain.com/mcp` |
| 传输类型 | `streamable-http` |
| Authorization | `Bearer your-secret-key`（对应服务器端 `MEMORY_CENTER_API_KEY`） |

> **安全提示**：API Key 在浏览器扩展中存储，请确保服务器端配置了速率限制和日志监控。

### 3.3 验证连接

1. 打开 DeepSeek 网页端
2. 确认 DeepSeek++ 扩展已激活
3. 新建对话，输入：

```
你好，请调用 memory-center 的 get_config 工具，告诉我当前的配置信息
```

4. 观察 LLM 是否成功调用 `get_config` 工具并返回配置 JSON

---

## 4. session_id 约定

memory-center 用 `session_id` 隔离不同会话的记忆。推荐约定：

```
deepseek-{项目名}-{日期}
```

示例：
- `deepseek-myapp-20260705`
- `deepseek-research-20260711`

> 一个 session_id 对应一个独立的记忆空间。同会话内复用同一 session_id，
> 切换项目或日期时换新 session_id。

---

## 5. 主动归档协议（伪钩子方案）

DeepSeek 网页端没有 compaction 事件，所有归档由 LLM 主动调用完成。LLM 需遵守以下协议：

### 5.1 会话开始：调 prompt 获取历史记忆

每个新会话的**第一次回复前**，必须先调用：

```
mcp_memory-center.prompt(session_id)
```

### 5.2 上下文接近阈值：主动调 archive 归档

触发条件（满足任一即调用）：
- 对话长度超过 20 轮
- 内容复杂度高（大量代码 / 长文档 / 多次工具调用）
- 主观判断"上下文开始变重"
- 上次 archive 返回的 `threshold_ratio_percent >= 80`

```
mcp_memory-center.archive(
    session_id,
    turns_json,  // [{"user_message":{"text":"..."},"llm_message":{"text":"..."}}]
    project_id
)
```

### 5.3 用户提到过去事件：先调 semantic_search

当用户消息中出现"之前""上次""还记得"等指代过去的词语时：

```
mcp_memory-center.semantic_search(query, session_id, top_k=5)
```

### 5.4 完成开发阶段：调 update_project_memory

```
mcp_memory-center.update_project_memory(
    project_id="myapp",
    section="task_state",
    content="## 当前任务\n- xxx 已完成",
    action="replace"
)
```

> 完整协议见仓库根目录 `AGENTS.md`。

---

## 6. install_rules 远程模式

由于 MCP server 部署在远程，`install_rules` 工具无法直接写入客户端本地文件。v2.37 起支持远程模式：

1. LLM 调用 `install_rules(client="deepseek", project_root="C:/Users/xxx/project")`
2. 工具检测到路径不存在（远程模式），返回 `action=remote_template` + `files[]` 数组
3. LLM 用 Write 工具按 `relative_path` 创建文件

> **注意**：DeepSeek 网页端没有本地文件系统访问能力，`install_rules` 远程模式对 DeepSeek 网页端意义有限。建议通过 DeepSeek++ 的自定义指令 / 系统提示功能手动注入记忆协议规则。

---

## 7. 跨 Agent 记忆共享

MemoryCenter 支持跨 Agent 记忆共享。DeepSeek 网页端与本地 IDE（Trae/Cursor/Claude Code）可以共享同一份记忆库：

```
# 场景：用户在 Trae 中完成开发，切换到 DeepSeek 网页端继续讨论

# Trae 端（stdio 模式，本地）
session_id = "trae-myapp-20260705"
→ 归档开发过程、架构决策、代码片段

# DeepSeek 网页端（Streamable HTTP 模式，远程）
session_id = "deepseek-myapp-20260705"
→ 调 prompt 拉取记忆（可看到 Trae 的归档）
→ 调 semantic_search 检索特定内容
```

### 跨 Agent 检索机制

`prompt` 工具返回的信息包含：
- **Current Agent Context**：当前 Agent family + 钩子模式
- **Cross-Agent Summary**：同 project 下其他 Agent 的 session 列表（按 family 分组，最多 10 个）

LLM 可据此主动调 `retrieve` / `semantic_search` 检索其他 Agent 的记忆。

---

## 8. 故障排查

### 8.1 MCP 连接失败

- 检查服务器 `MEMORY_CENTER_MCP_ENABLED=true` 是否设置
- 检查 Nginx/Caddy 反向代理是否正确转发 `/mcp` 端点
- 检查 `proxy_buffering off` 是否设置（SSE 需要关闭缓冲）
- 检查 `MEMORY_CENTER_MCP_ALLOWED_HOSTS` 是否包含你的域名

### 8.2 401/403 鉴权错误

- 确认 DeepSeek++ 扩展中配置的 API Key 与服务器端 `MEMORY_CENTER_API_KEY` 一致
- 确认 Authorization 头格式为 `Bearer <key>`（注意空格）
- 检查服务器日志确认请求到达

### 8.3 LLM 不主动调用记忆工具

- DeepSeek++ 扩展需要在系统提示中注入 AGENTS.md 内容（或 MemoryCenter 的 `instructions` 字段）
- 确认 `get_config` 工具返回的 `instructions` 字段非空
- 尝试在对话中明确提示："请先调用 prompt 工具获取历史记忆"

### 8.4 CORS 错误

- 检查 `MEMORY_CENTER_MCP_ALLOWED_ORIGINS` 是否包含 DeepSeek 网页端的 Origin
- 默认为空（不校验 Origin），生产环境建议设置为 `https://chat.deepseek.com`

### 8.5 semantic_search 返回空

- 未配置 `MEMORY_CENTER_EMBEDDER_API_URL`，降级为仅关键词检索（BM25）
- 确认对应 session_id 下有已归档的记忆
- 尝试用 `summaries` 工具确认记忆是否存在

---

## 9. 安全注意事项

### 9.1 API Key 保护

- `MEMORY_CENTER_API_KEY` 是访问记忆库的唯一凭证，切勿泄露
- DeepSeek++ 扩展中的 API Key 存储在浏览器本地，切换设备时需重新配置
- 建议定期轮换 API Key

### 9.2 数据隐私

- 所有对话内容（经 archive 工具归档后）存储在远程服务器的 `.mcp-data/` 目录
- 记忆文件为明文 JSON，建议对磁盘加密（LUKS / BitLocker）
- 生产环境建议配置 HTTPS 端到端加密

### 9.3 访问控制

- 服务器防火墙仅开放 443 端口
- `MEMORY_CENTER_MCP_ALLOWED_HOSTS` 限制允许的 Host 头
- `MEMORY_CENTER_MCP_ALLOWED_ORIGINS` 限制允许的 Origin
- 考虑配置 Nginx 速率限制防止滥用

---

## 10. 下一步

- 阅读 [架构文档](../ARCHITECTURE.md) 了解 memory-center 内部设计
- 参考 [部署文档](../DEPLOY.md) 配置 systemd 服务和日志收集
- 如需本地 IDE 接入，参考 [Trae 接入指南](trae.md) 或 [OpenCode 接入指南](opencode.md)
- 调整 `MEMORY_CENTER_PRESET_SCENARIO` 适配你的工作场景（daily / writing / research / coding 等）
