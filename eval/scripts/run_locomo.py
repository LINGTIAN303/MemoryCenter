"""LoCoMo 评测脚本：baseline（裸考）vs memory_center（记忆摘要）对比。

流程：
1. 加载 locomo10.json（10 个 sample）
2. 对每个 sample×模型×条件，对每个 QA 生成 hypothesis：
   - baseline：所有 session 拼成对话历史，直接问 LLM
   - memory_center：每 session 归档为 1 个 daily 文件 → /prompt 拉摘要 → 注入 system prompt
3. F1/EM 评分（严格复制自 locomo 官方 evaluation.py）
4. 输出 JSONL + 统计报告

用法：
  python run_locomo.py                    # 全部 10 sample
  python run_locomo.py --smoke-test       # 1 sample，且每 sample 只跑前 3 个 QA
  python run_locomo.py --n-samples 3      # 只跑 3 个 sample
"""
from __future__ import annotations

import argparse
import json
import re
import string
import sys
import time
from collections import Counter, defaultdict
from pathlib import Path

import numpy as np
import regex
from nltk.stem import PorterStemmer
from tqdm import tqdm

from common import (
    DEFAULT_CONDITIONS,
    DEFAULT_MODELS,
    MODEL_SHORT,
    RESULTS_DIR,
    append_jsonl,
    call_llm,
    compact_jsonl,
    get_model_config,
    mc_archive,
    mc_get_prompt,
    mc_get_summaries,
    mc_retrieve_all_content,
    load_completed_keys,
    make_message_turn,
    parse_locomo_timestamp,
    save_summary_report,
)

# ---------------------------------------------------------------------------
# 路径
# ---------------------------------------------------------------------------
DATA_FILE = Path(__file__).resolve().parent.parent / "locomo" / "data" / "locomo10.json"


# ---------------------------------------------------------------------------
# F1/EM 评分算法（严格复制自 locomo 官方 evaluation.py）
# ---------------------------------------------------------------------------
ps = PorterStemmer()


def normalize_answer(s: str) -> str:
    """标准化答案：去逗号 → 去冠词(a/an/the/and) → 去标点 → 小写 → 合并空格。"""
    s = s.replace(",", "")

    def remove_articles(text: str) -> str:
        return regex.sub(r"\b(a|an|the|and)\b", " ", text)

    def white_space_fix(text: str) -> str:
        return " ".join(text.split())

    def remove_punc(text: str) -> str:
        exclude = set(string.punctuation)
        return "".join(ch for ch in text if ch not in exclude)

    def lower(text: str) -> str:
        return text.lower()

    return white_space_fix(remove_articles(remove_punc(lower(s))))


def f1_score(prediction: str, ground_truth: str) -> float:
    """单答案 F1（用 Porter 词干提取）。"""
    prediction_tokens = [ps.stem(w) for w in normalize_answer(prediction).split()]
    ground_truth_tokens = [ps.stem(w) for w in normalize_answer(ground_truth).split()]
    common = Counter(prediction_tokens) & Counter(ground_truth_tokens)
    num_same = sum(common.values())
    if num_same == 0:
        return 0.0
    precision = 1.0 * num_same / len(prediction_tokens)
    recall = 1.0 * num_same / len(ground_truth_tokens)
    f1 = (2 * precision * recall) / (precision + recall)
    return f1


def f1_multi(prediction: str, ground_truth: str) -> float:
    """多答案 F1（按逗号分割，对每个 gt 取最大 F1 后求均值）。"""
    predictions = [p.strip() for p in prediction.split(",")]
    ground_truths = [g.strip() for g in ground_truth.split(",")]
    return float(np.mean([max([f1_score(pred, gt) for pred in predictions]) for gt in ground_truths]))


def eval_question(qa_item: dict, prediction: str) -> float:
    """按 category 评分，返回 F1 分数（0-1）。

    - category 1: 多跳，按逗号分割子答案
    - category 2/3/4: 单跳/时序/开放域，单答案 F1
    - category 3: 取分号前第一部分
    - category 5: abstention，检测是否说"no information available"或"not mentioned"
    """
    answer = str(qa_item["answer"])
    category = qa_item["category"]
    if category == 3:
        answer = answer.split(";")[0].strip()
    if category in [2, 3, 4]:
        return f1_score(prediction, answer)
    elif category == 1:
        return f1_multi(prediction, answer)
    elif category == 5:
        if "no information available" in prediction.lower() or "not mentioned" in prediction.lower():
            return 1.0
        else:
            return 0.0
    else:
        return 0.0


