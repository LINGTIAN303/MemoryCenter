# Hippocampus C 集成测试构建脚本（Linux/macOS）
#
# 用法：
#   make build   # 构建 Rust 动态库 + 编译 C 测试
#   make test    # 运行 C 测试
#   make all     # build + test
#   make clean   # 清理

PROJECT_ROOT := ../..
FFI_INCLUDE := $(PROJECT_ROOT)/crates/hippocampus-ffi/include
LIB_DIR := $(PROJECT_ROOT)/target/release

# 平台相关
UNAME_S := $(shell uname -s)
ifeq ($(UNAME_S),Linux)
    LIB_NAME := libhippocampus.so
    LDFLAGS := -lpthread -ldl
    LD_PATH := LD_LIBRARY_PATH=$(LIB_DIR)
else ifeq ($(UNAME_S),Darwin)
    LIB_NAME := libhippocampus.dylib
    LDFLAGS :=
    LD_PATH := DYLD_LIBRARY_PATH=$(LIB_DIR)
endif

CC ?= cc

.PHONY: build rust-lib c-test test all clean

all: build test

build: rust-lib c-test

rust-lib:
	cd $(PROJECT_ROOT) && cargo build --release -p hippocampus-ffi

c-test:
	$(CC) test.c -o test \
		-I $(FFI_INCLUDE) \
		-L $(LIB_DIR) \
		-lhippocampus $(LDFLAGS)

test: c-test
	$(LD_PATH) ./test

clean:
	rm -f test
	rm -rf tmp_test_*
