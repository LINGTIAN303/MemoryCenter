"""评测公共模块：环境加载、LLM 客户端、MemoryCenter HTTP 封装、消息构造、时间戳解析、JSONL 续传。

设计要点：
- .env 在模块导入时自动加载，后续脚本只需 `from common import *` 即可用全部工具
- LLM 调用统一封装 call_llm()，内置简单重试（3 次指数退避）
- MemoryCenter 流程：archive（归档会话）→ /prompt（拉取记忆摘要），支持幂等（已归档则跳过）
- 断点续传：load_completed_keys() 读取已完成的 question_id/sample_id，跳过已完成项
"""
from __future__ import annotations

import json
import os
import re
import time
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import requests
from openai import OpenAI

# ---------------------------------------------------------------------------
# 1. .env 加载（与 probe_apis.py 一致的手写解析器）
# ---------------------------------------------------------------------------
_ENV_PATH = Path(__file__).parent / ".env"
if _ENV_PATH.exists():
    for _line in _ENV_PATH.read_text(encoding="utf-8").splitlines():
        _line = _line.strip()
        if not _line or _line.startswith("#") or "=" not in _line:
            continue
        _k, _v = _line.split("=", 1)
        os.environ.setdefault(_k.strip(), _v.strip())

# ---------------------------------------------------------------------------
# 2. 常量
# ---------------------------------------------------------------------------
MC_BASE = os.environ.get("MEMORY_CENTER_BASE_URL", "http://127.0.0.1:8765/api/v1")

# 结果目录
RESULTS_DIR = Path(__file__).resolve().parent.parent / "results"
RESULTS_DIR.mkdir(parents=True, exist_ok=True)

# 默认评测参数
DEFAULT_MODELS = ["sensenova", "step"]
DEFAULT_CONDITIONS = ["baseline", "memory_center"]

# 模型简称映射（用于 session_id 命名）
MODEL_SHORT = {
    "sensenova": "sn",
    "step": "step",
}


# ---------------------------------------------------------------------------
# 3. LLM 客户端工厂
# ---------------------------------------------------------------------------
class ModelConfig:
    """单个 LLM 的配置（base_url / api_key / model / 额外参数）。

    gen_max_tokens: 生成 hypothesis 时的默认 max_tokens。
      - 普通模型 1024 即可
      - reasoning 模型（如 Step）thinking tokens 会消耗配额，需 8192
    """

    def __init__(
        self,
        name: str,
        base_url: str,
        api_key: str,
        model: str,
        extra: dict | None = None,
        gen_max_tokens: int = 1024,
    ):
        self.name = name
        self.base_url = base_url
        self.api_key = api_key
        self.model = model
        self.extra = extra or {}
        self.gen_max_tokens = gen_max_tokens
        # 延迟创建 client，避免导入时就报错
        self._client: OpenAI | None = None

    @property
    def client(self) -> OpenAI:
        if self._client is None:
            self._client = OpenAI(api_key=self.api_key, base_url=self.base_url)
        return self._client


def get_model_config(name: str) -> ModelConfig:
    """根据简称返回 ModelConfig，name ∈ {sensenova, step, deepseek}。"""
    if name == "sensenova":
        return ModelConfig(
            name="sensenova",
            base_url=os.environ["SENSENOVA_BASE_URL"],
            api_key=os.environ["SENSENOVA_API_KEY"],
            model=os.environ["SENSENOVA_MODEL"],
            gen_max_tokens=4096,
        )
    if name == "step":
        return ModelConfig(
            name="step",
            base_url=os.environ["STEP_BASE_URL"],
            api_key=os.environ["STEP_API_KEY"],
            model=os.environ["STEP_MODEL"],
            extra={"reasoning_effort": os.environ.get("STEP_REASONING_EFFORT", "medium")},
            gen_max_tokens=4096,  # 用户指定统一 4096（原 8192，reasoning thinking 配额减半，存在截断风险）
        )
    if name == "deepseek":
        return ModelConfig(
            name="deepseek",
            base_url=os.environ["DEEPSEEK_BASE_URL"],
            api_key=os.environ["DEEPSEEK_API_KEY"],
            model=os.environ["DEEPSEEK_MODEL"],
            gen_max_tokens=4096,
        )
    raise ValueError(f"未知模型: {name}（支持: sensenova / step / deepseek）")


