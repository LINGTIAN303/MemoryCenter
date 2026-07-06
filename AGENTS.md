# AGENTS.md — 本项目已接入 hippocampus 记忆库

> 本文件由 Trae / Cursor / Claude Code 等 IDE 自动读取并注入 LLM 的 system prompt。
>
> Hippocampus 启动时会自动识别 Agent 客户端并注入 `usage_protocol.instructions`
> 到 MCP `server_info.instructions` 字段。本文件作为补充，提供项目级硬性规则。

---

## 记忆协议（Agent 必须遵守，无需用户提示主动执行）

### 1. 会话开始：调 prompt 获取历史记忆

每个新会话的**第一次回复前**，必须先调用：

```
mcp_hippocampus.prompt(session_id)
```

把返回的历史记忆摘要拼接到 system prompt。若返回空列表，说明该 session 无历史记忆，正常继续。

### 2. 上下文接近阈值：主动调 archive 归档（伪钩子方案）

#### 2.1 触发条件（满足任一即调用）

- **对话长度**：已进行超过 20 轮（用户消息 + LLM 回复算 1 轮）
- **内容复杂度**：包含大量代码 / 长文档 / 多次工具调用
- **主观判断**：你感觉到"上下文开始变重"、"前面说过但记不清细节"
- **Token 反馈**：上次 archive 返回的 `threshold_ratio_percent >= 80`
- **兜底**：每 30 轮对话主动归档一次（即使未达阈值）

#### 2.2 调用方式（简化格式）

```
mcp_hippocampus.archive(
    session_id,                  // 必填
    turns_json                   // 必填，最简格式：[{"user_message":{"text":"..."},"llm_message":{"text":"..."}}]
    // id/timestamp/tags/token_count 可省略，服务端自动补全
)
```

#### 2.3 Token 反馈循环

归档后返回结果包含：
- `estimated_total_tokens`：当前累计估算 token 数
- `threshold`：归档阈值（如 120000 = 120K）
- `threshold_ratio_percent`：当前占比百分比
- `suggestion`：归档建议（人类可读）

**判断规则**：
- `threshold_ratio_percent >= 100` → 立即归档或触发上下文压缩
- `threshold_ratio_percent >= 80` → 提示用户准备归档
- `< 80` → 继续对话，记录累计值作为后续判断依据

> 你（LLM）无法直接感知自身 token 消耗，但通过 archive 返回的 `estimated_total_tokens`
> 可以建立"token 意识"——这是伪钩子方案的核心：通过外部反馈循环模拟主动感知。

#### 2.4 客户端压缩前主动归档

当用户即将手动触发上下文压缩（如 Trae 对话框上方的"压缩"按钮），
或你预判客户端即将自动压缩时，**先调 archive 归档完整上下文**，
避免压缩时丢失原始轮次信息。

> Trae/Cursor 的自动压缩机制会丢弃旧轮次，归档可保留被丢弃的内容。

### 3. 用户提到过去事件：先调 semantic_search 再回复

当用户消息中出现「之前」「上次」「还记得」「上次我们讨论的」「之前那个方案」等指代过去的词语时，**先调用**：

```
mcp_hippocampus.semantic_search(query, session_id, top_k=5)
```

用用户原话作为 `query`，检索相关记忆。把检索结果作为上下文再回复用户。

### 4. 用户陈述与记忆可能矛盾：先调 detect_conflicts 检测

当用户陈述的事实与记忆中的记录可能冲突时（如用户说「我用的是 Python」但记忆里是 Rust），**先调用**：

```
mcp_hippocampus.detect_conflicts(session_id, hook_id, added_facts, revised_facts, deprecated_facts)
```

检测冲突。若检测到冲突，向用户确认后再更新记忆。

### 5. 上下文被压缩后：执行压缩后行为协议

当系统消息中出现以下固定文本时：

```
This session continues a previous conversation that lost its context.
```

表明 Trae 客户端刚压缩了上下文，必须立即执行**压缩后行为协议**（详见 `.trae/rules/hippocampus-archive.md` 的「压缩后行为协议」章节）：

1. 归档压缩前未持久化的轮次（如有）
2. 调用 `prompt` 拉取 hippocampus 一手记忆
3. 交叉校准 Summary 第8章节"Current Work"与 hippocampus 记忆
4. 执行「Next Step 决策协议」：用 Pending todos 校准 Summary 第10章节建议

**核心原则**：
- hippocampus 记忆优先级 > Trae Summary
- Pending todos 状态 > Summary 第10章节建议
- in_progress 任务必须从断点继续，禁止重复提问已完成决策

### 6. project_memory 反向写入：让 hippocampus 记忆流入第7层 Memory Context

