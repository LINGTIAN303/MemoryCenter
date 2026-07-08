# MemoryCenter HTTP 服务部署指南

本文档记录将 MemoryCenter HTTP 服务部署到 Linux 生产服务器的完整流程。
已在 `162.211.183.236`（openworld.dpdns.org）上验证通过。

> 当前版本：v2.37（含 MCP Streamable HTTP 端点，v2.36+ 启用）

## 架构概览

```
公网请求 https://openworld.dpdns.org/memory-center/api/v1/...  或  /memory-center/mcp
    ↓
Nginx 80 端口（location /memory-center/ 反代 + SSE 流支持）
    ↓ proxy_pass http://127.0.0.1:8765/
systemd 守护进程 memory-center.service
    ↓
/opt/memory-center/bin/memory-center-server（Rust 单二进制，约 9-10MB）
    ↓
/opt/memory-center/data/（SQLite + 文件树存储）
```

## 前置条件

- Linux 服务器（x86_64），已安装 Nginx
- Rust 工具链（用于编译，也可交叉编译后上传二进制）
- 已开放 80/443 端口出方向

## 1. 编译二进制

### 方案 A：服务器直接编译（推荐，避免交叉编译问题）

```bash
# 1. 拉取代码
git clone https://github.com/lingtian303/MemoryCenter.git /root/MemoryCenter-work
cd /root/MemoryCenter-work

# 2. 编译 release 二进制（约 5-10 分钟）
cargo build --release -p memory-center-server

# 3. 部署二进制
mkdir -p /opt/memory-center/bin /opt/memory-center/data
cp target/release/memory-center-server /opt/memory-center/bin/
```

### 方案 B：本地交叉编译后上传

```powershell
# Windows 上交叉编译 Linux x86_64（需要 x86_64-unknown-linux-gnu 工具链）
rustup target add x86_64-unknown-linux-gnu
cargo build --release --target x86_64-unknown-linux-gnu -p memory-center-server

# 上传
scp target/x86_64-unknown-linux-gnu/release/memory-center-server root@SERVER:/opt/memory-center/bin/
```

> **注意**：交叉编译需要 `x86_64-linux-gnu-gcc`，Windows 上配置较复杂，推荐方案 A。

## 2. 配置环境变量

编辑 systemd 服务文件（见下一步），或在 `/etc/profile.d/memory-center.sh` 中配置：

| 环境变量 | 说明 | 默认值 | 必填 |
|---------|------|--------|------|
| `MEMORY_CENTER_HOST` | 监听地址 | `127.0.0.1` | 否（公网通过 Nginx 暴露） |
| `MEMORY_CENTER_PORT` | 监听端口 | `8765` | 否 |
| `MEMORY_CENTER_ROOT` | 存储根目录（SQLite + 文件树） | `./data` | 是 |
| `MEMORY_CENTER_API_KEY` | API Key 鉴权（v2.24） | 空（不鉴权） | **生产强烈建议配置** |
| `MEMORY_CENTER_MCP_ENABLED` | 启用 MCP Streamable HTTP 端点（v2.36+） | `false` | 否 |
| `MEMORY_CENTER_MCP_STATEFUL` | MCP session 模式（true: SSE 流 + session 管理） | `true` | 否 |
| `MEMORY_CENTER_MCP_ALLOWED_HOSTS` | DNS rebinding 防护：允许的 Host 列表（逗号分隔） | `localhost,127.0.0.1,::1` | 否 |
| `MEMORY_CENTER_MCP_ALLOWED_ORIGINS` | CORS 防护：允许的 Origin 列表（逗号分隔） | 空（不校验） | 否 |
| `MEMORY_CENTER_PRESET_AGENT` | HTTP 模式下的 Agent 预设（如 `trae` / `cursor`） | 空 | 否（HTTP 模式推荐配置） |
| `RUST_LOG` | 日志级别 | `memory_center_server=info,tower_http=info` | 否 |

### 可选增强组件

