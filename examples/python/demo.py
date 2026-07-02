#!/usr/bin/env python3
"""
Hippocampus Python 调用示例（基于 ctypes）

演示通过 ctypes 调用 Hippocampus 的 C ABI，覆盖 5 个核心操作。

前置准备：
    1. 在项目根目录执行：cargo build --release -p hippocampus-ffi
    2. 确认动态库路径：
       - Linux:   target/release/libhippocampus.so
       - macOS:   target/release/libhippocampus.dylib
       - Windows: target/release/hippocampus.dll

运行：
    cd examples/python
    python3 demo.py
"""

import ctypes
import json
import os
import sys
import uuid
from datetime import datetime, timezone
from pathlib import Path


# =============================================================================
# 1. 加载动态库 + 配置函数签名
# =============================================================================

def load_library():
    """加载 Hippocampus 动态库。"""
    # 定位项目根目录（示例位于 examples/python/，向上两级）
    project_root = Path(__file__).resolve().parent.parent.parent
    target_dir = project_root / "target" / "release"

    if sys.platform == "win32":
        lib_path = target_dir / "hippocampus.dll"
    elif sys.platform == "darwin":
        lib_path = target_dir / "libhippocampus.dylib"
    else:
        lib_path = target_dir / "libhippocampus.so"

    if not lib_path.exists():
        raise FileNotFoundError(
            f"动态库不存在：{lib_path}\n"
            "请先在项目根目录执行：cargo build --release -p hippocampus-ffi"
        )

    lib = ctypes.CDLL(str(lib_path))

    # ---- 配置函数签名 ----
    # 句柄生命周期
    lib.hippocampus_new.restype = ctypes.c_void_p
    lib.hippocampus_new.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p]
    lib.hippocampus_free.restype = None
    lib.hippocampus_free.argtypes = [ctypes.c_void_p]

    # 结果处理
    lib.hippocampus_is_ok.restype = ctypes.c_bool
    lib.hippocampus_is_ok.argtypes = [ctypes.c_void_p]
    lib.hippocampus_get_data.restype = ctypes.POINTER(ctypes.c_char)
    lib.hippocampus_get_data.argtypes = [ctypes.c_void_p]
    lib.hippocampus_get_error.restype = ctypes.POINTER(ctypes.c_char)
    lib.hippocampus_get_error.argtypes = [ctypes.c_void_p]
    lib.hippocampus_result_free.restype = None
    lib.hippocampus_result_free.argtypes = [ctypes.c_void_p]
    lib.hippocampus_free_string.restype = None
    lib.hippocampus_free_string.argtypes = [ctypes.POINTER(ctypes.c_char)]

    # 核心操作
    lib.hippocampus_archive.restype = ctypes.c_void_p
    lib.hippocampus_archive.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
    lib.hippocampus_retrieve.restype = ctypes.c_void_p
    lib.hippocampus_retrieve.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
    lib.hippocampus_get_summaries.restype = ctypes.c_void_p
    lib.hippocampus_get_summaries.argtypes = [ctypes.c_void_p]
    lib.hippocampus_render_prompt.restype = ctypes.c_void_p
    lib.hippocampus_render_prompt.argtypes = [ctypes.c_void_p]
    lib.hippocampus_run_compaction.restype = ctypes.c_void_p
    lib.hippocampus_run_compaction.argtypes = [ctypes.c_void_p, ctypes.c_uint]

    return lib


# 周期常量
HIPPOCAMPUS_COMPACTION_WEEKLY = 0
HIPPOCAMPUS_COMPACTION_MONTHLY = 1


# =============================================================================
# 2. 包装层：将 C ABI 包装为 Python 友好的 API
# =============================================================================

class HippocampusError(Exception):
    """Hippocampus 调用错误。"""


