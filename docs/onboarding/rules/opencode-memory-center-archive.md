# MemoryCenter 记忆归档触发规则（OpenCode）

> 本文件是源码 `crates/memory-center-mcp/src/lib.rs` 中 `OPENCODE_RULES_TEMPLATE` 常量的独立文档镜像。
> `install_rules` 工具安装时写入 `.opencode/rules/memory-center-archive.md`。

## 你的角色

你是接入了 MemoryCenter 记忆库的 OpenCode Agent。MemoryCenter 帮你保留长对话中的关键信息，
避免上下文压缩时丢失重要内容。

**真钩子归档机制（sidecar 自动处理，无需 LLM 介入）**：
1. **被动保存层（sidecar 全自动）**：`mc-sidecar` 进程轮询 OpenCode SQLite 的
   `session_message` 表，检测 `type='compaction'` 的新消息。压缩完成时自动调
   MemoryCenter `pre-compress` 端点，**增量归档**上次压缩到本次压缩之间的完整上下文。
   **你无需手动处理压缩前归档**，这是真钩子（非伪钩子）——基于 OpenCode 开源特性实现。
2. **主动召回层（你的职责）**：压缩后你需要主动调 `prompt` 拉取历史记忆，
   并在用户提到过去事件时调 `semantic_search` 检索。

## 1. 会话开始：调 prompt 获取历史记忆

每个新会话的**第一次回复前**，必须先调用：

```
mcp_memory-center.prompt(session_id)
```

把返回的历史记忆摘要拼接到 system prompt。若返回空列表，说明该 session 无历史记忆。

## 2. 上下文接近阈值：主动调 archive 归档

### 触发条件（满足任一即调用）

- **对话长度**：已进行超过 20 轮（用户消息 + LLM 回复算 1 轮）
- **内容复杂度**：包含大量代码 / 长文档 / 多次工具调用（累计内容超过 5000 字）
- **主观判断**：你感觉到"上下文开始变重"、"前面说过但记不清细节"
- **Token 反馈**：上次 archive 返回的 `threshold_ratio_percent >= 80`
- **兜底**：每 30 轮对话主动归档一次（即使未达阈值）

### 调用方式（简化格式）

```
mcp_memory-center.archive(
    session_id,                  // 必填
    turns_json                  // 必填，最简格式：[{"user_message":{"text":"..."},"llm_message":{"text":"..."}}]
    // id/timestamp/tags/token_count 可省略，服务端自动补全
)
```

### Token 反馈循环（核心机制）

归档后返回结果包含：
- `estimated_total_tokens`：当前累计估算 token 数
- `threshold`：归档阈值（如 120000 = 120K）
- `threshold_ratio_percent`：当前占比百分比
- `suggestion`：归档建议（人类可读）

**判断规则**：
- `threshold_ratio_percent >= 100` → **立即归档**，并提示用户触发上下文压缩
- `threshold_ratio_percent >= 80` → **准备归档**，主动提示用户"建议归档"
- `threshold_ratio_percent >= 50` → 继续对话，但注意跟踪累计值
- `< 50` → 继续对话

> 你（LLM）无需感知自身 token 消耗——归档由 sidecar 真钩子自动触发。
> 这里的 archive 调用是**补充手段**（可选），用于你在对话中主动归档重要节点。
> 与闭源 Agent（Trae/Cursor）的伪钩子方案不同，OpenCode 不依赖 LLM 自感知 token。

### OpenCode 压缩机制说明

OpenCode 有两种压缩方式（都走同一流程）：
1. **手动压缩**：用户输入 `/compact` 命令
2. **自动压缩**：`compactIfNeeded` 在上下文接近窗口上限时自动触发

压缩流程：`Compaction.Started` → LLM 生成摘要 → `Compaction.Ended`，
OpenCode 往 `session_message` 表插入一条 `type='compaction'` 的消息。

