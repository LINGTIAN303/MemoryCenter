#!/bin/bash
set -e
source "$HOME/.cargo/env"

# 配置 cargo 国内镜像
mkdir -p "$HOME/.cargo"
cat > "$HOME/.cargo/config.toml" << 'CARGOCONF'
[source.crates-io]
replace-with = 'rsproxy-sparse'
[source.rsproxy]
registry = "https://rsproxy.cn/crates.io-index"
[source.rsproxy-sparse]
registry = "sparse+https://rsproxy.cn/index/"
[registries.rsproxy]
index = "https://rsproxy.cn/crates.io-index"
[net]
git-fetch-with-cli = true
CARGOCONF

echo "=== cargo mirror configured ==="

# 复制代码到 Linux 文件系统（避免 /mnt/d 慢速 I/O）
rm -rf "$HOME/mc-build"
mkdir -p "$HOME/mc-build"
cp -r /mnt/d/本地化AI/MemoryCenter/crates "$HOME/mc-build/"
cp /mnt/d/本地化AI/MemoryCenter/Cargo.toml /mnt/d/本地化AI/MemoryCenter/Cargo.lock "$HOME/mc-build/"

echo "=== code copied to ~/mc-build ==="

# 编译 release 版本（只编译 memory-center-server）
cd "$HOME/mc-build"
cargo build --release -p memory-center-server 2>&1

echo "=== BUILD_EXIT_CODE=$? ==="

# 复制二进制到 Windows 可访问路径
cp target/release/memory-center-server /mnt/d/本地化AI/MemoryCenter/memory-center-server-linux
echo "=== binary copied to /mnt/d/本地化AI/MemoryCenter/memory-center-server-linux ==="
ls -lh /mnt/d/本地化AI/MemoryCenter/memory-center-server-linux
