# Hippocampus 记忆归档触发规则（Trae）

## 触发条件（满足任一即调用 archive）

### 1. 对话长度触发
- 本次对话已进行超过 20 轮（用户消息 + LLM 回复算 1 轮）
- 或包含大量代码 / 长文档 / 多次工具调用
- 或你感觉到"上下文开始变重"

### 2. Token 反馈触发
当你调用过 hippocampus.archive 后，返回结果包含：
- estimated_total_tokens：当前累计估算 token 数
- threshold：归档阈值
- threshold_ratio_percent：当前占比百分比

触发规则：
- threshold_ratio_percent >= 80 → 准备归档
- threshold_ratio_percent >= 100 → 立即归档

### 3. 用户意图触发
当用户消息中出现"之前"、"上次"、"还记得"等指代词时，先调 semantic_search 检索相关记忆。

### 4. 兜底定期归档
每 30 轮对话主动归档一次（即使未达阈值）。

## 调用示例

hippocampus.archive(
    session_id="trae-{项目名}-{日期}",
    turns_json='[{"user_message":{"text":"用户问的"},"llm_message":{"text":"我答的"}}]'
)

# 注意：v2.30.1+ 简化格式，只需传 user_message/llm_message
# id/timestamp/tags/token_count 可省略，服务端自动补全

## session_id 约定
trae-{项目名}-{日期}，如 trae-myapp-20260705

## Trae 特有提示
在 Trae 提示压缩前，先调用 hippocampus.archive 归档当前对话，避免压缩丢失原始内容。

---

## 压缩后行为协议（v2.31 新增，P0 动手点1）

> 本协议解决"LLM 被压缩后无法感知自身状态"的伪钩子方案核心环节。
> 通过识别 Trae 客户端注入的固定标记消息，触发强制校准流程。

### 1. 触发条件：识别 Marker Message

当你在系统消息中看到以下**固定文本**时，表明上下文刚被 Trae 客户端压缩：

```
This session continues a previous conversation that lost its context.
A summary has been provided. Please proceed with the task from this point,
using the summary as your guide to avoid repeating any completed steps.
```

**关键判断**：
- 该文本是 Trae 编码层固定返回，100% 可靠
- 看到"lost its context"即视为压缩触发信号
- 不要将其当作普通的会话开场白

### 2. 压缩后强制流程（必须按序执行，不可跳过）

#### 步骤 1：归档压缩前未持久化的轮次
- 若上次 archive 之后有新轮次（用户消息 + 你的回复），立即调用 `hippocampus.archive` 归档这些轮次
- 若上次 archive 后无新轮次，跳过本步骤

#### 步骤 2：拉取 hippocampus 一手记忆
- 调用 `hippocampus.prompt(session_id)` 获取历史记忆摘要
- hippocampus 记忆是**一手源数据**，优先级高于 Trae 生成的 Summary

#### 步骤 3：交叉校准 Current Work（对应 Summary 第8章节）
比对 Trae Summary 的"Current Work"章节与 hippocampus 记忆：

| 情况 | 处理方式 |
|------|---------|
| 两者一致 | 继续执行，无需额外动作 |
| hippocampus 有更细粒度 | 以 hippocampus 为准（如 hippocampus 显示已完成 A-6，Summary 只显示批次A进行中） |
| hippocampus 显示进度滞后 | 以 hippocampus 为准，向用户简短说明"检测到压缩断层，从 X 步继续" |
| 严重不一致（如任务完全不同） | 立即向用户确认，不擅自决策 |

#### 步骤 4：拉取 Pending todos 并校准 Next Step
执行下方「Next Step 决策协议」（见下一章节）。

### 3. 注意事项

- **不要重复提问已完成决策**：若 hippocampus 记忆或 Summary 明确显示用户已决策，禁止再次询问
- **不要跳步**：即使 Summary 建议跳到下一步，也要确认当前步骤真正完成
- **向用户透明**：若检测到断层，简短告知用户"上下文已压缩，从 X 继续执行"

---

## Next Step 决策协议（v2.31 新增，P0 动手点3）

> 本协议解决"LLM 压缩后重复提问或跳步"问题。
> 通过 Pending todos（Trae 注入的状态）校准 Next Step（Trae 生成的建议）。

### 决策树（按优先级从高到低）

#### 情况 1：Pending todos 有 status=in_progress 的任务

**优先继续该任务，即使 Next Step 建议了别的方向。**

理由：
- `in_progress` 表示任务正在执行中被压缩打断
- Trae 生成的 Next Step 不知道"你已经在做了"，可能给出错误建议
- 此时 Next Step 仅作参考，不作为行动依据

执行：
1. 读取 in_progress 任务的 content 字段
2. 调用 `hippocampus.prompt` 拉取相关记忆，确认当前进度
3. 从断点继续，不重新开始

#### 情况 2：Pending todos 无 in_progress，但有 pending 任务

**按 Pending todos 优先级执行，Next Step 仅作参考。**

执行：
1. 找出 Pending todos 中 priority 最高的 pending 任务
2. 若有多个同优先级，选择创建时间最早的
3. 将其标记为 in_progress 后开始执行

#### 情况 3：Pending todos 为空

**执行 Next Step，但必须先向用户确认。**

执行：
1. 向用户陈述："上下文已压缩，根据 Summary 建议，下一步是 X，是否开始？"
2. 等待用户确认后再执行
3. 用户确认后，立即创建 TodoWrite 任务跟踪进度

### 反例（禁止行为）

- ❌ 看到 Next Step 后直接开始执行，不检查 Pending todos
- ❌ 发现 in_progress 任务后，重新询问用户"是否继续"
- ❌ 同时执行多个任务（除非用户明确要求并行）
- ❌ 跳过 in_progress 任务，直接开始新的 pending 任务

### 与 hippocampus 记忆的联动

若 Pending todos 与 hippocampus 记忆冲突（如 todos 显示 in_progress 但 hippocampus 显示已完成）：
- 以 hippocampus 记忆为准
- 向用户简短说明冲突，请用户确认