| 环境变量前缀 | 功能 | 未配置时行为 |
|------------|------|------------|
| `MEMORY_CENTER_EMBEDDER_*` | 语义检索（向量+BM25 混合） | 降级为仅 BM25 关键词检索 |
| `MEMORY_CENTER_DETECTOR_*` | 冲突检测（启发式+LLM 混合） | 降级为启发式纯算法 |
| `MEMORY_CENTER_GENERATOR_*` | LLM 摘要生成 | 降级为启发式 `Summary::from_title` |

详细配置见 [crates/memory-center-server/src/main.rs](../crates/memory-center-server/src/main.rs) 顶部文档。

## 3. 配置 systemd 守护

创建 `/etc/systemd/system/memory-center.service`：

```ini
[Unit]
Description=MemoryCenter Memory Service
After=network.target

[Service]
Type=simple
User=root
WorkingDirectory=/opt/memory-center
Environment=MEMORY_CENTER_HOST=127.0.0.1
Environment=MEMORY_CENTER_PORT=8765
Environment=MEMORY_CENTER_ROOT=/opt/memory-center/data
Environment=MEMORY_CENTER_API_KEY=你的强随机API Key
# 启用 MCP Streamable HTTP 端点（v2.36+）
Environment=MEMORY_CENTER_MCP_ENABLED=true
# MCP session 模式（true: 支持 SSE 流 + session 管理）
Environment=MEMORY_CENTER_MCP_STATEFUL=true
# DNS rebinding 防护：允许的 Host 列表
Environment=MEMORY_CENTER_MCP_ALLOWED_HOSTS=localhost,127.0.0.1,::1,openworld.dpdns.org
# CORS 防护：允许的 Origin 列表（逗号分隔）
Environment=MEMORY_CENTER_MCP_ALLOWED_ORIGINS=https://openworld.dpdns.org
Environment=RUST_LOG=memory_center_server=info,tower_http=info
ExecStart=/opt/memory-center/bin/memory-center-server
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
```

> 仓库提供模板文件 `deploy/memory-center.service.example`，可复制后修改路径和 API Key。

启用并启动：

```bash
systemctl daemon-reload
systemctl enable memory-center
systemctl start memory-center
systemctl status memory-center --no-pager -l
```

**生成强随机 API Key**：

```bash
openssl rand -hex 32
# 输出示例：a3f5e8b2c1d4...（64 个十六进制字符）
```

## 4. 配置 Nginx 反向代理

### 4.1 主站点配置

在主站点 server 块中添加 `location /memory-center/`（**必须在 `location /` 之前**，否则会被 SPA fallback 兜底）：

```nginx
server {
    listen 80;
    server_name openworld.dpdns.org;

    # ... 其他 location ...

    # MemoryCenter 记忆库 API 反代
    location /memory-center/ {
        proxy_pass http://127.0.0.1:8765/;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_read_timeout 60s;
        proxy_send_timeout 60s;
    }

    # MCP Streamable HTTP 端点反代（v2.36+）
    # 公网路径：https://openworld.dpdns.org/memory-center/mcp
    # SSE 流支持：proxy_buffering off + HTTP/1.1 + Connection 清空 + 长超时
    location /memory-center/mcp {
        proxy_pass http://127.0.0.1:8765/mcp;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header Connection "";
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        # SSE 必需：关闭缓冲，实时转发事件流
        proxy_buffering off;
        proxy_cache off;
        # SSE 长连接超时（24h，与 WebSocket 一致）
        proxy_read_timeout 86400s;
        proxy_send_timeout 86400s;
    }

    # SPA fallback（必须在 /memory-center/ 之后）
    location / {
        try_files $uri $uri/ /index.html;
    }
}
```

### 4.2 验证并 reload

```bash
nginx -t          # 测试配置语法
nginx -s reload   # 重新加载
```

### 4.3 路径映射说明

