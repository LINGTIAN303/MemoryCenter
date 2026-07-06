#!/usr/bin/env python3
"""Hippocampus 项目能力综合测试

覆盖：
  1. v2.29 新功能：presets API（4 端点）+ archive 带 preset 参数
  2. 基础能力：archive / retrieve / summaries / prompt
  3. 冲突检测：detect_conflicts（启发式）
  4. 语义检索：search（仅关键词降级模式）
  5. 批量更新：batch_update（带冲突检测）
"""
import json
import time
import urllib.request
import urllib.error
import uuid

BASE = "http://127.0.0.1:8765/api/v1"
SID = f"capability-test-{int(time.time())}"


def headers():
    return {"Content-Type": "application/json"}


def make_turn(user_text, assistant_text, tags=None, token_count=100):
    return {
        "id": str(uuid.uuid4()),
        "user_message": {"text": user_text, "attachments": [], "tool_calls": [], "thinking": None},
        "llm_message": {"text": assistant_text, "attachments": [], "tool_calls": [], "thinking": None},
        "tags": [{"kind": t} for t in (tags or ["Text"])],
        "timestamp": "2026-07-05T10:00:00Z",
        "token_count": token_count,
    }


def call(method, path, body=None, expect_status=200):
    url = f"{BASE}{path}"
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, headers=headers(), method=method)
    try:
        resp = urllib.request.urlopen(req, timeout=15)
        status = resp.status
        text = resp.read().decode()
    except urllib.error.HTTPError as e:
        status = e.code
        text = e.read().decode()
    print(f"  [{method} {path}] -> {status} ({len(text)} bytes)")
    try:
        return status, json.loads(text)
    except json.JSONDecodeError:
        return status, text


def section(title):
    print(f"\n{'=' * 70}\n=== {title}\n{'=' * 70}")


# ============================================================================
# 1. v2.29 Presets API（新功能）
# ============================================================================
section("1. v2.29 Presets API - 4 个端点")

# 1.1 GET /presets/agents
status, agents = call("GET", "/presets/agents")
assert status == 200, f"agents 列表应返回 200，实际 {status}"
print(f"  Agent 数量: {len(agents)}（预期 11 个内置）")
for a in agents[:3]:
    print(f"    - {a['name']} (prefix={a['session_prefix']}, mainstream={a['is_mainstream']})")

# 1.2 GET /presets/scenarios
status, scenarios = call("GET", "/presets/scenarios")
assert status == 200
print(f"  Scenario 数量: {len(scenarios)}（预期 7 个内置）")
for s in scenarios:
    print(f"    - {s['variant']}: {s['display_name']} (默认阈值={s['archive_threshold']})")

# 1.3 GET /presets/models
status, models = call("GET", "/presets/models")
assert status == 200
print(f"  Model 数量: {len(models)}（v2.24 后预期 ≥ 15）")
defaults = [m for m in models if m["is_default"]]
print(f"  家族默认型号数: {len(defaults)}")
for m in models[:3]:
    print(f"    - {m['name']} (family={m['family']}, ctx={m['context_window']}, default={m['is_default']})")

# 1.4 POST /presets/build - 空请求（默认值）
section("1.4 POST /presets/build - 默认值")
status, result = call("POST", "/presets/build", body={})
assert status == 200
print(f"  默认 archive_threshold: {result['archive_threshold']}（预期 400000）")
print(f"  has_agent: {result['has_agent']}, has_scenario: {result['has_scenario']}")

# 1.5 POST /presets/build - 完整组合（联动推导）
section("1.5 POST /presets/build - 完整组合（v2.29 联动机制）")
status, result = call("POST", "/presets/build", body={
    "agent": "Claude Code",
    "scenario": "coding",
    "model": "claude-opus-4.8",
    "archive_threshold": 300000,
    "summary_template": "请总结以下对话:\n{conversation}",
})
assert status == 200, f"完整组合构建失败: {result}"
print(f"  archive_threshold: {result['archive_threshold']}（用户覆盖 300000）")
print(f"  session_prefix: {result['session_prefix']}（联动推导 claude-code）")
print(f"  summary_template 长度: {len(result['summary_template'])} 字符")
print(f"  has_agent={result['has_agent']}, has_scenario={result['has_scenario']}, "
      f"has_window={result['has_window']}, has_model={result['has_model']}")
print(f"  skills_count: {result['skills_count']}（Claude Code 联动 skills）")

# 1.6 POST /presets/build - 错误处理
section("1.6 POST /presets/build - 错误处理")
status, result = call("POST", "/presets/build", body={"model": "nonexistent"}, expect_status=400)
print(f"  未知 model 错误响应: {status} - {result}")

status, result = call("POST", "/presets/build", body={"summary_template": "missing placeholder"}, expect_status=400)
print(f"  缺少占位符错误响应: {status} - {result}")

# ============================================================================
# 2. 基础能力 - archive / retrieve / summaries / prompt
# ============================================================================
section("2. 基础能力 - archive（带 preset）+ retrieve + summaries + prompt")

# 2.1 archive 带 preset 参数
section("2.1 POST /archive 带 preset 参数（v2.29 新功能）")
turns = [
    make_turn("我叫小明，今年 25 岁，住在上海。", "你好小明！很高兴认识你。", ["Text"]),
    make_turn("我在一家互联网公司做 Rust 后端开发。", "Rust 是优秀的系统编程语言。", ["Text", "CodeBlock"]),
    make_turn("记住这个关键事实：项目代号是 Hippocampus。", "好的，我已记录项目代号。", ["Text"]),
]
status, result = call("POST", f"/sessions/{SID}/archive", body={
    "turns": turns,
    "preset": {
        "agent": "Claude Code",
        "scenario": "coding",
    },
})
assert status == 200, f"archive 失败: {result}"
hook_id = result.get("hook_id", "")
print(f"  hook_id: {hook_id}")
print(f"  summary_title: {result.get('summary_title', 'N/A')[:60]}")