# ---------------------------------------------------------------------------
# LoCoMo session 提取
# ---------------------------------------------------------------------------
def get_locomo_sessions(conversation: dict) -> list[tuple[str, list[dict]]]:
    """从 conversation 提取所有 session_N 及其 date_time。

    返回 [(date_time_str, session_turns), ...]，按 session 编号顺序。
    """
    sessions: list[tuple[str, list[dict]]] = []
    # 找出所有 session_N 键（排除 session_N_date_time）
    session_keys = sorted(
        [k for k in conversation if re.match(r"^session_\d+$", k)],
        key=lambda x: int(x.split("_")[1]),
    )
    for sk in session_keys:
        dk = sk + "_date_time"
        date_time = conversation.get(dk, "")
        turns = conversation[sk]
        if turns:  # 跳过空 session
            sessions.append((date_time, turns))
    return sessions


def locomo_session_to_turns(session_turns: list[dict], speaker_a: str, timestamp: str) -> list[dict]:
    """将 LoCoMo 的一个 session 转为 MessageTurn 列表。

    每对 speaker_a+speaker_b 消息合并为 1 个 MessageTurn：
    - user_message = speaker_a 的 text
    - llm_message = speaker_b 的 text
    """
    turns: list[dict] = []
    i = 0
    while i < len(session_turns):
        cur = session_turns[i]
        if cur["speaker"] == speaker_a:
            user_text = cur["text"]
            llm_text = ""
            if i + 1 < len(session_turns) and session_turns[i + 1]["speaker"] != speaker_a:
                llm_text = session_turns[i + 1]["text"]
                i += 2
            else:
                # 连续 speaker_a，先归到 user，llm 留空
                i += 1
                continue
            turns.append(make_message_turn(user_text, llm_text, timestamp))
        else:
            # speaker_b 开头（无配对 speaker_a），跳过
            i += 1
    return turns


def locomo_build_baseline_messages(sample: dict, question: str) -> list[dict]:
    """构造 baseline 的完整 messages 列表。

    - system: 固定 prompt
    - 所有 session 拼接，speaker_a → user, speaker_b → assistant
    - 末尾追加 question
    """
    conv = sample["conversation"]
    speaker_a = conv["speaker_a"]
    speaker_b = conv["speaker_b"]
    system_prompt = (
        "You are a helpful assistant. Below is a conversation between two people. "
        "Based on the conversation, answer the question."
    )
    messages: list[dict] = [{"role": "system", "content": system_prompt}]
    for _date, session_turns in get_locomo_sessions(conv):
        for turn in session_turns:
            role = "user" if turn["speaker"] == speaker_a else "assistant"
            messages.append({"role": role, "content": turn["text"]})
    messages.append({"role": "user", "content": question})
    return messages


def locomo_run_memory_center(sample: dict, model_short: str) -> tuple[str, str]:
    """执行 memory_center 条件的归档 + 拉取摘要和完整内容，返回 (summary_prompt, full_content)。

    - session_id = f"locomo-{sample_id}-{model_short}"
    - 每个 session_N 归档为 1 个 daily 文件（幂等：已有则跳过）
    """
    sample_id = sample["sample_id"]
    session_id = f"locomo-{sample_id}-{model_short}"
    conv = sample["conversation"]
    speaker_a = conv["speaker_a"]

    # 幂等检查
    existing = mc_get_summaries(session_id)
    if not existing:
        sessions = get_locomo_sessions(conv)
        for date_str, session_turns in sessions:
            ts = parse_locomo_timestamp(date_str)
            turns = locomo_session_to_turns(session_turns, speaker_a, ts)
            if turns:
                mc_archive(session_id, turns)

    summary_prompt = mc_get_prompt(session_id)
    full_content = mc_retrieve_all_content(session_id)
    return summary_prompt, full_content


