"""LongMemEval 评测脚本：baseline（裸考）vs memory_center（记忆摘要）对比。

流程：
1. 加载 longmemeval_oracle.json（500 题）
2. 分层抽样 N 题（seed=42，覆盖所有 question_type）
3. 对每题×每个模型×每个条件，生成 hypothesis：
   - baseline：原始 haystack 作为对话历史，直接问 LLM
   - memory_center：每 session 归档为 1 个 daily 记忆 → /prompt 拉摘要 → 注入 system prompt
4. DeepSeek-V4-Pro 做 LLM-as-judge（temperature=0, max_tokens=10）
5. 输出 JSONL + 统计报告

用法：
  python run_longmemeval.py                      # 默认 30 题，2 模型 × 2 条件
  python run_longmemeval.py --smoke-test         # 只跑 1 题，验证流程
  python run_longmemeval.py --n-questions 50 --models sensenova
"""
from __future__ import annotations

import argparse
import json
import random
import sys
import time
from collections import defaultdict
from pathlib import Path
from typing import Any

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
    parse_lme_timestamp,
    save_summary_report,
)

# ---------------------------------------------------------------------------
# 路径
# ---------------------------------------------------------------------------
DATA_FILE = Path(__file__).resolve().parent.parent / "LongMemEval" / "data" / "longmemeval_oracle.json"


# ---------------------------------------------------------------------------
# judge prompt（严格复制自官方 evaluate_qa.py）
# ---------------------------------------------------------------------------
def get_anscheck_prompt(task: str, question: str, answer: str, response: str, abstention: bool = False) -> str:
    if not abstention:
        if task in ["single-session-user", "single-session-assistant", "multi-session"]:
            template = (
                "I will give you a question, a correct answer, and a response from a model. "
                "Please answer yes if the response contains the correct answer. Otherwise, answer no. "
                "If the response is equivalent to the correct answer or contains all the intermediate steps "
                "to get the correct answer, you should also answer yes. "
                "If the response only contains a subset of the information required by the answer, answer no. \n\n"
                "Question: {}\n\nCorrect Answer: {}\n\nModel Response: {}\n\n"
                "Is the model response correct? Answer yes or no only."
            )
            return template.format(question, answer, response)
        elif task == "temporal-reasoning":
            template = (
                "I will give you a question, a correct answer, and a response from a model. "
                "Please answer yes if the response contains the correct answer. Otherwise, answer no. "
                "If the response is equivalent to the correct answer or contains all the intermediate steps "
                "to get the correct answer, you should also answer yes. "
                "If the response only contains a subset of the information required by the answer, answer no. "
                "In addition, do not penalize off-by-one errors for the number of days. "
                "If the question asks for the number of days/weeks/months, etc., and the model makes "
                "off-by-one errors (e.g., predicting 19 days when the answer is 18), the model's response is still correct. \n\n"
                "Question: {}\n\nCorrect Answer: {}\n\nModel Response: {}\n\n"
                "Is the model response correct? Answer yes or no only."
            )
            return template.format(question, answer, response)
        elif task == "knowledge-update":
            template = (
                "I will give you a question, a correct answer, and a response from a model. "
                "Please answer yes if the response contains the correct answer. Otherwise, answer no. "
                "If the response contains some previous information along with an updated answer, "
                "the response should be considered as correct as long as the updated answer is the required answer.\n\n"
                "Question: {}\n\nCorrect Answer: {}\n\nModel Response: {}\n\n"
                "Is the model response correct? Answer yes or no only."
            )
            return template.format(question, answer, response)
        elif task == "single-session-preference":
            template = (
                "I will give you a question, a rubric for desired personalized response, and a response from a model. "
                "Please answer yes if the response satisfies the desired response. Otherwise, answer no. "
                "The model does not need to reflect all the points in the rubric. "
                "The response is correct as long as it recalls and utilizes the user's personal information correctly.\n\n"
                "Question: {}\n\nRubric: {}\n\nModel Response: {}\n\n"
                "Is the model response correct? Answer yes or no only."
            )
            return template.format(question, answer, response)
        else:
            raise NotImplementedError(f"未知 task: {task}")
    else:
        template = (
            "I will give you an unanswerable question, an explanation, and a response from a model. "
            "Please answer yes if the model correctly identifies the question as unanswerable. "
            "The model could say that the information is incomplete, or some other information is given "
            "but the asked information is not.\n\n"
            "Question: {}\n\nExplanation: {}\n\nModel Response: {}\n\n"
            "Does the model correctly identify the question as unanswerable? Answer yes or no only."
        )
        return template.format(question, answer, response)


