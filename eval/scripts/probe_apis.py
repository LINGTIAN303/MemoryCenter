"""最小 API 探针：验证 SenseNova / Step / DeepSeek 三个 API key 是否可用。"""
import os
import sys
import time
from pathlib import Path

# 加载 .env
env_path = Path(__file__).parent / ".env"
for line in env_path.read_text(encoding="utf-8").splitlines():
    line = line.strip()
    if not line or line.startswith("#"):
        continue
    if "=" in line:
        k, v = line.split("=", 1)
        os.environ.setdefault(k.strip(), v.strip())

from openai import OpenAI


def probe(name: str, base_url: str, api_key: str, model: str, extra: dict | None = None) -> None:
    print(f"\n=== {name} ===")
    print(f"  base_url: {base_url}")
    print(f"  model: {model}")
    client = OpenAI(api_key=api_key, base_url=base_url)
    t0 = time.time()
    try:
        kwargs = {
            "model": model,
            "messages": [{"role": "user", "content": "Reply with the word: OK"}],
            "max_tokens": 1024,  # reasoning 模式需要更大
            "temperature": 0,
        }
        if extra:
            kwargs.update(extra)
        resp = client.chat.completions.create(**kwargs)
        content = resp.choices[0].message.content.strip()
        dt = time.time() - t0
        print(f"  ✓ 耗时 {dt:.2f}s, 返回: {content[:100]}")
    except Exception as e:
        dt = time.time() - t0
        print(f"  ✗ 耗时 {dt:.2f}s, 错误: {type(e).__name__}: {e}", file=sys.stderr)


probe(
    "SenseNova",
    os.environ["SENSENOVA_BASE_URL"],
    os.environ["SENSENOVA_API_KEY"],
    os.environ["SENSENOVA_MODEL"],
)

probe(
    "Step",
    os.environ["STEP_BASE_URL"],
    os.environ["STEP_API_KEY"],
    os.environ["STEP_MODEL"],
    extra={"reasoning_effort": "medium"},
)

probe(
    "DeepSeek",
    os.environ["DEEPSEEK_BASE_URL"],
    os.environ["DEEPSEEK_API_KEY"],
    os.environ["DEEPSEEK_MODEL"],
)
