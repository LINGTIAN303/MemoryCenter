# Hippocampus Python 示例

基于 ctypes 通过 C ABI 调用 Hippocampus，覆盖 5 个核心操作。

## 前置准备

```bash
# 在项目根目录构建动态库
cargo build --release -p hippocampus-ffi
```

## 运行

```bash
cd examples/python
python3 demo.py
```

## 预期输出

```
======================================================================
Hippocampus Python 示例 - 通过 ctypes 调用 C ABI
======================================================================

[1] 句柄创建成功

[2] 归档成功
    hook_id:         ...
    memory_file_id:  ...
    summary_title:   你好，介绍一下记忆库设计
    tags:            ['文本消息', '代码块']
    token_count:     140

[3] 所有摘要视图（共 1 条）
    - 你好，介绍一下记忆库设计 [文本消息, 代码块] (140 tokens)

[4] 渲染的 system prompt（xxx 字符）:
----------------------------------------------------------------------
# 可用记忆索引
...
----------------------------------------------------------------------

[5] 检索完整记忆文件（hook_id=...）
    memory_file_id:  ...
    turns 数量:       2
    total_tokens:    140
    truncated:       False

======================================================================
演示完成
记忆文件已保存到：.../examples/python/mem_data
======================================================================
```

## 关键点

- **函数签名配置**：`ctypes` 调用前必须配置 `restype` 和 `argtypes`，否则默认按 int 返回（指针被截断）
- **字符串编码**：所有传入字符串需 `.encode("utf-8")`，返回字符串需 `.decode("utf-8")`
- **内存释放**：`hippocampus_get_data` / `hippocampus_get_error` 返回的字符串必须用 `hippocampus_free_string` 释放，否则内存泄漏
- **句柄生命周期**：推荐用 `with` 语句（已实现 `__enter__` / `__exit__`），保证异常情况下也释放
- **project_id 为 None**：传 `None` 而非空字符串，C 端会判 NULL

## 包装层说明

`demo.py` 内置了 `Hippocampus` 包装类，将 C ABI 包装为 Python 友好的 API：

- 自动处理字符串编解码
- 自动释放返回的字符串和结果对象
- 错误转为 `HippocampusError` 异常
- 支持 `with` 语句自动释放句柄

生产场景可直接复用此包装类，或基于它扩展（如增加类型注解、异步包装等）。
