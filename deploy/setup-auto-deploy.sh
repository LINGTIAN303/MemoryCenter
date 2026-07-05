#!/bin/bash
# Hippocampus 自动部署配置脚本（服务器端一次性执行）
# 创建裸仓库 + post-receive hook，实现 git push production main 自动部署
set -e

export PATH=/root/.cargo/bin:/usr/bin:/bin:/usr/local/bin:$PATH

GIT_DIR=/root/hippocampus.git
WORK_DIR=/root/hippocampus-work
BIN_DIR=/opt/hippocampus-server/bin

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

GIT_DIR=/root/hippocampus.git
WORK_DIR=/root/hippocampus-work
BIN_DIR=/opt/hippocampus-server/bin

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

        # 复制二进制
        echo "[deploy] 复制二进制到 $BIN_DIR..."
        mkdir -p "$BIN_DIR"
        cp target/release/hippocampus-server "$BIN_DIR/"

        # 重启服务
        echo "[deploy] 重启 hippocampus-server 服务..."
        systemctl restart hippocampus-server

        # 验证
        sleep 2
        if systemctl is-active --quiet hippocampus-server; then
            echo "[deploy] 服务启动成功"
            echo "[deploy] ============================================"
            echo "[deploy] 部署完成"
            echo "[deploy] ============================================"
        else
            echo "[deploy] 错误：服务启动失败"
            systemctl status hippocampus-server --no-pager | tail -20
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
if systemctl is-active --quiet hippocampus-server; then
    echo "hippocampus-server 服务运行中"
else
    echo "警告：hippocampus-server 服务未运行"
fi

echo ""
echo "=== 配置完成 ==="
echo "服务器端配置已就绪"
echo "本地执行以下命令添加 production remote："
echo "  git remote add production __REDACTED_SERVER__:$GIT_DIR"
echo "  git push production main"
