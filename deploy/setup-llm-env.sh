#!/bin/bash
# 配置生产环境 LLM 环境变量到 systemd 服务
set -e

SERVICE_FILE=/etc/systemd/system/hippocampus-server.service
BACKUP_FILE=/etc/systemd/system/hippocampus-server.service.bak.$(date +%Y%m%d%H%M%S)

echo "=== 1. 备份现有配置 ==="
cp "$SERVICE_FILE" "$BACKUP_FILE"
echo "备份到: $BACKUP_FILE"

echo "=== 2. 添加 LLM 环境变量 ==="
# 在 RUST_LOG 行之后插入 LLM 配置
sed -i '/^Environment=RUST_LOG=/a \
Environment=HIPPOCAMPUS_GENERATOR_API_URL=https://token.sensenova.cn/v1/chat/completions\
Environment=HIPPOCAMPUS_GENERATOR_API_KEY=sk-rWrlIGq55eTzKCrxMUoDjsqIipXdKKLd\
Environment=HIPPOCAMPUS_GENERATOR_MODEL=sensenova-6.7-flash-lite\
Environment=HIPPOCAMPUS_GENERATOR_TIMEOUT=60\
Environment=HIPPOCAMPUS_GENERATOR_MAX_TOKENS=500\
Environment=HIPPOCAMPUS_DETECTOR_API_URL=https://api.deepseek.com/chat/completions\
Environment=HIPPOCAMPUS_DETECTOR_API_KEY=sk-8789c68cf4164736842eb21883f2abf9\
Environment=HIPPOCAMPUS_DETECTOR_MODEL=deepseek-v4-flash\
Environment=HIPPOCAMPUS_DETECTOR_TIMEOUT=30\
Environment=HIPPOCAMPUS_DETECTOR_MAX_TOKENS=500' "$SERVICE_FILE"

echo "=== 3. 验证配置 ==="
grep -E 'Environment=' "$SERVICE_FILE"

echo "=== 4. 重新加载并重启 ==="
systemctl daemon-reload
systemctl restart hippocampus-server
sleep 2

echo "=== 5. 验证服务状态 ==="
if systemctl is-active --quiet hippocampus-server; then
    echo "服务启动成功"
    systemctl status hippocampus-server --no-pager | head -10
else
    echo "错误：服务启动失败"
    systemctl status hippocampus-server --no-pager | tail -20
    exit 1
fi

echo ""
echo "=== 配置完成 ==="
echo "LLM Generator: SenseNova (sensenova-6.7-flash-lite)"
echo "LLM Detector:  DeepSeek (deepseek-v4-flash)"