| 公网路径 | 内部转发到 |
|---------|----------|
| `https://openworld.dpdns.org/memory-center/api/v1/sessions/{sid}/archive` | `http://127.0.0.1:8765/api/v1/sessions/{sid}/archive` |
| `https://openworld.dpdns.org/memory-center/api/v1/sessions/{sid}/summaries` | `http://127.0.0.1:8765/api/v1/sessions/{sid}/summaries` |
| `https://openworld.dpdns.org/memory-center/mcp` | `http://127.0.0.1:8765/mcp`（MCP Streamable HTTP） |

> `proxy_pass` 末尾带 `/` 会自动去除 `/memory-center` 前缀。注意：`/memory-center/mcp` 的 `proxy_pass` 不带末尾 `/`，因为需要保留完整路径。

## 5. 验证部署

### 5.1 本地直连测试

```bash
curl -sS -o /dev/null -w "HTTP %{http_code}\n" http://127.0.0.1:8765/api/v1/sessions/probe/summaries
# 期望：HTTP 200
```

### 5.2 Nginx 反代测试

```bash
curl -sS -o /dev/null -w "HTTP %{http_code}\n" \
  -H "Authorization: Bearer 你的API Key" \
  http://127.0.0.1/memory-center/api/v1/sessions/probe/summaries
# 期望：HTTP 200
```

### 5.3 公网访问测试

```bash
curl -sS -o /dev/null -w "HTTP %{http_code}\n" \
  -H "Authorization: Bearer 你的API Key" \
  -k https://openworld.dpdns.org/memory-center/api/v1/sessions/probe/summaries
# 期望：HTTP 200
```

### 5.4 鉴权失败测试

```bash
# 未携带 Authorization 头 → 401
curl -sS -w "HTTP %{http_code}\n" https://openworld.dpdns.org/memory-center/api/v1/sessions/probe/summaries
# 期望：HTTP 401 {"error":{"code":"UNAUTHORIZED","message":"缺少 Authorization 头"}}

# 错误的 API Key → 403
curl -sS -w "HTTP %{http_code}\n" \
  -H "Authorization: Bearer wrong-key" \
  https://openworld.dpdns.org/memory-center/api/v1/sessions/probe/summaries
# 期望：HTTP 403 {"error":{"code":"FORBIDDEN","message":"API Key 不正确"}}
```

### 5.5 端到端功能测试

使用 `deploy/test_e2e.py` 脚本验证归档/检索/摘要/Prompt/反代 5 个端点：

```bash
# 在服务器上执行
python3 /path/to/test_e2e.py

# 或运行完整能力测试
python3 deploy/test_full_capabilities.py
```

> **注意**：测试脚本默认访问 `http://127.0.0.1:8765`（本地直连，绕过鉴权）。
> 若配置了 `MEMORY_CENTER_API_KEY`，需在脚本中加上 `Authorization` 头。

### 5.6 MCP 端点测试（v2.36+）

```bash
# 本地直连测试 MCP 端点（initialize 请求）
curl -sS -o /dev/null -w "HTTP %{http_code}\n" \
  -X POST http://127.0.0.1:8765/mcp \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -d '{"jsonrpc":"2.0","method":"initialize","id":1,"params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"probe","version":"1.0"}}}'
# 期望：HTTP 200

# 公网测试 MCP 端点
curl -sS -o /dev/null -w "HTTP %{http_code}\n" \
  -X POST https://openworld.dpdns.org/memory-center/mcp \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -d '{"jsonrpc":"2.0","method":"initialize","id":1,"params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"probe","version":"1.0"}}}'
# 期望：HTTP 200
```

## 6. 客户端调用示例

### curl

```bash
# 归档对话
curl -X POST https://openworld.dpdns.org/memory-center/api/v1/sessions/my-session/archive \
  -H "Authorization: Bearer 你的API Key" \
  -H "Content-Type: application/json" \
  -d '{
    "turns": [{
      "id": "uuid-1",
      "user_message": {"text": "你好", "attachments": [], "tool_calls": [], "thinking": null},
      "llm_message": {"text": "你好！有什么可以帮您？", "attachments": [], "tool_calls": [], "thinking": null},
      "tags": [],
      "timestamp": "2026-07-05T10:00:00Z",
      "token_count": 50
    }]
  }'

# 检索摘要列表
curl -H "Authorization: Bearer 你的API Key" \
  https://openworld.dpdns.org/memory-center/api/v1/sessions/my-session/summaries
```

