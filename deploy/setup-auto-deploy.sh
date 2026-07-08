#!/bin/bash
# MemoryCenter 自动部署配置脚本（服务器端一次性执行）
# 创建裸仓库 + post-receive hook，实现 git push production main 自动部署
set -e

export PATH=/root/.cargo/bin:/usr/bin:/bin:/usr/local/bin:$PATH

GIT_DIR=/root/memory-center.git
WORK_DIR=/root/memory-center-work
BIN_DIR=/opt/memory-center/bin

echo "=== 1. 创建裸仓库 ==="
if [ -d "$GIT_DIR" ]; then
    echo "裸仓库已存在，跳过"
else
    mkdir -p "$GIT_DIR"
    cd "$GIT_DIR"
    git init --bare
    echo "裸仓库创建完成: $GIT_DIR"
fi

echo "=== 2. 创建 post-receive hook ==="
cat > "$GIT_DIR/hooks/post-receive" << 'HOOK'
#!/bin/bash
set -e
export PATH=/root/.cargo/bin:/usr/bin:/bin:/usr/local/bin:$PATH

GIT_DIR=/root/memory-center.git
WORK_DIR=/root/memory-center-work
BIN_DIR=/opt/memory-center/bin

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

        # 复制二进制
        echo "[deploy] 复制二进制到 $BIN_DIR..."
        mkdir -p "$BIN_DIR"
        cp target/release/memory-center-server "$BIN_DIR/"

        # 重启服务
        echo "[deploy] 重启 memory-center 服务..."
        systemctl restart memory-center

        # 验证
        sleep 2
        if systemctl is-active --quiet memory-center; then
            echo "[deploy] 服务启动成功"
            echo "[deploy] ============================================"
            echo "[deploy] 部署完成"
            echo "[deploy] ============================================"
        else
            echo "[deploy] 错误：服务启动失败"
            systemctl status memory-center --no-pager | tail -20
            exit 1
        fi
    fi
done
HOOK
chmod +x "$GIT_DIR/hooks/post-receive"
echo "post-receive hook 创建完成"

echo "=== 3. 创建工作目录 ==="
mkdir -p "$WORK_DIR"
echo "工作目录: $WORK_DIR"

echo "=== 4. 验证现有服务状态 ==="
if systemctl is-active --quiet memory-center; then
    echo "memory-center 服务运行中"
else
    echo "警告：memory-center 服务未运行"
fi

echo ""
echo "=== 配置完成 ==="
echo "服务器端配置已就绪"
echo "本地执行以下命令添加 production remote："
echo "  git remote add production __REDACTED_SERVER__:$GIT_DIR"
echo "  git push production main"