# ---------------------------------------------------------------------------
# 抽样：分层覆盖所有 question_type
# ---------------------------------------------------------------------------
def stratified_sample(data: list[dict], n: int, seed: int = 42) -> list[dict]:
    """分层抽样确保覆盖所有 question_type。

    策略：按 question_type 分组，每组均匀分配名额（至少 1 个），组内随机抽取。
    """
    rng = random.Random(seed)
    by_type: dict[str, list[dict]] = defaultdict(list)
    for item in data:
        by_type[item["question_type"]].append(item)

    types = sorted(by_type.keys())
    n_types = len(types)
    per_type = max(1, n // n_types)  # 每类至少 1 个

    sampled: list[dict] = []
    for t in types:
        pool = list(by_type[t])
        rng.shuffle(pool)
        sampled.extend(pool[:per_type])

    # 若超出 n，随机裁剪；若不足 n，从剩余池补
    if len(sampled) > n:
        rng.shuffle(sampled)
        sampled = sampled[:n]
    elif len(sampled) < n:
        # 从未抽中的题目里补
        sampled_ids = {s["question_id"] for s in sampled}
        remaining = [d for d in data if d["question_id"] not in sampled_ids]
        rng.shuffle(remaining)
        sampled.extend(remaining[: n - len(sampled)])

    rng.shuffle(sampled)
    return sampled


# ---------------------------------------------------------------------------
# LongMemEval session → MessageTurn 转换
# ---------------------------------------------------------------------------
def lme_session_to_turns(session: list[dict], timestamp: str) -> list[dict]:
    """将 LongMemEval 的一个 session（list[turn]）转为 MessageTurn 列表。

    每对 user+assistant turn 合并为 1 个 MessageTurn。
    若 user 后没有 assistant，跳过该孤立的 user turn。
    """
    turns: list[dict] = []
    i = 0
    while i < len(session):
        cur = session[i]
        if cur["role"] == "user":
            # 找紧随其后的 assistant
            user_text = cur["content"]
            llm_text = ""
            if i + 1 < len(session) and session[i + 1]["role"] == "assistant":
                llm_text = session[i + 1]["content"]
                i += 2
            else:
                # 孤立 user turn，跳过
                i += 1
                continue
            turns.append(make_message_turn(user_text, llm_text, timestamp))
        else:
            # assistant 开头（无配对 user），跳过
            i += 1
    return turns


def lme_build_baseline_messages(item: dict) -> list[dict]:
    """构造 baseline 的完整 messages 列表。

    - system: 固定 prompt
    - 按时间排序的 haystack 各 turn（role: user/assistant）
    - 末尾追加 question 作为 user message
    """
    system_prompt = (
        "You are a helpful chat assistant. Below are the chat history between you and a user. "
        "Please answer the user's question based on the chat history."
    )
    # 按时间排序 sessions
    sessions_with_dates = list(zip(item["haystack_dates"], item["haystack_sessions"]))
    # 解析时间用于排序
    def _sort_key(sd: tuple) -> str:
        return sd[0]  # 原始字符串按字典序大致可排序，进一步用解析后的 ISO
    sessions_with_dates.sort(key=_sort_key)

    messages: list[dict] = [{"role": "system", "content": system_prompt}]
    for _date, session in sessions_with_dates:
        for turn in session:
            role = turn["role"]  # 'user' or 'assistant'
            messages.append({"role": role, "content": turn["content"]})
    # 最后追加问题
    messages.append({"role": "user", "content": item["question"]})
    return messages


def lme_run_memory_center(item: dict, model_short: str) -> tuple[list[dict], str]:
    """执行 memory_center 条件的归档 + 拉取摘要，返回 (messages, summary_prompt)。

    - session_id = f"lme-{question_id}-{model_short}"
    - 每个 haystack_session 归档为 1 个 daily 文件（幂等：已有则跳过）
    - 拉取 /prompt 得到记忆摘要文本
    - 构造 messages：system=摘要+指令, user=question
    """
    question_id = item["question_id"]
    session_id = f"lme-{question_id}-{model_short}"

    # 幂等检查：若已有摘要，直接拉 prompt
    existing = mc_get_summaries(session_id)
    if not existing:
        # 需要归档：逐 session 归档
        for date_str, session in zip(item["haystack_dates"], item["haystack_sessions"]):
            ts = parse_lme_timestamp(date_str)
            turns = lme_session_to_turns(session, ts)
            if turns:
                mc_archive(session_id, turns)

    # 拉取记忆摘要 + 完整对话内容
    summary_prompt = mc_get_prompt(session_id)
    full_content = mc_retrieve_all_content(session_id)
    system_prompt = (
        summary_prompt
        + "\n\n---\n\n以下是完整的对话记录：\n\n"
        + full_content
        + "\n\n---\n\n"
        + "Based on the memory and conversation history above, answer the user's question. "
        + "Do NOT attempt to call any tools or request memory retrieval. "
        + "Answer directly using only the information provided above."
    )
    messages = [
        {"role": "system", "content": system_prompt},
        {"role": "user", "content": item["question"]},
    ]
    return messages, summary_prompt


# ---------------------------------------------------------------------------
# 单题评测
# ---------------------------------------------------------------------------
def evaluate_one(
    item: dict,
    model_name: str,
    condition: str,
    judge_cfg,
) -> dict:
    """评测单个 (question × model × condition) 组合，返回 JSONL 行。"""
    question_id = item["question_id"]
    qtype = item["question_type"]
    question = item["question"]
    answer = item["answer"]
    abstention = question_id.endswith("_abs")
    model_short = MODEL_SHORT.get(model_name, model_name)

    result = {
        "question_id": question_id,
        "model": model_name,
        "condition": condition,
        "question_type": qtype,
        "abstention": abstention,
        "hypothesis": "",
        "judge_label": None,
        "judge_raw": "",
        "error": None,
    }

    try:
        model_cfg = get_model_config(model_name)

        # 1. 生成 hypothesis
        if condition == "baseline":
            messages = lme_build_baseline_messages(item)
        elif condition == "memory_center":
            messages, _ = lme_run_memory_center(item, model_short)
        else:
            raise ValueError(f"未知 condition: {condition}")

        hypothesis = call_llm(model_cfg, messages, temperature=0)
        result["hypothesis"] = hypothesis

        # 2. judge 评分（DeepSeek-v4-flash reasoning 模型，thinking tokens 消耗配额，需 4096）
        judge_prompt = get_anscheck_prompt(qtype, question, answer, hypothesis, abstention=abstention)
        judge_raw = call_llm(judge_cfg, [{"role": "user", "content": judge_prompt}], temperature=0, max_tokens=4096)
        result["judge_raw"] = judge_raw
        result["judge_label"] = "yes" in judge_raw.lower()

    except Exception as e:  # noqa: BLE001 - 单题失败不阻塞
        result["error"] = f"{type(e).__name__}: {e}"

    return result


# ---------------------------------------------------------------------------
# 统计报告
# ---------------------------------------------------------------------------
def compute_stats(records: list[dict]) -> dict:
    """按 model×condition×question_type 聚合准确率。"""
    # 聚合: {model: {condition: {qtype: [0/1, ...]}}}
    bucket: dict[str, dict[str, dict[str, list[int]]]] = defaultdict(lambda: defaultdict(lambda: defaultdict(list)))
    for r in records:
        if r.get("judge_label") is None:
            continue  # 跳过失败项
        bucket[r["model"]][r["condition"]][r["question_type"]].append(1 if r["judge_label"] else 0)

    stats: dict[str, Any] = {}
    for model, conds in bucket.items():
        stats[model] = {}
        for cond, qtypes in conds.items():
            stats[model][cond] = {}
            all_scores: list[int] = []
            for qtype, scores in qtypes.items():
                acc = sum(scores) / len(scores) if scores else 0
                stats[model][cond][qtype] = {
                    "accuracy": round(acc, 4),
                    "count": len(scores),
                }
                all_scores.extend(scores)
            overall = sum(all_scores) / len(all_scores) if all_scores else 0
            stats[model][cond]["overall"] = {
                "accuracy": round(overall, 4),
                "count": len(all_scores),
            }
    return stats


# ---------------------------------------------------------------------------
# 主流程
# ---------------------------------------------------------------------------
def main() -> int:
    parser = argparse.ArgumentParser(description="LongMemEval 评测：baseline vs memory_center")
    parser.add_argument("--n-questions", type=int, default=30, help="抽样题数（默认 30）")
    parser.add_argument("--models", type=str, default=",".join(DEFAULT_MODELS), help="逗号分隔的模型列表")
    parser.add_argument("--conditions", type=str, default=",".join(DEFAULT_CONDITIONS), help="逗号分隔的条件列表")
    parser.add_argument("--smoke-test", action="store_true", help="只跑 1 题验证流程")
    parser.add_argument("--seed", type=int, default=42, help="随机种子")
    args = parser.parse_args()

    models = [m.strip() for m in args.models.split(",") if m.strip()]
    conditions = [c.strip() for c in args.conditions.split(",") if c.strip()]
    n_questions = 1 if args.smoke_test else args.n_questions

    # 加载数据
    print(f"[1/4] 加载 {DATA_FILE.name} ...")
    data = json.loads(DATA_FILE.read_text(encoding="utf-8"))
    print(f"      共 {len(data)} 题")

    # 抽样
    sampled = stratified_sample(data, n_questions, seed=args.seed)
    print(f"[2/4] 抽样 {len(sampled)} 题（覆盖 {len(set(d['question_type'] for d in sampled))} 种 question_type）")

    # judge 配置
    judge_cfg = get_model_config("deepseek")
    print(f"[3/4] judge: {judge_cfg.name} / {judge_cfg.model}")

    # 评测循环
    print(f"[4/4] 开始评测：{'×'.join(models)} × {'×'.join(conditions)}")
    all_records: list[dict] = []

    # 预压缩所有 JSONL 文件（移除失败条目，防止重试产生重复）
    for model_name in models:
        for cond in conditions:
            out_path = RESULTS_DIR / f"longmemeval_{model_name}_{cond}.jsonl"
            compact_jsonl(out_path)

    # 总进度：题数 × 模型 × 条件
    total_units = len(sampled) * len(models) * len(conditions)
    pbar = tqdm(total=total_units, desc="LongMemEval", unit="q")

    for item in sampled:
        qid = item["question_id"]
        for model_name in models:
            for cond in conditions:
                # 断点续传：读取已完成的主键
                out_path = RESULTS_DIR / f"longmemeval_{model_name}_{cond}.jsonl"
                done = load_completed_keys(out_path, key_field="question_id")
                if (qid,) in done:
                    # 已完成，读出来用于统计
                    for line in out_path.read_text(encoding="utf-8").splitlines():
                        line = line.strip()
                        if not line:
                            continue
                        try:
                            entry = json.loads(line)
                        except json.JSONDecodeError:
                            continue
                        if entry.get("question_id") == qid and not entry.get("error") and entry.get("hypothesis"):
                            all_records.append(entry)
                            break
                    pbar.update(1)
                    continue

                # 执行评测
                t0 = time.time()
                result = evaluate_one(item, model_name, cond, judge_cfg)
                dt = time.time() - t0

                status = "✓" if result["judge_label"] is not None else "✗"
                tqdm.write(
                    f"  {status} [{model_name}/{cond}] {qid} ({item['question_type']}) "
                    f"label={'Y' if result['judge_label'] else 'N' if result['judge_label'] is not None else '?'} "
                    f"{dt:.1f}s"
                    + (f" err={result['error']}" if result["error"] else "")
                )

                append_jsonl(out_path, result)
                if not result["error"]:
                    all_records.append(result)
                pbar.update(1)

    pbar.close()

    # 统计
    stats = compute_stats(all_records)
    report_path = save_summary_report(stats, name="longmemeval_summary")
    print(f"\n=== LongMemEval 统计 ===")
    print(json.dumps(stats, ensure_ascii=False, indent=2))
    print(f"\n报告已保存: {report_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