### Python

```python
import requests

API_BASE = "https://openworld.dpdns.org/memory-center/api/v1"
API_KEY = "你的API Key"
HEADERS = {"Authorization": f"Bearer {API_KEY}", "Content-Type": "application/json"}

# 归档
resp = requests.post(
    f"{API_BASE}/sessions/my-session/archive",
    headers=HEADERS,
    json={"turns": [...]}
)
print(resp.json())
```

## 7. MCP Streamable HTTP 部署（v2.36+）

MCP Streamable HTTP 让远程客户端（如 DeepSeek 网页端、Web Agent）通过 HTTPS 接入 MemoryCenter，无需本地安装二进制。

### 7.1 启用方式

在 systemd unit 或启动命令中设置环境变量（已在第 3 节 systemd 配置中启用）：

```bash
# 启用 MCP 端点
MEMORY_CENTER_MCP_ENABLED=true

# （可选）session 模式：true 支持 SSE 流 + session 管理，false 无状态
MEMORY_CENTER_MCP_STATEFUL=true

# （可选）DNS rebinding 防护：允许的 Host 列表
MEMORY_CENTER_MCP_ALLOWED_HOSTS=localhost,127.0.0.1,::1,openworld.dpdns.org

# （可选）CORS 防护：允许的 Origin 列表
MEMORY_CENTER_MCP_ALLOWED_ORIGINS=https://openworld.dpdns.org
```

### 7.2 端点说明

| 端点 | 方法 | 说明 |
|------|------|------|
| `/mcp` | POST | MCP 请求（JSON-RPC 2.0） |
| `/mcp` | GET | SSE 流（server → client 推送） |
| `/mcp` | DELETE | 关闭 session |

> `/mcp` 端点不经过 REST API 的 API Key 鉴权，MCP 客户端使用 MCP 协议自身认证。DNS rebinding + CORS 由 `StreamableHttpServerConfig` 内部处理。

### 7.3 配置项详解

| 环境变量 | 说明 | 默认值 |
|---------|------|--------|
| `MEMORY_CENTER_MCP_ENABLED` | 是否启用 MCP Streamable HTTP 端点 | `false`（需显式启用） |
| `MEMORY_CENTER_MCP_STATEFUL` | 是否启用 session 模式 | `true` |
| `MEMORY_CENTER_MCP_ALLOWED_HOSTS` | 允许的 Host 列表（逗号分隔，DNS rebinding 防护） | `localhost,127.0.0.1,::1` |
| `MEMORY_CENTER_MCP_ALLOWED_ORIGINS` | 允许的 Origin 列表（逗号分隔，CORS 防护） | 空（不校验 Origin） |
| `MEMORY_CENTER_PRESET_AGENT` | HTTP 模式下的 Agent 预设（Layer 1 识别） | 空 |

> Agent 识别限制：rmcp `service_factory` 签名不支持传入 ClientInfo，HTTP 模式下 per-session 自动识别（Layer 2）失效。生产环境推荐在 systemd unit 设置 `MEMORY_CENTER_PRESET_AGENT`（如 `trae` / `cursor` / `claude-code`）。

### 7.4 远程客户端配置示例

DeepSeek 网页端、ChatGPT 等支持 MCP 的 Web 客户端配置：

```json
{
  "mcpServers": {
    "memory-center": {
      "url": "https://openworld.dpdns.org/memory-center/mcp",
      "transport": "streamable-http"
    }
  }
}
```

接入后，客户端会自动发现 21 个 MCP tools（archive / retrieve / semantic_search / pre_compress_hook 等）。

### 7.5 与 stdio 模式的对比