hippocampus 维护一份 `project_memory.md` 副本（`projects/{project_id}/project_memory.md`），
LLM 通过 `update_project_memory` 工具更新副本后，用 Write 工具将内容写入 Trae 客户端的
memory 文件夹（如 `c:\Users\<user>\.trae-cn\memory\projects\<project>\project_memory.md`），
完成"反向写入"闭环——让 hippocampus 记忆主动流入第7层 Memory Context。

#### 何时调用 update_project_memory

- **完成一个开发阶段时**：更新 `task_state` / `progress` 章节
- **关键架构决策时**：更新 `decisions` 章节
- **发现风险点时**：更新 `risks` 章节
- **用户说"记住这个"时**：立即更新对应章节

#### 调用方式

```
mcp_hippocampus.update_project_memory(
    project_id="myapp",
    section="task_state",
    content="## 当前任务\n- 动手点 4 已完成\n- 下一步：提交部署",
    action="replace"  // 默认 replace，可选 append / delete
)
```

返回 `full_content` 后，用 Write 工具写入 Trae 的 project_memory.md。

#### 固定章节覆盖策略

章节用 HTML 注释标记界定，**不影响用户手动写入的内容**：

```markdown
<!-- HIPPOCAMPUS:SECTION:task_state START -->
（hippocampus 写入的内容）
<!-- HIPPOCAMPUS:SECTION:task_state END -->

（用户手动写入的内容，不受 hippocampus 影响）
```

同一 section 的内容会被覆盖（action=replace），不同 section 独立存在。

---

## session_id 约定

```
trae-{项目名}-{日期}
```

示例：
- `trae-hippocampus-20260705`
- `trae-myapp-20260705`

> 一个 session_id 对应一个独立的记忆空间。同会话内复用同一 session_id，
> 切换项目或日期时换新 session_id。

---

## 工具触发规则速查表

| 时机 | 工具 | 说明 |
|------|------|------|
| 会话第一次回复前 | `prompt` | 获取历史记忆摘要 |
| 会话开始时 / 调 semantic_search 前 | `get_config` | 查询运行时配置快照（归档阈值 / Agent / scenario / **降级状态**），scope=runtime\|preset\|degraded\|all |
| 上下文接近 400K | `archive` | 归档完整上下文（非摘要） |
| 用户提到过去事件 | `semantic_search` | 检索相关记忆 |
| 用户陈述与记忆矛盾 | `detect_conflicts` | 检测事实冲突 |
| 需要查特定记忆细节 | `retrieve` | 按 hook_id 检索完整记忆 |
| 需要查所有记忆列表 | `summaries` | 获取所有周期摘要列表 |
| 周级去重合并 | `compaction` | period="weekly" |
| 月级评分淘汰 | `compaction` | period="monthly" |
| 批量检索/删除/更新 | `batch_retrieve` / `batch_delete` / `batch_update` | 批量操作 |
| 查询冲突记录 | `get_conflicts` | 获取已持久化的冲突记录 |
| **上下文被压缩后** | `archive` + `prompt` | 归档断层轮次 + 拉取一手记忆校准 Summary，详见「压缩后行为协议」 |
| 完成开发阶段/关键决策/风险点 | `update_project_memory` | 更新 project_memory.md 副本指定章节 |
| 查看 project_memory 副本 | `get_project_memory` | 读取 hippocampus 维护的 project_memory.md 完整内容 |

---

## Preset 查询

hippocampus 内置 5 个 Preset 维度，可通过以下工具查询可选值：

- `mcp_hippocampus.preset_list_agents` — 11 个内置 Agent（ClaudeCode/Cursor/Trae/Codex 等）
- `mcp_hippocampus.preset_list_scenarios` — 7 个内置 Scenario（coding/writing/research 等）
- `mcp_hippocampus.preset_list_models` — 所有 ModelVariant
- `mcp_hippocampus.preset_build` — 构建自定义 CombinedProfile

---

## 降级说明

hippocampus 在以下情况会降级，但仍保持核心功能可用：

| 未配置 | 降级行为 |
|--------|----------|
| LLM 摘要生成器 | 启发式摘要（首条消息前 80 字符） |
| Embedder API | 仅关键词检索（BM25） |
| LLM 冲突检测器 | 启发式纯算法（三维度检测） |
| Agent 客户端未识别 | 不注入 usage_protocol，LLM 需依赖本 AGENTS.md 主动调用 |

---

## 参考文档

- [Trae 接入指南](docs/onboarding/trae.md)
- [v2.30 Agent 接入路线图](docs/v2.30-roadmap-agent-onboarding.md)
- [架构文档](docs/ARCHITECTURE.md)
- [部署文档](docs/DEPLOY.md)
