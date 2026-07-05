#!/bin/bash
# Hippocampus 自动部署 post-receive hook
# 触发条件：git push production main
# 流程：checkout → cargo build → stop → cp → start → verify
set -e
export PATH=/root/.cargo/bin:/usr/bin:/bin:/usr/local/bin:$PATH

GIT_DIR=/root/hippocampus.git
WORK_DIR=/root/hippocampus-work
BIN_DIR=/opt/hippocampus-server/bin
BIN_FILE=$BIN_DIR/hippocampus-server

while read oldrev newrev ref; do
    if [ "$ref" = "refs/heads/main" ]; then
        echo "[deploy] ============================================"
        echo "[deploy] 开始部署 Hippocampus Server"
        echo "[deploy] commit: $newrev"
        echo "[deploy] 时间: $(date '+%Y-%m-%d %H:%M:%S')"
        echo "[deploy] ============================================"

        # checkout 到工作目录
        mkdir -p "$WORK_DIR"
        git --work-tree="$WORK_DIR" --git-dir="$GIT_DIR" checkout -f main
        cd "$WORK_DIR"

        # 编译
        echo "[deploy] 编译 hippocampus-server..."
        cargo build --release -p hippocampus-server

        # 停止服务（二进制运行中无法直接覆盖）
        echo "[deploy] 停止 hippocampus-server 服务..."
        systemctl stop hippocampus-server || true

        # 复制二进制
        echo "[deploy] 复制二进制到 $BIN_FILE..."
        mkdir -p "$BIN_DIR"
        cp target/release/hippocampus-server "$BIN_FILE"

        # 启动服务
        echo "[deploy] 启动 hippocampus-server 服务..."
        systemctl start hippocampus-server

        # 验证
        sleep 2
        if systemctl is-active --quiet hippocampus-server; then
            echo "[deploy] ============================================"
            echo "[deploy] 部署成功"
            echo "[deploy] ============================================"
        else
            echo "[deploy] 错误：服务启动失败"
            systemctl status hippocampus-server --no-pager | tail -20
            exit 1
        fi
    fi
done