| 维度 | stdio 模式 | Streamable HTTP 模式 |
|------|-----------|---------------------|
| 适用场景 | 本地 IDE（Claude Code / Cursor / Trae） | 远程客户端、Web Agent、多客户端共享 |
| 二进制 | `memory-center-mcp` | `memory-center-server`（共享） |
| 启用方式 | 客户端 MCP 配置 `command` | `MEMORY_CENTER_MCP_ENABLED=true` |
| 鉴权 | 无需（进程间通信） | DNS rebinding + CORS 防护 |
| 端点 | 无（stdin/stdout） | `/mcp`（POST / GET / DELETE） |
| 多客户端 | 否（每客户端独立进程） | 是（共享 Axum 服务） |

## 8. Git Auto-Deploy（post-receive hook）

通过 Git bare 仓库 + post-receive hook 实现 `git push production main` 自动编译 + 重启服务。

### 8.1 服务器端配置（一次性）

在服务器上执行 `deploy/setup-auto-deploy.sh` 脚本，自动完成以下操作：

```bash
# 1. 下载脚本到服务器
scp deploy/setup-auto-deploy.sh root@your-server:/root/

# 2. 在服务器上执行
ssh root@your-server
chmod +x /root/setup-auto-deploy.sh
/root/setup-auto-deploy.sh
```

脚本执行内容：
1. 创建裸仓库 `/root/memory-center.git`
2. 创建 post-receive hook（编译 + 替换二进制 + 重启服务）
3. 创建工作目录 `/root/MemoryCenter-work`
4. 验证现有服务状态

### 8.2 post-receive hook 脚本

hook 脚本核心逻辑如下（完整版见 `deploy/post-receive.sh`）：

```bash
#!/bin/bash
# 触发条件：git push production main
# 流程：checkout → cargo build → stop → cp → start → verify
set -e
export PATH=/root/.cargo/bin:/usr/bin:/bin:/usr/local/bin:$PATH

GIT_DIR=/root/memory-center.git
WORK_DIR=/root/MemoryCenter-work
BIN_DIR=/opt/memory-center/bin

while read oldrev newrev ref; do
    if [ "$ref" = "refs/heads/main" ]; then
        echo "[deploy] 开始部署 MemoryCenter Server"
        echo "[deploy] commit: $newrev"

        # 1. checkout 到工作目录
        mkdir -p "$WORK_DIR"
        git --work-tree="$WORK_DIR" --git-dir="$GIT_DIR" checkout -f main
        cd "$WORK_DIR"

        # 2. 编译 release 二进制（约 5-10 分钟）
        echo "[deploy] 编译 memory-center-server..."
        cargo build --release -p memory-center-server

        # 3. 停止服务（二进制运行中无法直接覆盖）
        echo "[deploy] 停止 memory-center 服务..."
        systemctl stop memory-center || true

        # 4. 复制新二进制
        echo "[deploy] 复制二进制..."
        mkdir -p "$BIN_DIR"
        cp target/release/memory-center-server "$BIN_DIR/"

        # 5. 启动服务
        echo "[deploy] 启动 memory-center 服务..."
        systemctl start memory-center

        # 6. 验证（等待 2 秒后检查状态）
        sleep 2
        if systemctl is-active --quiet memory-center; then
            echo "[deploy] 部署成功"
        else
            echo "[deploy] 错误：服务启动失败"
            systemctl status memory-center --no-pager | tail -20
            exit 1
        fi
    fi
done
```

### 8.3 本地配置 remote

```bash
# 在本地仓库添加 production remote
git remote add production root@your-server:/root/memory-center.git

# 验证
git remote -v
# production  root@your-server:/root/memory-center.git (fetch)
# production  root@your-server:/root/memory-center.git (push)
```

### 8.4 标准部署命令

本地与远端历史一致时，一行命令完成部署：

```bash
git add . && git commit -m "feat(xxx): 描述" && git push production main
```

push 后在服务器上观察部署日志：

```bash
# 实时查看 hook 输出（push 时的 SSH 输出会显示部署进度）
# 或在服务器上查看服务状态
systemctl status memory-center --no-pager -l
```

### 8.5 历史分叉时的处理

若 push 报 `Updates were rejected because the remote contains work`：