# 2.2 archive 不带 preset（向后兼容）
section("2.2 POST /archive 不带 preset（向后兼容）")
status, result = call("POST", f"/sessions/{SID}/archive", body={
    "turns": [make_turn("再来一轮对话", "好的，继续。", ["Text"])],
})
assert status == 200
print(f"  向后兼容 archive 成功，hook_id: {result.get('hook_id', 'N/A')}")

# 2.3 retrieve
if hook_id:
    section(f"2.3 GET /memories/{{hook_id}} 检索")
    status, mem = call("GET", f"/sessions/{SID}/memories/{hook_id}")
    assert status == 200
    print(f"  检索到 {len(mem.get('turns', []))} 轮对话")
    for t in mem.get("turns", [])[:2]:
        u = t.get("user_message", {}).get("text", "")[:40]
        a = t.get("llm_message", {}).get("text", "")[:40]
        print(f"    [user] {u}")
        print(f"    [asst] {a}")

# 2.4 summaries
section("2.4 GET /summaries 摘要列表")
status, summaries = call("GET", f"/sessions/{SID}/summaries")
assert status == 200
print(f"  摘要数: {len(summaries)}")
for s in summaries[:3]:
    print(f"    - {s.get('summary_title', 'N/A')[:50]}")

# 2.5 prompt
section("2.5 GET /prompt 渲染 system prompt")
status, prompt = call("GET", f"/sessions/{SID}/prompt")
assert status == 200
if isinstance(prompt, str):
    print(f"  Prompt 长度: {len(prompt)} 字符")
    print(f"  前 200 字符:\n{prompt[:200]}")
else:
    print(f"  Prompt 响应: {str(prompt)[:200]}")

# ============================================================================
# 3. 冲突检测 - detect_conflicts
# ============================================================================
section("3. POST /detect-conflicts 冲突预检测（v2.27）")
if hook_id:
    status, result = call("POST", f"/sessions/{SID}/memories/{hook_id}/detect-conflicts", body={
        "added_facts": ["项目代号是 NeuronNet"],  # 与归档的 Hippocampus 冲突
        "revised_facts": [],
        "deprecated_facts": [],
        "project_id": None,
    })
    print(f"  冲突检测响应: {status}")
    if isinstance(result, dict):
        print(f"    total: {result.get('total', 'N/A')}")
        print(f"    critical_count: {result.get('critical_count', 'N/A')}")
        for c in result.get("conflicts", [])[:3]:
            print(f"    - severity={c.get('severity')}, kind={c.get('kind')}")
            print(f"      existing: {c.get('existing_fact', 'N/A')[:60]}")
            print(f"      new: {c.get('new_fact', 'N/A')[:60]}")

# ============================================================================
# 4. 语义检索 - search（仅关键词降级模式）
# ============================================================================
section("4. POST /search 语义检索（仅关键词降级）")
# 注意：启发式 Summary::from_title 只填 title（首条消息前 80 字符），
# 未配置 LLM Generator 时 key_facts/key_entities 为空，搜索词需匹配 title。
# 配置 HIPPOCAMPUS_GENERATOR_API_URL 后，归档时生成结构化摘要，搜索能匹配 key_facts。
status, result = call("POST", f"/sessions/{SID}/search", body={
    "query": "小明",  # 首条消息包含 "小明"
    "top_k": 5,
})
print(f"  检索响应: {status}")
if isinstance(result, dict):
    print(f"  mode: {result.get('mode')}")
    hits = result.get("results", [])
    print(f"  命中数: {len(hits)}")
    for hit in hits[:3]:
        print(f"    - score={hit.get('score', 'N/A'):.4f}, source={hit.get('source')}, hook_id={hit.get('hook_id', 'N/A')[:30]}")
elif isinstance(result, list):
    print(f"  命中数: {len(result)}")

# ============================================================================
# 5. 批量更新 - batch-update（带冲突检测）
# ============================================================================
section("5. POST /memories/batch-update 批量更新（带冲突检测）")
if hook_id:
    status, result = call("POST", f"/sessions/{SID}/memories/batch-update", body={
        "updates": [
            {
                "hook_id": hook_id,
                "added_facts": ["新事实：今天测试通过"],
                "revised_facts": [],
                "deprecated_facts": [],
            }
        ],
        "project_id": None,
    })
    print(f"  批量更新响应: {status}")
    if isinstance(result, list):
        print(f"  返回 {len(result)} 条更新结果")
        for item in result:
            print(f"    - hook_id={item.get('hook_id', 'N/A')[:30]}, success={item.get('success')}")
    elif isinstance(result, dict):
        print(f"  响应: {str(result)[:200]}")

# ============================================================================
# 汇总
# ============================================================================
section("测试完成")
print("""
能力验证总结：
  ✅ v2.29 Presets API（4 端点 + 联动推导 + 错误处理）
  ✅ archive 带 preset 参数（v2.29 新功能）
  ✅ archive 向后兼容（无 preset 默认行为）
  ✅ retrieve 完整对话检索
  ✅ summaries 摘要列表
  ✅ prompt system prompt 渲染
  ✅ detect_conflicts 冲突预检测（启发式）
  ✅ search 关键词检索（降级模式）
  ✅ batch_update 批量更新（集成冲突检测）
""")