def call_llm(
    cfg: ModelConfig,
    messages: list[dict],
    temperature: float = 0.0,
    max_tokens: int | None = None,
    max_retries: int = 3,
) -> str:
    """统一 LLM 调用入口，内置 3 次指数退避重试。

    max_tokens 为 None 时自动使用 cfg.gen_max_tokens（Step reasoning 模型需 8192）。
    返回模型生成的文本。失败时抛出最后一次异常。
    """
    if max_tokens is None:
        max_tokens = cfg.gen_max_tokens
    kwargs: dict[str, Any] = {
        "model": cfg.model,
        "messages": messages,
        "temperature": temperature,
        "max_tokens": max_tokens,
        "timeout": 180,  # 防 LoCoMo baseline 大 messages 无限挂起（180s 足够）
    }
    kwargs.update(cfg.extra)

    last_err: Exception | None = None
    for attempt in range(max_retries):
        try:
            resp = cfg.client.chat.completions.create(**kwargs)
            content = resp.choices[0].message.content
            # 某些模型偶尔返回 None（如 SenseNova 触发安全过滤），视为空字符串
            return (content or "").strip()
        except Exception as e:  # noqa: BLE001 - 评测脚本需要尽量容错
            last_err = e
            if attempt < max_retries - 1:
                wait = 2 ** attempt  # 1s, 2s, 4s
                print(f"  [重试 {attempt + 1}/{max_retries}] {cfg.name} 调用失败: {type(e).__name__}: {e}，{wait}s 后重试")
                time.sleep(wait)
    raise last_err  # type: ignore[misc]


# ---------------------------------------------------------------------------
# 4. MemoryCenter HTTP API 封装
# ---------------------------------------------------------------------------
def mc_archive(session_id: str, turns: list[dict], timeout: int = 60) -> dict:
    """POST /sessions/{sid}/archive —— 归档会话，生成 daily 记忆文件。

    turns: MessageTurn 列表（用 make_message_turn 构造）。
    返回 SummaryView。
    """
    r = requests.post(
        f"{MC_BASE}/sessions/{session_id}/archive",
        json={"turns": turns, "project_id": None},
        timeout=timeout,
    )
    r.raise_for_status()
    return r.json()


def mc_get_summaries(session_id: str, timeout: int = 15) -> list[dict]:
    """GET /sessions/{sid}/summaries —— 返回已归档的摘要列表。"""
    r = requests.get(f"{MC_BASE}/sessions/{session_id}/summaries", timeout=timeout)
    r.raise_for_status()
    return r.json()


def mc_get_prompt(session_id: str, timeout: int = 30) -> str:
    """GET /sessions/{sid}/prompt —— 返回可直接注入 system prompt 的记忆摘要文本。"""
    r = requests.get(f"{MC_BASE}/sessions/{session_id}/prompt", timeout=timeout)
    r.raise_for_status()
    return r.json().get("prompt", "")


def mc_retrieve_all_content(session_id: str, timeout: int = 60) -> str:
    """retrieve 所有 MemoryFile 的完整对话内容，以紧凑格式返回。

    流程：
    1. GET /summaries 获取所有 hook_id
    2. 对每个 hook_id，GET /memories/{hook_id} retrieve 完整 MemoryFile
    3. 从 MemoryFile.turns 提取 user_message.text + llm_message.text
    4. 以紧凑格式拼接返回（User: xxx\\n\\nAssistant: yyy）

    用于评测：memory_center 条件下除了 /prompt 摘要钩子，还需 retrieve 完整对话内容，
    否则模型只看到摘要标题，无法回答需要具体细节的问题。
    """
    summaries = mc_get_summaries(session_id)
    if not summaries:
        return ""

    parts: list[str] = []
    for s in summaries:
        hook_id = s.get("hook_id")
        if not hook_id:
            continue
        r = requests.get(
            f"{MC_BASE}/sessions/{session_id}/memories/{hook_id}",
            timeout=timeout,
        )
        r.raise_for_status()
        memory_file = r.json()
        turns = memory_file.get("turns", [])
        for turn in turns:
            user_text = (turn.get("user_message") or {}).get("text", "") or ""
            llm_text = (turn.get("llm_message") or {}).get("text", "") or ""
            if user_text:
                parts.append(f"User: {user_text}")
            if llm_text:
                parts.append(f"Assistant: {llm_text}")

    return "\n\n".join(parts)


def mc_ensure_archived(session_id: str, turns: list[dict]) -> dict | None:
    """幂等归档：若 session 已有摘要则跳过，否则归档。

    用于断点续传场景——重跑 memory_center 条件时不会重复归档。
    返回：新建摘要（首次归档）或 None（已存在）。
    """
    existing = mc_get_summaries(session_id)
    if existing:
        return None  # 已有数据，跳过
    return mc_archive(session_id, turns)