**sidecar 检测到这条新消息后**，自动读取上次压缩到本次压缩之间的完整上下文，
增量归档到 MemoryCenter。你无需在压缩前手动调 `pre_compress_hook`，
但可以在压缩后主动调 `prompt` 拉取记忆。

## 3. 用户提到过去事件：先调 semantic_search 再回复

当用户消息中出现"之前"、"上次"、"还记得"、"我们之前讨论的"、"之前那个方案"等
指代过去的词语时，**先调用**：

```
mcp_memory-center.semantic_search(query, session_id, top_k=5)
```

用用户原话作为 query，检索相关记忆。把检索结果作为上下文再回复用户。

## 4. 用户陈述与记忆可能矛盾：先调 detect_conflicts 检测

当用户陈述的事实与记忆中的记录可能冲突时（如用户说"我用的是 Python"但记忆里是 Rust），
**先调用**：

```
mcp_memory-center.detect_conflicts(session_id, hook_id, added_facts, revised_facts, deprecated_facts)
```

## 5. 上下文被压缩后：执行压缩后行为协议

当系统消息中出现以下固定文本时：

```
This session continues a previous conversation that lost its context.
```

表明 OpenCode 刚压缩了上下文，必须立即执行**压缩后行为协议**：

1. 归档压缩前未持久化的轮次（sidecar 通常已处理，但建议检查）
2. 调 `prompt` 拉取 MemoryCenter 一手记忆
3. 交叉校准 OpenCode 摘要与 MemoryCenter 记忆
4. 用 Pending todos 校准下一步建议

**核心原则**：
- MemoryCenter 记忆优先级 > OpenCode 压缩摘要
- in_progress 任务必须从断点继续，禁止重复提问已完成决策

## 6. project_memory 反向写入

完成开发阶段/关键架构决策/风险点时，调 `update_project_memory` 更新 project_memory.md。
拿到 full_content 后用 Write 工具写入 OpenCode 的 memory 文件夹。

## 7. session_id 约定

```
opencode-{项目名}-{日期}
```

示例：
- `opencode-myapp-20260705`
- `opencode-MemoryCenter-20260705`

> 一个 session_id 对应一个独立的记忆空间。同会话内复用同一 session_id，
> 切换项目或日期时换新 session_id。

## 8. 不要归档的情况

- 单次简单问答（如"这个变量什么意思"）
- 纯闲聊或问候
- 用户明确说"不用记"

## OpenCode MCP 配置参考

在 `opencode.jsonc`（或 `opencode.json` / `config.json`）中配置 MemoryCenter MCP server：

### 本地 stdio 模式（推荐）

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "memory-center": {
      "type": "local",
      "command": ["memory-center-mcp"],
      "environment": {
        "MEMORY_CENTER_ROOT": "/path/to/memory/data"
      }
    }
  },
  "instructions": [".opencode/rules/memory-center-archive.md"]
}
```

### 远程 Streamable HTTP 模式

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "memory-center": {
      "type": "remote",
      "url": "https://your-server/mcp",
      "headers": {
        "Authorization": "Bearer <API_KEY>"
      }
    }
  },
  "instructions": [".opencode/rules/memory-center-archive.md"]
}
```

> `instructions` 字段让 OpenCode 自动加载本 Rules 文件，注入 LLM system prompt。
> 若未配置 `instructions`，AGENTS.md 仍会被通用约定加载。

## 与其他工具配合

| 时机 | 工具 | 说明 |
|------|------|------|
| 会话第一次回复前 | `prompt` | 获取历史记忆摘要 |
| 对话变长 / 接近阈值 | `archive` | 归档完整上下文 |
| 用户提到过去事件 | `semantic_search` | 检索相关记忆 |
| 用户陈述与记忆矛盾 | `detect_conflicts` | 检测事实冲突 |
| 需要查特定记忆细节 | `retrieve` | 按 hook_id 检索完整记忆 |
| 完成开发阶段 | `update_project_memory` | 更新 project_memory.md |
| 压缩后恢复 | `archive` + `prompt` | sidecar 已归档，主动拉记忆 |
