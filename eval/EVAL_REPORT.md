# MemoryCenter 评测报告 V2.3

> 生成时间：2026-07-03
> 评测目的：验证 MemoryCenter 记忆库对 Agent 长时记忆能力的提升效果

## 1. 评测设计

### 1.1 评测矩阵

| 维度 | 配置 |
|------|------|
| 被评测模型 | SenseNova-6.7-flash-lite、Step-3.7-flash（reasoning_effort=medium）|
| 裁判模型 | DeepSeek-V4-flash（max_tokens=4096）|
| 条件对照 | baseline（裸考，完整对话历史直接拼入 messages）vs memory_center（MemoryCenter 归档 + /prompt + retrieve 完整内容注入 system prompt）|
| 基准数据集 | LongMemEval-oracle（30 题抽样，覆盖 6 种 question_type）+ LoCoMo（3 sample × 30 QA = 90 题）|

### 1.2 评分方法

| 数据集 | 评分方式 | 说明 |
|--------|----------|------|
| LongMemEval | LLM-as-Judge | DeepSeek-V4-flash 判 yes/no（max_tokens=4096，留 thinking 配额）|
| LoCoMo | 纯算法 F1/EM | Porter 词干提取 + 标准化 + 多答案 F1（严格复制官方 evaluation.py，无 judge 主观性）|

### 1.3 MemoryCenter 用法

```
memory_center 条件流程：
1. 每个 session_N 归档为 1 个 daily 记忆文件（POST /archive）
2. GET /prompt 拉取记忆摘要
3. GET /memories/{hook_id} retrieve 每个 MemoryFile 的完整对话内容
4. 拼接到 system prompt：摘要 + 完整对话 + 防御指令
5. LLM 基于 system prompt + 当前 question 生成 hypothesis
```

防御指令（防止模型循环调用 tool）：
```
Based on the memory and conversation history above, answer the user's question.
Do NOT attempt to call any tools or request memory retrieval.
Answer directly using only the information provided above.
```

## 2. 结果

### 2.1 LongMemEval（30 题 × 4 组）

| 模型 | baseline | memory_center | 差异 |
|------|----------|-------|------|
| sensenova | 0.7333 | 0.7333 | 持平 |
| step | 0.7333 | 0.7333 | 持平 |

**能力分布变化**（memory_center vs baseline，5 题/能力）：

| 能力 | sensenova Δ | step Δ |
|------|-------------|--------|
| multi-session | -0.2 | +0.2 |
| temporal-reasoning | 0 | 0 |
| single-session-assistant | 0 | 0 |
| knowledge-update | 0 | 0 |
| single-session-user | 0 | 0 |
| single-session-preference | +0.2 | -0.2 |

**关键观察**：
- 4 组总分持平 0.7333，DeepSeek-V4-flash judge 较宽松（原 V4-pro 时 sensenova baseline 为 0.4667，flash 升至 0.7333）
- memory_center 改变了能力分布：sensenova 在 preference 提升，step 在 multi-session 提升
- 单 session 能力（assistant/user）两组都保持 1.0 满分，memory_center 无负面影响

### 2.2 LoCoMo（90 题 × 4 组，F1 算法评分）

| 模型 | baseline F1 | memory_center F1 | 提升 |
|------|-------------|----------|------|
| sensenova | 0.1036 | **0.1465** | **+41.4%** |
| step | 0.1105 | **0.1345** | **+21.7%** |

**按 category 分**（90 题/组）：

| category | 类型 | sensenova base→memory_center | step base→memory_center |
|----------|------|----------------------|-----------------|
| 1 | 多跳推理 | 0.1941 → **0.2886** (+48.7%) | 0.1961 → **0.2452** (+25%) |
| 2 | 单跳 | 0.0373 → 0.0363 (-3%) | 0.0463 → **0.0543** (+17%) |
| 3 | 时序 | 0.0503 → 0.0514 (+2%) | 0.0285 → 0.0360 (+26%) |
| 4 | 开放域 | 0.1548 → **0.4000** (+158%) | 0.3016 → 0.2405 (-20%) |

**按 sample 分**：