```bash
# 方案一：rebase（推荐）
git fetch production main
git rebase production/main
git push production main

# 方案二：format-patch 打包上传（rebase 失败时）
git format-patch production/main -o /tmp/patches/
scp -r /tmp/patches root@your-server:/root/patches
ssh root@your-server
cd /root/MemoryCenter-work
git am /root/patches/*.patch
git push origin main --force  # 触发 hook
```

> 禁止使用 `git push --force` + 大文件 bundle/scp，容易导致 SSH 被 reset 且不触发 hook。

## 9. 运维操作

### 查看日志

```bash
# 实时日志
journalctl -u memory-center -f

# 最近 100 行
journalctl -u memory-center -n 100 --no-pager

# 按时间筛选
journalctl -u memory-center --since "2026-07-05 10:00" --until "2026-07-05 12:00"

# 按关键字筛选
journalctl -u memory-center | grep "ERROR"
```

### 重启服务

```bash
systemctl restart memory-center
```

### 更新二进制

```bash
# 1. 拉取最新代码
cd /root/MemoryCenter-work
git pull origin main

# 2. 重新编译
cargo build --release -p memory-center-server

# 3. 替换二进制并重启
cp target/release/memory-center-server /opt/memory-center/bin/
systemctl restart memory-center

# 4. 验证
systemctl status memory-center --no-pager
curl -sS -o /dev/null -w "HTTP %{http_code}\n" \
  -H "Authorization: Bearer $MEMORY_CENTER_API_KEY" \
  http://127.0.0.1:8765/api/v1/sessions/probe/summaries
```

> 推荐使用 Git auto-deploy 一键部署，详见第 8 节。

### 备份数据

```bash
# 压缩存储目录
tar -czf memory-center-backup-$(date +%Y%m%d).tar.gz /opt/memory-center/data/

# 定时备份（crontab）
# 每日凌晨 3 点备份
0 3 * * * tar -czf /backup/memory-center-$(date +\%Y\%m\%d).tar.gz /opt/memory-center/data/

# 保留最近 30 天备份（清理旧备份）
0 4 * * * find /backup/ -name "memory-center-*.tar.gz" -mtime +30 -delete
```

### 清理测试数据

```bash
# 删除指定 session
rm -rf /opt/memory-center/data/sessions/test-session

# 清空所有数据（危险！仅开发环境使用）
rm -rf /opt/memory-center/data/sessions/*
```

## 10. 故障排查

### 服务无法启动

```bash
# 查看详细错误
journalctl -u memory-center -n 50 --no-pager

# 常见原因：
# 1. 端口被占用 → 改 MEMORY_CENTER_PORT 或停止占用进程
# 2. 存储目录无权限 → chown -R root:root /opt/memory-center/data
# 3. 二进制架构不对 → 用 uname -m 确认是 x86_64
```

### Nginx 反代返回 Vue 首页 HTML

**原因**：`location /memory-center/` 被主站点的 `location /` 兜底了。

**解决**：确认 `location /memory-center/` 在 `location /` 之前，且 Nginx 配置文件被正确 include。

```bash
# 查看实际生效的配置
nginx -T 2>/dev/null | grep -A5 "location /memory-center"
```

### MCP 客户端连接失败

```bash
# 1. 确认 MCP 已启用
systemctl show memory-center -p Environment | grep MCP_ENABLED
# 期望：MEMORY_CENTER_MCP_ENABLED=true

# 2. 本地直连测试 MCP 端点
curl -sS -o /dev/null -w "HTTP %{http_code}\n" \
  -X POST http://127.0.0.1:8765/mcp \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -d '{"jsonrpc":"2.0","method":"initialize","id":1,"params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"probe","version":"1.0"}}}'
# 期望：HTTP 200

# 3. 检查 Nginx SSE 配置
nginx -T 2>/dev/null | grep -A10 "location /memory-center/mcp"
# 确认：proxy_buffering off; proxy_cache off; proxy_read_timeout 86400s;

# 4. 检查 DNS rebinding 防护
systemctl show memory-center -p Environment | grep ALLOWED_HOSTS
# 确认客户端访问的域名在允许列表中

# 5. 检查 CORS 防护
systemctl show memory-center -p Environment | grep ALLOWED_ORIGINS
# 确认客户端 Origin 在允许列表中
```

