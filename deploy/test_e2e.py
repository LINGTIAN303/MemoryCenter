#!/usr/bin/env python3
"""MemoryCenter HTTP 服务端到端测试

用法：
  python3 test_e2e.py [API_KEY]

  API_KEY - 可选，配置了 MEMORY_CENTER_API_KEY 后必填，会作为
            Authorization: Bearer <API_KEY> 头携带
"""
import os
import sys
import urllib.request
import json
import uuid

# 从命令行参数或环境变量读取 API Key
API_KEY = sys.argv[1] if len(sys.argv) > 1 else os.environ.get("MEMORY_CENTER_API_KEY", "")
BASE = "http://127.0.0.1:8765/api/v1"
SID = "e2e-test"

def auth_headers():
    """构造鉴权头（未配置 API_KEY 时返回空 dict，兼容本地零配置）"""
    return {"Authorization": f"Bearer {API_KEY}"} if API_KEY else {}

def make_turn(user_text, assistant_text):
    """构造符合 MessageTurn 结构的轮次（token_count 必填，text 为 Option<String>）"""
    return {
        "id": str(uuid.uuid4()),
        "user_message": {"text": user_text, "attachments": [], "tool_calls": [], "thinking": None},
        "llm_message": {"text": assistant_text, "attachments": [], "tool_calls": [], "thinking": None},
        "tags": [],
        "timestamp": "2026-07-04T21:00:00Z",
        "token_count": 100
    }

# 1. 归档
print("=== 1. 归档测试 ===")
data = json.dumps({
    "turns": [
        make_turn("Hello MemoryCenter, 记住这个测试", "好的，我已记录这条消息"),
        make_turn("今天天气怎么样", "今天北京晴天，气温32度"),
    ]
}).encode()
req = urllib.request.Request(
    f"{BASE}/sessions/{SID}/archive",
    data=data,
    headers={**auth_headers(), "Content-Type": "application/json"},
)
resp = urllib.request.urlopen(req)
result = json.loads(resp.read().decode())
print(f"状态: {resp.status}")
print(f"hook_id: {result.get('hook_id', 'N/A')}")
hook_id = result.get('hook_id', '')
print(f"摘要标题: {result.get('summary_title', 'N/A')[:80]}")

# 2. 检索
if hook_id:
    print(f"\n=== 2. 检索测试 (hook_id={hook_id}) ===")
    req = urllib.request.Request(
        f"{BASE}/sessions/{SID}/memories/{hook_id}",
        headers=auth_headers(),
    )
    resp = urllib.request.urlopen(req)
    mem = json.loads(resp.read().decode())
    print(f"状态: {resp.status}")
    print(f"消息数: {len(mem.get('turns', []))}")
    for t in mem.get('turns', []):
        user_text = t.get('user_message', {}).get('text', '')[:50]
        llm_text = t.get('llm_message', {}).get('text', '')[:50]
        print(f"  [user] {user_text}")
        print(f"  [asst] {llm_text}")

# 3. 摘要列表
print(f"\n=== 3. 摘要列表测试 ===")
req = urllib.request.Request(
    f"{BASE}/sessions/{SID}/summaries",
    headers=auth_headers(),
)
resp = urllib.request.urlopen(req)
summaries = json.loads(resp.read().decode())
print(f"状态: {resp.status}")
print(f"摘要数: {len(summaries)}")
for s in summaries[:3]:
    print(f"  - {s.get('summary_title', 'N/A')[:60]}")

# 4. Prompt 渲染
print(f"\n=== 4. Prompt 渲染测试 ===")
req = urllib.request.Request(
    f"{BASE}/sessions/{SID}/prompt",
    headers=auth_headers(),
)
resp = urllib.request.urlopen(req)
prompt = resp.read().decode()
print(f"状态: {resp.status}")
print(f"Prompt 长度: {len(prompt)} 字符")
print(f"前 200 字符: {prompt[:200]}")

# 5. 公网 Nginx 反代测试（走 openworld 域名 + Mozilla UA，避免 WAF 拦截）
print(f"\n=== 5. 公网 Nginx 反代测试 ===")
req = urllib.request.Request(
    "https://openworld.dpdns.org/memory-center/api/v1/sessions/e2e-test/summaries",
    headers={**auth_headers(), "User-Agent": "Mozilla/5.0"},
)
resp = urllib.request.urlopen(req, timeout=10)
print(f"状态: {resp.status}")
body = resp.read().decode()
print(f"响应: {body[:200]}")
assert body.startswith("[") or body.startswith("{"), "反代响应应为 JSON"

print("\n=== 端到端测试全部通过 ===")