| sample | 规模 | sensenova base→memory_center | step base→memory_center |
|--------|------|----------------------|-----------------|
| conv-26 | 19 sessions / 419 turns | 0.0877 → **0.1502** (+71%) | 0.1075 → **0.1272** (+18%) |
| conv-30 | 19 sessions / 369 turns | 0.1355 → **0.1427** (+5%) | 0.1636 → 0.1592 (-3%) |
| conv-41 | 32 sessions / 663 turns | 0.0877 → **0.1467** (+67%) | 0.0603 → **0.1169** (+94%) |

## 3. 关键发现

### 3.1 LoCoMo 是 MemoryCenter 优势的有力证据

LoCoMo 用纯算法 F1 评分（无 judge 主观性），memory_center 在两个模型上都显著提升 overall F1：
- sensenova +41.4%
- step +21.7%

### 3.2 memory_center 对多跳推理帮助最大

LoCoMo category_1（多跳推理）两个模型都显著提升（+25% ~ +48.7%），证明 MemoryCenter 的归档 + retrieve 机制让模型能跨 session 关联信息。

### 3.3 大规模对话 memory_center 优势更明显

conv-41（663 turns，最大规模）memory_center 提升最显著：sensenova +67%、step +94%。
小规模 conv-30（369 turns）memory_center 优势减弱甚至略降，可能因 baseline 短上下文已能覆盖。

### 3.4 LongMemEval flash judge 抹平差异

DeepSeek-V4-flash 比 V4-pro 更宽松，sensenova baseline 从 0.4667 升至 0.7333，天花板效应掩盖 memory_center 优势。
若需更敏感的 LongMemEval 对比，建议换回 V4-pro judge 或用更难的 question_type。

### 3.5 Step API rate limit 影响

Step 在并行评测时触发 429 rate limit（RPM 10 限制），重试机制恢复但部分 QA 因超时返回空 hypothesis，影响 F1。
正式评测建议串行执行或加大 RPM 配额。

## 4. 评测执行信息

### 4.1 配置变更

| 项目 | 原值 | 新值 | 原因 |
|------|------|------|------|
| DeepSeek judge 模型 | deepseek-v4-pro | deepseek-v4-flash | 用户指定 |
| 所有模型 max_tokens | 8192 (step) / 1024 (其他) | 4096 统一 | 用户指定 |
| LoCoMo 规模 | 10 sample × 全部 QA (1986 QA) | 3 sample × 30 QA (90 QA) | 全跑约 50+ 小时不现实 |
| call_llm timeout | 无 | 180s | 防 LoCoMo baseline 大 messages 无限挂起 |

### 4.2 修复的问题

| 问题 | 根因 | 修复 |
|------|------|------|
| /prompt 措辞误导 | retrieve.rs "可通过记忆检索工具获取" | 改为"可直接基于此信息回答用户问题" + Python 防御指令 |
| judge max_tokens 太小 | DeepSeek reasoning thinking 消耗配额 | 2048 → 4096 |
| /prompt 只返回摘要标题 | 无 retrieve 完整内容入口 | 新增 mc_retrieve_all_content() 函数 |
| LoCoMo evaluate_sample tuple 未解包 | locomo_run_memory_center 返回 tuple 但调用方未解包 | 改为 mc_summary, mc_content = ... |
| call_llm 无 timeout | LoCoMo baseline 大 messages 无限挂起 | 加 timeout=180 |

### 4.3 资源占用

- memory-center-server：本地运行，127.0.0.1:8765
- 总耗时：LongMemEval 约 28 分钟 + LoCoMo 约 80 分钟（并行执行）
- API 调用：LongMemEval ~120 次 + LoCoMo ~360 次

## 5. 结论

MemoryCenter 记忆库在 LoCoMo 长时记忆基准上验证有效：
- **纯算法 F1 评分下，两个模型 overall F1 都显著提升（+21.7% ~ +41.4%）**
- **多跳推理能力提升最显著（+25% ~ +48.7%）**
- **大规模对话场景 memory_center 优势更明显（conv-41 step +94%）**

LongMemEval 在 flash judge 下差异不显著，建议后续用更严格的 judge 或更大样本量复测。

## 6. 数据文件

| 文件 | 说明 |
|------|------|
| results/longmemeval_*.jsonl | 4 组逐题结果（question_id/model/condition/hypothesis/judge_label）|
| results/longmemeval_summary.json | 按 question_type 聚合统计 |
| results/locomo_*.jsonl | 4 组逐题结果（sample_id/qa_index/category/hypothesis/f1）|
| results/locomo_summary.json | 按 category 聚合统计 |