# ---------------------------------------------------------------------------
# 单 sample 评测
# ---------------------------------------------------------------------------
def evaluate_sample(
    sample: dict,
    model_name: str,
    condition: str,
    qa_limit: int | None = None,
) -> list[dict]:
    """评测单个 sample 的所有 QA，返回 JSONL 行列表。"""
    sample_id = sample["sample_id"]
    model_short = MODEL_SHORT.get(model_name, model_name)
    qas = sample["qa"]
    if qa_limit is not None:
        qas = qas[:qa_limit]

    results: list[dict] = []

    # memory_center 条件：先归档一次，所有 QA 共享同一份记忆摘要
    mc_summary = ""
    mc_content = ""
    if condition == "memory_center":
        try:
            mc_summary, mc_content = locomo_run_memory_center(sample, model_short)
        except Exception as e:  # noqa: BLE001
            # 归档失败，所有 QA 标记 error
            for idx, qa in enumerate(qas):
                results.append({
                    "sample_id": sample_id,
                    "qa_index": idx,
                    "model": model_name,
                    "condition": condition,
                    "category": qa["category"],
                    "question": qa["question"],
                    "answer": qa["answer"],
                    "hypothesis": "",
                    "f1": None,
                    "error": f"memory_center archive/prompt failed: {type(e).__name__}: {e}",
                })
            return results

    # 评测每个 QA
    model_cfg = get_model_config(model_name)
    for idx, qa in enumerate(qas):
        result = {
            "sample_id": sample_id,
            "qa_index": idx,
            "model": model_name,
            "condition": condition,
            "category": qa["category"],
            "question": qa["question"],
            "answer": qa["answer"],
            "hypothesis": "",
            "f1": None,
            "error": None,
        }
        try:
            t0 = time.time()
            # 构造 messages
            if condition == "baseline":
                messages = locomo_build_baseline_messages(sample, qa["question"])
            elif condition == "memory_center":
                system_prompt = (
                    mc_summary
                    + "\n\n---\n\n以下是完整的对话记录：\n\n"
                    + mc_content
                    + "\n\n---\n\n"
                    + "Based on the memory and conversation history above, answer the user's question. "
                    + "Do NOT attempt to call any tools or request memory retrieval. "
                    + "Answer directly using only the information provided above."
                )
                messages = [
                    {"role": "system", "content": system_prompt},
                    {"role": "user", "content": qa["question"]},
                ]
            else:
                raise ValueError(f"未知 condition: {condition}")

            print(f"  [{model_name}/{condition}] sample={sample_id} qa={idx}/{len(qas)} calling LLM...", flush=True)
            hypothesis = call_llm(model_cfg, messages, temperature=0)
            dt = time.time() - t0
            print(f"  [{model_name}/{condition}] sample={sample_id} qa={idx} done in {dt:.1f}s, len={len(hypothesis)}", flush=True)
            result["hypothesis"] = hypothesis
            result["f1"] = eval_question(qa, hypothesis)

        except Exception as e:  # noqa: BLE001 - 单题失败不阻塞
            result["error"] = f"{type(e).__name__}: {e}"

        results.append(result)

    return results


# ---------------------------------------------------------------------------
# 统计报告
# ---------------------------------------------------------------------------
def compute_stats(records: list[dict]) -> dict:
    """按 model×condition×category 聚合 F1。"""
    bucket: dict[str, dict[str, dict[int, list[float]]]] = defaultdict(lambda: defaultdict(lambda: defaultdict(list)))
    for r in records:
        if r.get("f1") is None:
            continue  # 跳过失败项
        bucket[r["model"]][r["condition"]][r["category"]].append(r["f1"])

    stats: dict = {}
    for model, conds in bucket.items():
        stats[model] = {}
        for cond, cats in conds.items():
            stats[model][cond] = {}
            all_scores: list[float] = []
            for cat in sorted(cats.keys()):
                scores = cats[cat]
                stats[model][cond][f"category_{cat}"] = {
                    "f1": round(float(np.mean(scores)), 4),
                    "count": len(scores),
                }
                all_scores.extend(scores)
            if all_scores:
                stats[model][cond]["overall"] = {
                    "f1": round(float(np.mean(all_scores)), 4),
                    "count": len(all_scores),
                }
    return stats


