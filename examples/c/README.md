# Hippocampus C 示例与集成测试

本目录包含：

| 文件 | 说明 |
|------|------|
| `demo.c` | C 调用示例（演示 5 个核心操作的用法） |
| `test.c` | C 集成测试（带 assert 断言，覆盖 8 个场景） |
| `Makefile` | Linux/macOS 构建脚本 |
| `build.bat` | Windows MSVC 构建脚本 |

## 前置准备

```bash
# 在项目根目录构建动态库
cargo build --release -p hippocampus-ffi
```

构建产物位置：
- Linux: `target/release/libhippocampus.so`
- macOS: `target/release/libhippocampus.dylib`
- Windows: `target/release/hippocampus.dll`

## 运行示例（demo.c）

### Linux/macOS

```bash
cd examples/c
gcc demo.c -o demo \
  -I ../../crates/hippocampus-ffi/include \
  -L ../../target/release \
  -lhippocampus -lpthread -ldl
LD_LIBRARY_PATH=../../target/release ./demo
```

### Windows MSVC

需在「x64 Native Tools Command Prompt」中运行：

```powershell
cd examples\c
cl demo.c /I ..\..\crates\hippocampus-ffi\include /link ..\..\target\release\hippocampus.dll.lib
set PATH=..\..\target\release;%PATH%
demo.exe
```

## 运行集成测试（test.c）

### 一键构建运行

```bash
# Linux/macOS
cd examples/c
make all

# 或分步：
make build   # 构建 Rust 库 + 编译 C 测试
make test    # 运行 C 测试
```

### 测试场景

`test.c` 覆盖 8 个场景：

1. **句柄生命周期** — 创建/释放/NULL 幂等
2. **archive + retrieve 全链路** — 归档后按 hook_id 检索完整记忆
3. **get_summaries** — 空状态返回 `[]`，归档后有摘要
4. **render_prompt** — 空状态返回空串，归档后含 `# 可用记忆索引`
5. **错误处理** — 无效 JSON / 无效 hook_id / NULL handle / NULL 参数
6. **run_compaction 无效 period** — period=99 应失败
7. **run_compaction weekly 无 daily** — 未归档直接 weekly_merge 应失败
8. **完整工作流** — archive → summaries → prompt → retrieve（带 project_id）

### 预期输出

```
================ Hippocampus C 集成测试 ================
[test] 句柄生命周期...
  PASS
[test] archive + retrieve 全链路...
  hook_id: ...
  PASS
...
=========================================================
所有测试通过
```

## CI 集成

C 集成测试已集成到 GitHub Actions（见 `.github/workflows/ci.yml` 的 `c-integration-test` job），在 Linux x86_64 上自动运行。

## 注意事项

- 句柄不保证线程安全，多线程访问需自行加锁
- `hippocampus_get_data` / `hippocampus_get_error` 返回的字符串必须用 `hippocampus_free_string` 释放
- `HippocampusResult*` 必须用 `hippocampus_result_free` 释放
- 本测试用 `strstr` 粗略提取 JSON 字段，生产场景建议用 jsmn / cJSON 等 JSON 解析库
- 测试会在当前目录创建 `tmp_test_*` 临时目录，可用 `make clean` 清理
