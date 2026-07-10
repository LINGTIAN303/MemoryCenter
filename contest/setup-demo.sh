#!/bin/bash
# MemoryCenter Demo 独立部署脚本（参赛专用）
# 端口 8766，数据目录 /opt/memory-center-demo/data/
# 不污染现有 memory-center 服务数据
set -e

echo "=== [1/6] 创建独立目录 ==="
mkdir -p /opt/memory-center-demo/bin
mkdir -p /opt/memory-center-demo/data
cp /opt/memory-center/bin/memory-center-server /opt/memory-center-demo/bin/
chmod +x /opt/memory-center-demo/bin/memory-center-server
echo "二进制已复制"

echo "=== [2/6] 写入 systemd unit ==="
cat > /etc/systemd/system/memory-center-demo.service << 'UNIT_EOF'
[Unit]
Description=MemoryCenter Demo Service (TRAE Contest)
After=network.target

[Service]
Type=simple
User=root
WorkingDirectory=/opt/memory-center-demo
Environment=MEMORY_CENTER_HOST=127.0.0.1
Environment=MEMORY_CENTER_PORT=8766
Environment=MEMORY_CENTER_ROOT=/opt/memory-center-demo/data
Environment=MEMORY_CENTER_API_KEY=trae-contest-demo-key-2026
Environment=MEMORY_CENTER_MCP_ENABLED=true
Environment=MEMORY_CENTER_MCP_STATEFUL=true
Environment=MEMORY_CENTER_MCP_ALLOWED_HOSTS=localhost,127.0.0.1,162.211.183.236
Environment=RUST_LOG=memory_center_server=info,tower_http=info

# LLM 摘要生成器配置（SiliconFlow + Qwen2.5-7B）
Environment=MEMORY_CENTER_GENERATOR_API_URL=https://api.siliconflow.cn/v1/chat/completions
Environment=MEMORY_CENTER_GENERATOR_API_KEY=__REDACTED_LLM_API_KEY__
Environment=MEMORY_CENTER_GENERATOR_MODEL=Qwen/Qwen2.5-7B-Instruct
Environment=MEMORY_CENTER_GENERATOR_TIMEOUT=60
Environment=MEMORY_CENTER_GENERATOR_MAX_TOKENS=500

# LLM 冲突检测器配置
Environment=MEMORY_CENTER_DETECTOR_API_URL=https://api.siliconflow.cn/v1/chat/completions
Environment=MEMORY_CENTER_DETECTOR_API_KEY=__REDACTED_LLM_API_KEY__
Environment=MEMORY_CENTER_DETECTOR_MODEL=Qwen/Qwen2.5-7B-Instruct
Environment=MEMORY_CENTER_DETECTOR_TIMEOUT=30
Environment=MEMORY_CENTER_DETECTOR_MAX_TOKENS=500

# Embedding 语义检索配置（BAAI/bge-m3, 1024 维）
Environment=MEMORY_CENTER_EMBEDDER_API_URL=https://api.siliconflow.cn/v1/embeddings
Environment=MEMORY_CENTER_EMBEDDER_API_KEY=__REDACTED_LLM_API_KEY__
Environment=MEMORY_CENTER_EMBEDDER_MODEL=BAAI/bge-m3
Environment=MEMORY_CENTER_EMBEDDER_DIM=1024
Environment=MEMORY_CENTER_EMBEDDER_TIMEOUT=30

ExecStart=/opt/memory-center-demo/bin/memory-center-server
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
UNIT_EOF
echo "systemd unit 已写入"

echo "=== [3/6] 写入 nginx 配置 ==="
cat > /etc/nginx/sites-enabled/memory-center-demo << 'NGINX_EOF'
server {
    listen 8088;
    server_name _;

    # REST API 反代
    location / {
        proxy_pass http://127.0.0.1:8766;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_read_timeout 60s;
        proxy_send_timeout 60s;
    }

    # MCP Streamable HTTP 端点（SSE 流支持）
    location /mcp {
        proxy_pass http://127.0.0.1:8766;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header Connection "";
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_buffering off;
        proxy_cache off;
        proxy_read_timeout 86400s;
        proxy_send_timeout 86400s;
    }

    # 健康检查
    location /healthz {
        proxy_pass http://127.0.0.1:8766/healthz;
        proxy_set_header Host $host;
    }
}
NGINX_EOF
echo "nginx 配置已写入"

echo "=== [4/6] 启动服务 ==="
systemctl daemon-reload
systemctl enable memory-center-demo
systemctl start memory-center-demo
sleep 2
echo "服务状态："
systemctl status memory-center-demo --no-pager | head -8

echo "=== [5/6] 重载 nginx ==="
nginx -t && systemctl reload nginx
echo "nginx 已重载"

echo "=== [6/6] 验证 ==="
echo "--- 健康检查 ---"
curl -s http://127.0.0.1:8766/healthz || echo "FAIL: 8766 健康检查失败"
echo ""
echo "--- 外部访问测试 ---"
curl -s -o /dev/null -w "HTTP %{http_code}" http://162.211.183.236:8088/healthz || echo "FAIL: 8088 外部访问失败"
echo ""
echo "--- MCP 端点测试 ---"
curl -s -o /dev/null -w "HTTP %{http_code}" http://127.0.0.1:8766/mcp || echo "MCP 端点未响应"
echo ""
echo "=== 部署完成 ==="
echo "Demo 访问地址："
echo "  REST API:  http://162.211.183.236:8088/"
echo "  MCP 端点:  http://162.211.183.236:8088/mcp"
echo "  健康检查:  http://162.211.183.236:8088/healthz"
