# Hippocampus HTTP 服务部署指南

本文档记录将 Hippocampus HTTP 服务部署到 Linux 生产服务器的完整流程。
已在 `162.211.183.236`（openworld.dpdns.org）上验证通过。

## 架构概览

```
公网请求 https://openworld.dpdns.org/hippo/api/v1/...
    ↓
Nginx 80 端口（location /hippo/ 反代）
    ↓ proxy_pass http://127.0.0.1:8765/
systemd 守护进程 hippocampus-server.service
    ↓
/opt/hippocampus-server/bin/hippocampus-server（Rust 单二进制，9.2MB）
    ↓
/opt/hippocampus-server/data/（LocalStorage 文件树）
```

## 前置条件

- Linux 服务器（x86_64），已安装 Nginx
- Rust 工具链（用于编译，也可交叉编译后上传二进制）
- 已开放 80/443 端口出方向

## 1. 编译二进制

### 方案 A：服务器直接编译（推荐，避免交叉编译问题）

```bash
# 1. 拉取代码
git clone https://github.com/LINGTIAN303/Hippocampus.git /opt/hippocampus-src
cd /opt/hippocampus-src

# 2. 编译 release 二进制（约 5-10 分钟）
cargo build --release -p hippocampus-server

# 3. 部署二进制
mkdir -p /opt/hippocampus-server/bin /opt/hippocampus-server/data
cp target/release/hippocampus-server /opt/hippocampus-server/bin/
```

### 方案 B：本地交叉编译后上传

```powershell
# Windows 上交叉编译 Linux x86_64（需要 x86_64-unknown-linux-gnu 工具链）
rustup target add x86_64-unknown-linux-gnu
cargo build --release --target x86_64-unknown-linux-gnu -p hippocampus-server

# 上传
scp target/x86_64-unknown-linux-gnu/release/hippocampus-server root@SERVER:/opt/hippocampus-server/bin/
```

> **注意**：交叉编译需要 `x86_64-linux-gnu-gcc`，Windows 上配置较复杂，推荐方案 A。

## 2. 配置环境变量

编辑 systemd 服务文件（见下一步），或在 `/etc/profile.d/hippocampus.sh` 中配置：

| 环境变量 | 说明 | 默认值 | 必填 |
|---------|------|--------|------|
| `HIPPOCAMPUS_HOST` | 监听地址 | `127.0.0.1` | 否（公网通过 Nginx 暴露） |
| `HIPPOCAMPUS_PORT` | 监听端口 | `8765` | 否 |
| `HIPPOCAMPUS_ROOT` | 存储根目录 | `./data` | 是 |
| `HIPPOCAMPUS_API_KEY` | API Key 鉴权（v2.24） | 空（不鉴权） | **生产强烈建议配置** |
| `RUST_LOG` | 日志级别 | `hippocampus_server=info,tower_http=info` | 否 |

### 可选增强组件

| 环境变量前缀 | 功能 | 未配置时行为 |
|------------|------|------------|
| `HIPPOCAMPUS_EMBEDDER_*` | 语义检索（向量+BM25 混合） | 降级为仅关键词检索 |
| `HIPPOCAMPUS_DETECTOR_*` | 冲突检测（启发式+LLM 混合） | 降级为启发式纯算法 |
| `HIPPOCAMPUS_GENERATOR_*` | LLM 摘要生成 | 降级为启发式 `Summary::from_title` |

详细配置见 [crates/hippocampus-server/src/main.rs](../crates/hippocampus-server/src/main.rs) 顶部文档。

## 3. 配置 systemd 守护

创建 `/etc/systemd/system/hippocampus-server.service`：

```ini
[Unit]
Description=Hippocampus HTTP Server
After=network.target

[Service]
Type=simple
User=root
WorkingDirectory=/opt/hippocampus-server
Environment=HIPPOCAMPUS_HOST=127.0.0.1
Environment=HIPPOCAMPUS_PORT=8765
Environment=HIPPOCAMPUS_ROOT=/opt/hippocampus-server/data
Environment=HIPPOCAMPUS_API_KEY=你的强随机API Key
Environment=RUST_LOG=hippocampus_server=info,tower_http=info
ExecStart=/opt/hippocampus-server/bin/hippocampus-server
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
```

启用并启动：

```bash
systemctl daemon-reload
systemctl enable hippocampus-server
systemctl start hippocampus-server
systemctl status hippocampus-server --no-pager -l
```

**生成强随机 API Key**：

```bash
openssl rand -hex 32
# 输出示例：a3f5e8b2c1d4...（64 个十六进制字符）
```

## 4. 配置 Nginx 反向代理

### 4.1 主站点配置

在主站点 server 块中添加 `location /hippo/`（**必须在 `location /` 之前**，否则会被 SPA fallback 兜底）：

```nginx
server {
    listen 80;
    server_name openworld.dpdns.org;

    # ... 其他 location ...

    # Hippocampus 记忆库 API 反代
    location /hippo/ {
        proxy_pass http://127.0.0.1:8765/;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_read_timeout 60s;
        proxy_send_timeout 60s;
    }

    # SPA fallback（必须在 /hippo/ 之后）
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
| `https://openworld.dpdns.org/hippo/api/v1/sessions/{sid}/archive` | `http://127.0.0.1:8765/api/v1/sessions/{sid}/archive` |
| `https://openworld.dpdns.org/hippo/api/v1/sessions/{sid}/summaries` | `http://127.0.0.1:8765/api/v1/sessions/{sid}/summaries` |

> `proxy_pass` 末尾带 `/` 会自动去除 `/hippo` 前缀。

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
  http://127.0.0.1/hippo/api/v1/sessions/probe/summaries
