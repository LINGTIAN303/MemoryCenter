# ============================================================================
# Hippocampus Go 绑定构建与测试脚本（Linux / macOS）
#
# 流程：
#   1. cargo build --release -p hippocampus-ffi（生成动态库）
#   2. 复制 libhippocampus.so / libhippocampus.dylib 到包目录（cgo 链接 + 运行时加载）
#   3. 设置 LD_LIBRARY_PATH / DYLD_LIBRARY_PATH（让动态链接器找到 so/dylib）
#   4. go test -v ./...
#
# 使用方式：
#   cd crates/hippocampus-go
#   make test

.PHONY: build test clean

LIB_DIR := ../../target/release

# 检测平台对应的动态库扩展名
UNAME_S := $(shell uname -s)
ifeq ($(UNAME_S),Linux)
    LIB_EXT := .so
    LIB_NAME := libhippocampus.so
    LD_PATH_VAR := LD_LIBRARY_PATH
else ifeq ($(UNAME_S),Darwin)
    LIB_EXT := .dylib
    LIB_NAME := libhippocampus.dylib
    LD_PATH_VAR := DYLD_LIBRARY_PATH
else
    $(error 不支持的平台: $(UNAME_S)，请使用 build.bat（Windows）)
endif

build:
	@echo "[1/3] 构建 hippocampus-ffi 动态库..."
	@cargo build --release -p hippocampus-ffi

test: build
	@echo ""
	@echo "[2/3] 复制 $(LIB_NAME) 到包目录..."
	@cp $(LIB_DIR)/$(LIB_NAME) .
	@echo "    已复制 $(LIB_NAME) -> 包目录"
	@echo ""
	@echo "[3/3] 运行 Go 测试..."
	@export $(LD_PATH_VAR)=$(PWD):$$$(LD_PATH_VAR); \
	 go test -v ./...

clean:
	@echo "清理生成文件..."
	@rm -f hippocampus.lib hippocampus.dll libhippocampus.so libhippocampus.dylib
	@cargo clean -p hippocampus-ffi 2>/dev/null || true
