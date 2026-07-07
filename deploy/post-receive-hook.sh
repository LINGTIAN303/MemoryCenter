#!/bin/bash
# MemoryCenter 自动部署 post-receive hook
# 触发条件：git push production main
# 流程：checkout → cargo build → stop → cp → start → verify
set -e
export PATH=/root/.cargo/bin:/usr/bin:/bin:/usr/local/bin:$PATH

GIT_DIR=/root/memory-center.git
WORK_DIR=/root/memory-center-work
BIN_DIR=/opt/memory-center/bin
BIN_FILE=$BIN_DIR/memory-center-server

while read oldrev newrev ref; do
    if [ "$ref" = "refs/heads/main" ]; then
        echo "[deploy] ============================================"
        echo "[deploy] 开始部署 MemoryCenter Server"
        echo "[deploy] commit: $newrev"
        echo "[deploy] 时间: $(date '+%Y-%m-%d %H:%M:%S')"
        echo "[deploy] ============================================"

        # checkout 到工作目录
        mkdir -p "$WORK_DIR"
        git --work-tree="$WORK_DIR" --git-dir="$GIT_DIR" checkout -f main
        cd "$WORK_DIR"

        # 编译
        echo "[deploy] 编译 memory-center-server..."
        cargo build --release -p memory-center-server

        # 停止服务（二进制运行中无法直接覆盖）
        echo "[deploy] 停止 memory-center 服务..."
        systemctl stop memory-center || true

        # 复制二进制
        echo "[deploy] 复制二进制到 $BIN_FILE..."
        mkdir -p "$BIN_DIR"
        cp target/release/memory-center-server "$BIN_FILE"

        # 启动服务
        echo "[deploy] 启动 memory-center 服务..."
        systemctl start memory-center

        # 验证
        sleep 2
        if systemctl is-active --quiet memory-center; then
            echo "[deploy] ============================================"
            echo "[deploy] 部署成功"
            echo "[deploy] ============================================"
        else
            echo "[deploy] 错误：服务启动失败"
            systemctl status memory-center --no-pager | tail -20
            exit 1
        fi
    fi
done