# 期望：HTTP 200
```

### 5.3 公网访问测试

```bash
curl -sS -o /dev/null -w "HTTP %{http_code}\n" \
  -H "Authorization: Bearer 你的API Key" \
  -k https://openworld.dpdns.org/hippo/api/v1/sessions/probe/summaries
# 期望：HTTP 200
```

### 5.4 鉴权失败测试

```bash
# 未携带 Authorization 头 → 401
curl -sS -w "HTTP %{http_code}\n" https://openworld.dpdns.org/hippo/api/v1/sessions/probe/summaries
# 期望：HTTP 401 {"error":{"code":"UNAUTHORIZED","message":"缺少 Authorization 头"}}

# 错误的 API Key → 403
curl -sS -w "HTTP %{http_code}\n" \
  -H "Authorization: Bearer wrong-key" \
  https://openworld.dpdns.org/hippo/api/v1/sessions/probe/summaries
# 期望：HTTP 403 {"error":{"code":"FORBIDDEN","message":"API Key 不正确"}}
```

### 5.5 端到端功能测试

使用 `deploy/test_e2e.py` 脚本验证归档/检索/摘要/Prompt/反代 5 个端点：

```bash
# 在服务器上执行
python3 /path/to/test_e2e.py
```

> **注意**：测试脚本默认访问 `http://127.0.0.1:8765`（本地直连，绕过鉴权）。
> 若配置了 `HIPPOCAMPUS_API_KEY`，需在脚本中加上 `Authorization` 头。

## 6. 客户端调用示例

### curl

```bash
# 归档对话
curl -X POST https://openworld.dpdns.org/hippo/api/v1/sessions/my-session/archive \
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
  https://openworld.dpdns.org/hippo/api/v1/sessions/my-session/summaries
```

### Python

```python
import requests

API_BASE = "https://openworld.dpdns.org/hippo/api/v1"
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

## 7. 运维操作

### 查看日志

```bash
# 实时日志
journalctl -u hippocampus-server -f

# 最近 100 行
journalctl -u hippocampus-server -n 100 --no-pager

# 按时间筛选
journalctl -u hippocampus-server --since "2026-07-05 10:00" --until "2026-07-05 12:00"
```

### 重启服务

```bash
systemctl restart hippocampus-server
```

### 更新二进制

```bash
# 1. 拉取最新代码
cd /opt/hippocampus-src
git pull origin main

# 2. 重新编译
cargo build --release -p hippocampus-server

# 3. 替换二进制并重启
cp target/release/hippocampus-server /opt/hippocampus-server/bin/
systemctl restart hippocampus-server

# 4. 验证
systemctl status hippocampus-server --no-pager
curl -sS -o /dev/null -w "HTTP %{http_code}\n" \
  -H "Authorization: Bearer $HIPPOCAMPUS_API_KEY" \
  http://127.0.0.1:8765/api/v1/sessions/probe/summaries
```

### 备份数据

```bash
# 压缩存储目录
tar -czf hippocampus-backup-$(date +%Y%m%d).tar.gz /opt/hippocampus-server/data/

# 定时备份（crontab）
# 每日凌晨 3 点备份
0 3 * * * tar -czf /backup/hippocampus-$(date +\%Y\%m\%d).tar.gz /opt/hippocampus-server/data/
```

### 清理测试数据

```bash
# 删除指定 session
rm -rf /opt/hippocampus-server/data/sessions/test-session

# 清空所有数据（危险！仅开发环境使用）
rm -rf /opt/hippocampus-server/data/sessions/*
```

## 8. 故障排查

### 服务无法启动

```bash
# 查看详细错误
journalctl -u hippocampus-server -n 50 --no-pager

# 常见原因：
# 1. 端口被占用 → 改 HIPPOCAMPUS_PORT 或停止占用进程
# 2. 存储目录无权限 → chown -R root:root /opt/hippocampus-server/data
# 3. 二进制架构不对 → 用 uname -m 确认是 x86_64
```

### Nginx 反代返回 Vue 首页 HTML

**原因**：`location /hippo/` 被主站点的 `location /` 兜底了。

**解决**：确认 `location /hippo/` 在 `location /` 之前，且 Nginx 配置文件被正确 include。

```bash
# 查看实际生效的配置
nginx -T 2>/dev/null | grep -A5 "location /hippo"
```

### PowerShell 上传 Nginx 配置后 `$` 变量丢失

**原因**：PowerShell 反引号转义会吃掉 `$host`、`$remote_addr` 等 Nginx 变量。

**解决**：用本地文件 + `scp` 上传，避免通过 SSH 命令行传递含 `$` 的内容。

### 401/403 鉴权失败

```bash
# 确认 API Key 已配置
systemctl show hippocampus-server -p Environment | grep HIPPOCAMPUS_API_KEY

# 确认请求头格式正确
curl -v -H "Authorization: Bearer 你的API Key" http://127.0.0.1:8765/api/v1/sessions/probe/summaries
```

## 9. 安全建议

1. **生产环境必须配置 `HIPPOCAMPUS_API_KEY`**：未配置时所有请求无鉴权放行
2. **API Key 使用强随机值**：`openssl rand -hex 32`（64 字符）
3. **不要将 API Key 写入代码或提交到 git**：只通过环境变量或 systemd 配置
4. **定期轮换 API Key**：修改 systemd Environment 后 `systemctl daemon-reload && systemctl restart hippocampus-server`
5. **限制公网暴露范围**：若仅内网使用，可将 Nginx 配置为 `allow 内网网段; deny all;`
6. **启用 HTTPS**：通过 Nginx 终止 SSL，后端保持 HTTP（已在本架构中实现）

## 10. API 端点速查

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

所有端点均需携带 `Authorization: Bearer <API Key>` 头（配置 `HIPPOCAMPUS_API_KEY` 后）。