# ---------------------------------------------------------------------------
# 5. MessageTurn 构造
# ---------------------------------------------------------------------------
def make_message_turn(
    user_text: str,
    llm_text: str,
    timestamp: str,
    tags: list[str] | None = None,
    token_count: int | None = None,
) -> dict:
    """构造 MessageTurn（严格匹配 Rust 端 MessageTurn struct）。

    - tags: Tag 枚举的 kind 值列表，默认 ["Text"]
    - token_count: 用 len(split) 估算（若为 None 则自动估算）
    - timestamp: ISO 8601 格式字符串
    """
    if tags is None:
        tags = ["Text"]
    if token_count is None:
        token_count = len((user_text + " " + llm_text).split())
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


# ---------------------------------------------------------------------------
# 6. 时间戳解析
# ---------------------------------------------------------------------------
def parse_lme_timestamp(raw: str) -> str:
    """解析 LongMemEval 时间戳，如 '2023/04/10 (Mon) 17:50' → ISO 8601。

    格式: YYYY/MM/DD (Weekday) HH:MM
    返回: '2023-04-10T17:50:00Z'
    """
    # 提取 日期 + 时间两部分
    m = re.match(r"(\d{4})/(\d{2})/(\d{2})\s*\([^)]+\)\s*(\d{2}):(\d{2})", raw)
    if not m:
        # fallback：用 question_date 或当前时间
        return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    y, mo, d, h, mi = m.groups()
    return f"{y}-{mo}-{d}T{h}:{mi}:00Z"


def parse_locomo_timestamp(raw: str) -> str:
    """解析 LoCoMo 时间戳，如 '1:56 pm on 8 May, 2023' → ISO 8601。

    格式: H:MM am/pm on D Month, YYYY
    返回: '2023-05-08T13:56:00Z'
    """
    try:
        dt = datetime.strptime(raw.strip(), "%I:%M %p on %d %B, %Y")
        return dt.strftime("%Y-%m-%dT%H:%M:00Z")
    except ValueError:
        # 尝试其他可能的格式
        try:
            dt = datetime.strptime(raw.strip(), "%H:%M on %d %B, %Y")
            return dt.strftime("%Y-%m-%dT%H:%M:00Z")
        except ValueError:
            return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


# ---------------------------------------------------------------------------
# 7. JSONL 断点续传
# ---------------------------------------------------------------------------
def load_completed_keys(jsonl_path: Path, key_field: str, extra_key_field: str | None = None) -> set[tuple]:
    """读取已完成的 JSONL，返回主键集合用于跳过。

    - key_field: 主键字段名（如 'question_id'）
    - extra_key_field: 可选的次级键（如 'qa_index'），与主键组合成 tuple
    - 只跳过「无 error 字段且 hypothesis 非空」的条目（有 error 的会重试）

    返回 set[str] 或 set[tuple]，取决于 extra_key_field。
    """
    if not jsonl_path.exists():
        return set()
    done: set[tuple] = set()
    for line in jsonl_path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            entry = json.loads(line)
        except json.JSONDecodeError:
            continue
        # 有 error 或 hypothesis 为空 → 视为未完成，需要重试
        if entry.get("error") or not entry.get("hypothesis"):
            continue
        primary = entry.get(key_field)
        if primary is None:
            continue
        if extra_key_field is not None:
            secondary = entry.get(extra_key_field)
            done.add((primary, secondary))
        else:
            done.add((primary,))
    return done


def compact_jsonl(jsonl_path: Path) -> None:
    """压缩 JSONL：保留已完成的条目，移除失败/空条目（防止重试产生重复）。

    在评测开始前调用，确保 resume 干净无重复。
    """
    if not jsonl_path.exists():
        return
    kept: list[str] = []
    for line in jsonl_path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            entry = json.loads(line)
        except json.JSONDecodeError:
            continue
        # 只保留成功的条目（无 error 且 hypothesis 非空）
        if not entry.get("error") and entry.get("hypothesis"):
            kept.append(line)
    # 重写文件（仅保留成功条目）
    jsonl_path.write_text("\n".join(kept) + ("\n" if kept else ""), encoding="utf-8")


def append_jsonl(jsonl_path: Path, entry: dict) -> None:
    """向 JSONL 文件追加一行（自动 flush 保证落盘）。"""
    with open(jsonl_path, "a", encoding="utf-8") as f:
        f.write(json.dumps(entry, ensure_ascii=False) + "\n")
        f.flush()


# ---------------------------------------------------------------------------
# 8. 汇总报告
# ---------------------------------------------------------------------------
def save_summary_report(stats: dict, name: str = "summary") -> Path:
    """将统计结果写入 results/summary.json（合并模式）。"""
    path = RESULTS_DIR / f"{name}.json"
    existing: dict = {}
    if path.exists():
        try:
            existing = json.loads(path.read_text(encoding="utf-8"))
        except json.JSONDecodeError:
            existing = {}
    existing.update(stats)
    path.write_text(json.dumps(existing, ensure_ascii=False, indent=2), encoding="utf-8")
    return path
