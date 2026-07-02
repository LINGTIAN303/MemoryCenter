"""Hippocampus HTTP API 端到端探针：验证 archive → summaries → prompt 流程。"""
import json
import uuid
import requests

HIPPO_BASE = "http://127.0.0.1:8765/api/v1"
SID = "probe-session-001"


def make_turn(user_text: str, llm_text: str, tags: list[str], timestamp: str, token_count: int) -> dict:
    """构造 MessageTurn（与 Rust struct 对齐）。"""
    return {
        "id": str(uuid.uuid4()),
        "user_message": {
            "text": user_text,
            "attachments": [],
            "tool_calls": [],
            "thinking": None,
        },
        "llm_message": {
            "text": llm_text,
            "attachments": [],
            "tool_calls": [],
            "thinking": None,
        },
        "tags": [{"kind": t} for t in tags],
        "timestamp": timestamp,
        "token_count": token_count,
    }


def archive(session_id: str, turns: list[dict]) -> dict:
    r = requests.post(
        f"{HIPPO_BASE}/sessions/{session_id}/archive",
        json={"turns": turns, "project_id": None},
        timeout=30,
    )
    r.raise_for_status()
    return r.json()


def summaries(session_id: str) -> dict:
    r = requests.get(f"{HIPPO_BASE}/sessions/{session_id}/summaries", timeout=10)
    r.raise_for_status()
    return r.json()


def prompt(session_id: str) -> dict:
    r = requests.get(f"{HIPPO_BASE}/sessions/{session_id}/prompt", timeout=10)
    r.raise_for_status()
    return r.json()


def main():
    turns = [
        make_turn(
            "我叫小明，今年 25 岁，住在上海。",
            "你好小明！很高兴认识你。上海是个很棒的城市。",
            ["Text"],
            "2026-07-01T10:00:00Z",
            50,
        ),
        make_turn(
            "我在一家互联网公司做后端开发，主要用 Rust。",
            "Rust 是一门优秀的系统编程语言，性能和安全性都很好。",
            ["Text", "CodeBlock"],
            "2026-07-01T10:05:00Z",
            60,
        ),
    ]

    print("=== 1. Archive 测试 ===")
    result = archive(SID, turns)
    print(json.dumps(result, ensure_ascii=False, indent=2))

    print("\n=== 2. Summaries 测试 ===")
    s = summaries(SID)
    print(json.dumps(s, ensure_ascii=False, indent=2))

    print("\n=== 3. Prompt 测试 ===")
    p = prompt(SID)
    print(json.dumps(p, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