class Hippocampus:
    """Hippocampus 句柄的 Python 包装。"""

    def __init__(self, lib, root_path, session_id, project_id=None):
        self._lib = lib
        root_b = root_path.encode("utf-8")
        sid_b = session_id.encode("utf-8")
        pid_b = project_id.encode("utf-8") if project_id else None
        self._handle = lib.hippocampus_new(root_b, sid_b, pid_b)
        if not self._handle:
            raise HippocampusError("创建句柄失败（参数无效或 runtime 创建失败）")

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
        return False

    def close(self):
        if self._handle:
            self._lib.hippocampus_free(self._handle)
            self._handle = None

    def _get_result(self, result_ptr):
        """统一提取结果，成功返回 data 字符串，失败抛 HippocampusError。"""
        try:
            if not self._lib.hippocampus_is_ok(result_ptr):
                err_ptr = self._lib.hippocampus_get_error(result_ptr)
                if err_ptr:
                    err_msg = ctypes.cast(err_ptr, ctypes.c_char_p).value.decode("utf-8")
                    self._lib.hippocampus_free_string(err_ptr)
                else:
                    err_msg = "(未知错误)"
                raise HippocampusError(err_msg)

            data_ptr = self._lib.hippocampus_get_data(result_ptr)
            data = ctypes.cast(data_ptr, ctypes.c_char_p).value.decode("utf-8") if data_ptr else ""
            self._lib.hippocampus_free_string(data_ptr)
            return data
        finally:
            self._lib.hippocampus_result_free(result_ptr)

    # ---- 5 个核心操作 ----

    def archive(self, turns):
        """归档一批轮次，返回 SummaryView 字典。"""
        turns_json = json.dumps(turns, ensure_ascii=False).encode("utf-8")
        result = self._lib.hippocampus_archive(self._handle, turns_json)
        return json.loads(self._get_result(result))

    def retrieve(self, hook_id):
        """按钩子 ID 检索完整记忆文件。"""
        result = self._lib.hippocampus_retrieve(self._handle, hook_id.encode("utf-8"))
        return json.loads(self._get_result(result))

    def get_summaries(self):
        """获取所有周期摘要视图列表。"""
        result = self._lib.hippocampus_get_summaries(self._handle)
        return json.loads(self._get_result(result))

    def render_prompt(self):
        """渲染摘要为 system prompt 文本（非 JSON）。"""
        result = self._lib.hippocampus_render_prompt(self._handle)
        return self._get_result(result)  # 直接返回字符串

    def run_compaction(self, period):
        """触发周期任务（HIPPOCAMPUS_COMPACTION_WEEKLY / MONTHLY）。"""
        result = self._lib.hippocampus_run_compaction(self._handle, period)
        return json.loads(self._get_result(result))


# =============================================================================
# 3. 辅助函数：构造 MessageTurn
# =============================================================================

def make_turn(user_text, llm_text, token_count, tags=None):
    """构造一个 MessageTurn 字典（符合 Rust 端 serde 反序列化）。"""
    if tags is None:
        tags = [{"kind": "Text"}]
    return {
        "id": str(uuid.uuid4()),
        "user_message": {"text": user_text, "attachments": [], "tool_calls": [], "thinking": None},
        "llm_message": {"text": llm_text, "attachments": [], "tool_calls": [], "thinking": None},
        "tags": tags,
        "timestamp": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "token_count": token_count,
    }


# =============================================================================
# 4. 演示主流程
# =============================================================================

def main():
    lib = load_library()

    # 准备存储目录（脚本所在目录下）
    storage_root = Path(__file__).resolve().parent / "mem_data"
    storage_root.mkdir(exist_ok=True)

    print("=" * 70)
    print("Hippocampus Python 示例 - 通过 ctypes 调用 C ABI")
    print("=" * 70)

    # 1. 创建句柄（用 with 语句保证自动释放）
    with Hippocampus(lib, str(storage_root), "py-session-001") as h:
        print("\n[1] 句柄创建成功")

        # 2. 构造并归档 2 轮对话
        turns = [
            make_turn(
                "你好，介绍一下记忆库设计",
                "Hippocampus 采用三级索引周期：天级归档 / 周级合并 / 月级淘汰",
                80,
                tags=[{"kind": "Text"}, {"kind": "CodeBlock"}],
            ),
            make_turn(
                "如何通过 Python 调用？",
                "通过 ctypes 加载动态库，配置函数签名即可调用 5 个核心操作",
                60,
                tags=[{"kind": "Text"}, {"kind": "CodeBlock"}],
            ),
        ]
        summary = h.archive(turns)
        print(f"\n[2] 归档成功")
        print(f"    hook_id:         {summary['hook_id']}")
        print(f"    memory_file_id:  {summary['memory_file_id']}")
        print(f"    summary_title:   {summary['summary_title']}")
        print(f"    tags:            {summary['tags']}")
        print(f"    token_count:     {summary['token_count']}")

        # 3. 获取所有摘要视图
        summaries = h.get_summaries()
        print(f"\n[3] 所有摘要视图（共 {len(summaries)} 条）")
        for s in summaries:
            print(f"    - {s['summary_title']} [{', '.join(s['tags'])}] ({s['token_count']} tokens)")

        # 4. 渲染 system prompt（可直接注入 LLM）
        prompt = h.render_prompt()
        print(f"\n[4] 渲染的 system prompt（{len(prompt)} 字符）:")
        print("-" * 70)
        print(prompt)
        print("-" * 70)

        # 5. 按钩子 ID 检索完整记忆文件（模拟 LLM 通过 tool 调用）
        hook_id = summary["hook_id"]
        memory = h.retrieve(hook_id)
        print(f"\n[5] 检索完整记忆文件（hook_id={hook_id}）")
        print(f"    memory_file_id:  {memory['id']}")
        print(f"    turns 数量:       {len(memory['turns'])}")
        print(f"    total_tokens:    {memory['total_tokens']}")
        print(f"    truncated:       {memory['truncated']}")

        print("\n" + "=" * 70)
        print("演示完成")
        print(f"记忆文件已保存到：{storage_root}")
        print("=" * 70)


if __name__ == "__main__":
    main()