常见 MCP 故障原因：
- `MEMORY_CENTER_MCP_ENABLED` 未设为 `true` → MCP 端点不响应
- Nginx SSE 流被缓冲 → 确认 `proxy_buffering off; proxy_cache off;`
- DNS rebinding 防护拦截 → 将客户端域名加入 `MEMORY_CENTER_MCP_ALLOWED_HOSTS`
- CORS 防护拦截 → 将客户端 Origin 加入 `MEMORY_CENTER_MCP_ALLOWED_ORIGINS`

### PowerShell 上传 Nginx 配置后 `$` 变量丢失

**原因**：PowerShell 反引号转义会吃掉 `$host`、`$remote_addr` 等 Nginx 变量。

**解决**：用本地文件 + `scp` 上传，避免通过 SSH 命令行传递含 `$` 的内容。

### 401/403 鉴权失败

```bash
# 确认 API Key 已配置
systemctl show memory-center -p Environment | grep MEMORY_CENTER_API_KEY

# 确认请求头格式正确
curl -v -H "Authorization: Bearer 你的API Key" http://127.0.0.1:8765/api/v1/sessions/probe/summaries
```

## 11. 安全建议

1. **生产环境必须配置 `MEMORY_CENTER_API_KEY`**：未配置时所有请求无鉴权放行
2. **API Key 使用强随机值**：`openssl rand -hex 32`（64 字符）
3. **不要将 API Key 写入代码或提交到 git**：只通过环境变量或 systemd 配置
4. **定期轮换 API Key**：修改 systemd Environment 后 `systemctl daemon-reload && systemctl restart memory-center`
5. **限制公网暴露范围**：若仅内网使用，可将 Nginx 配置为 `allow 内网网段; deny all;`
6. **启用 HTTPS**：通过 Nginx 终止 SSL，后端保持 HTTP（已在本架构中实现）
7. **MCP 端点访问控制**：配置 `MEMORY_CENTER_MCP_ALLOWED_HOSTS` 和 `MEMORY_CENTER_MCP_ALLOWED_ORIGINS` 防止 DNS rebinding 和 CORS 攻击

## 12. API 端点速查

### REST API 端点

| 方法 | 路径 | 说明 |
|------|------|------|
| POST | `/api/v1/sessions/{sid}/archive` | 归档对话轮次 |
| GET | `/api/v1/sessions/{sid}/memories/{hook_id}` | 检索记忆文件 |
| PATCH | `/api/v1/sessions/{sid}/memories/{hook_id}` | 更新记忆（含冲突检测） |
| GET | `/api/v1/sessions/{sid}/summaries` | 摘要列表 |
| GET | `/api/v1/sessions/{sid}/prompt` | 渲染 system prompt |
| POST | `/api/v1/sessions/{sid}/compaction` | 触发周期任务（weekly/monthly） |
| POST | `/api/v1/sessions/{sid}/search` | 语义检索 |
| POST | `/api/v1/sessions/{sid}/memories/batch-retrieve` | 批量检索 |
| POST | `/api/v1/sessions/{sid}/memories/batch-delete` | 批量删除 |
| POST | `/api/v1/sessions/{sid}/memories/batch-update` | 批量更新 |
| GET | `/api/v1/sessions/{sid}/memories/{hook_id}/conflicts` | 查询冲突记录 |

### MCP Streamable HTTP 端点（v2.36+）

| 方法 | 路径 | 说明 |
|------|------|------|
| POST | `/mcp` | MCP 请求（JSON-RPC 2.0） |
| GET | `/mcp` | SSE 流（server → client 推送） |
| DELETE | `/mcp` | 关闭 session |

所有 REST API 端点均需携带 `Authorization: Bearer <API Key>` 头（配置 `MEMORY_CENTER_API_KEY` 后）。`/mcp` 端点使用 MCP 协议自身认证，不经过 REST API Key 鉴权。