# ---------------------------------------------------------------------------
# 主流程
# ---------------------------------------------------------------------------
def main() -> int:
    parser = argparse.ArgumentParser(description="LoCoMo 评测：baseline vs memory_center")
    parser.add_argument("--n-samples", type=int, default=10, help="评测 sample 数（默认 10）")
    parser.add_argument("--models", type=str, default=",".join(DEFAULT_MODELS), help="逗号分隔的模型列表")
    parser.add_argument("--conditions", type=str, default=",".join(DEFAULT_CONDITIONS), help="逗号分隔的条件列表")
    parser.add_argument("--smoke-test", action="store_true", help="1 sample + 每 sample 前 3 个 QA")
    parser.add_argument("--qa-limit", type=int, default=None, help="每 sample 评测的 QA 数（默认全部）")
    args = parser.parse_args()

    models = [m.strip() for m in args.models.split(",") if m.strip()]
    conditions = [c.strip() for c in args.conditions.split(",") if c.strip()]
    n_samples = 1 if args.smoke_test else args.n_samples
    qa_limit = 3 if args.smoke_test else args.qa_limit

    # 加载数据
    print(f"[1/4] 加载 {DATA_FILE.name} ...")
    data = json.loads(DATA_FILE.read_text(encoding="utf-8"))
    print(f"      共 {len(data)} 个 sample")

    samples = data[:n_samples]
    print(f"[2/4] 评测 {len(samples)} 个 sample" + (f"，每 sample 前 {qa_limit} 个 QA" if qa_limit else ""))

    print(f"[3/4] 模型: {models}, 条件: {conditions}")

    # 评测循环
    print(f"[4/4] 开始评测 ...")
    all_records: list[dict] = []

    # 预压缩所有 JSONL 文件（移除失败条目，防止重试产生重复）
    for model_name in models:
        for cond in conditions:
            out_path = RESULTS_DIR / f"locomo_{model_name}_{cond}.jsonl"
            compact_jsonl(out_path)

    # 总进度：sample × 模型 × 条件（每个 unit 内部有多个 QA）
    total_units = len(samples) * len(models) * len(conditions)
    pbar = tqdm(total=total_units, desc="LoCoMo", unit="sample×model×cond")

    for sample in samples:
        sample_id = sample["sample_id"]
        for model_name in models:
            for cond in conditions:
                out_path = RESULTS_DIR / f"locomo_{model_name}_{cond}.jsonl"
                done = load_completed_keys(out_path, key_field="sample_id", extra_key_field="qa_index")

                # 判断该 sample 的所有 QA 是否都已完成
                # 先收集已完成的 QA 数
                done_count = 0
                for qa_idx in range(len(sample["qa"])):
                    if (sample_id, qa_idx) in done:
                        done_count += 1

                qa_to_run = len(sample["qa"]) if qa_limit is None else min(qa_limit, len(sample["qa"]))

                if done_count >= qa_to_run and done_count > 0:
                    # 已完成，读取记录用于统计
                    for line in out_path.read_text(encoding="utf-8").splitlines():
                        line = line.strip()
                        if not line:
                            continue
                        try:
                            entry = json.loads(line)
                        except json.JSONDecodeError:
                            continue
                        if (
                            entry.get("sample_id") == sample_id
                            and not entry.get("error")
                            and entry.get("hypothesis")
                        ):
                            all_records.append(entry)
                    pbar.update(1)
                    continue

                # 执行评测
                t0 = time.time()
                results = evaluate_sample(sample, model_name, cond, qa_limit=qa_limit)
                dt = time.time() - t0

                # 写入 JSONL + 收集记录
                ok_count = sum(1 for r in results if r["f1"] is not None)
                err_count = sum(1 for r in results if r["error"])
                avg_f1 = float(np.mean([r["f1"] for r in results if r["f1"] is not None])) if ok_count else 0
                tqdm.write(
                    f"  [{model_name}/{cond}] sample={sample_id} "
                    f"ok={ok_count} err={err_count} avg_f1={avg_f1:.3f} {dt:.1f}s"
                )
                for r in results:
                    append_jsonl(out_path, r)
                    if not r["error"]:
                        all_records.append(r)

                pbar.update(1)

    pbar.close()

    # 统计
    stats = compute_stats(all_records)
    report_path = save_summary_report(stats, name="locomo_summary")
    print(f"\n=== LoCoMo 统计 ===")
    print(json.dumps(stats, ensure_ascii=False, indent=2))
    print(f"\n报告已保存: {report_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
