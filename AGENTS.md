# AGENTS.md — 本项目已接入 hippocampus 记忆库

> 本文件由 Trae / Cursor / Claude Code 等 IDE 自动读取并注入 LLM 的 system prompt。
>
> Hippocampus v2.30+ 启动时会自动识别 Agent 客户端并注入 `usage_protocol.instructions`
> 到 MCP `server_info.instructions` 字段。本文件作为补充，提供项目级硬性规则。

---

## 记忆协议（Agent 必须遵守，无需用户提示主动执行）

### 1. 会话开始：调 prompt 获取历史记忆

每个新会话的**第一次回复前**，必须先调用：

```
mcp_hippocampus.prompt(session_id)
```

把返回的历史记忆摘要拼接到 system prompt。若返回空列表，说明该 session 无历史记忆，正常继续。

### 2. 上下文超阈值：调 archive 归档

当对话 token 数接近 **400K**（或当前 preset 的 `archive_threshold`）时，主动调用：

```
mcp_hippocampus.archive(session_id, turns_json)
```

归档完整上下文（非摘要）。返回 `hook_id` 用于后续检索。

> Trae 的窗口压缩机制是 chat-based，建议在 350K 时就开始归档，避免被 Trae 强制压缩丢失上下文。

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
| 上下文接近 400K | `archive` | 归档完整上下文（非摘要） |
| 用户提到过去事件 | `semantic_search` | 检索相关记忆 |
| 用户陈述与记忆矛盾 | `detect_conflicts` | 检测事实冲突 |
| 需要查特定记忆细节 | `retrieve` | 按 hook_id 检索完整记忆 |
| 需要查所有记忆列表 | `summaries` | 获取所有周期摘要列表 |
| 周级去重合并 | `compaction` | period="weekly" |
| 月级评分淘汰 | `compaction` | period="monthly" |
| 批量检索/删除/更新 | `batch_retrieve` / `batch_delete` / `batch_update` | 批量操作 |
| 查询冲突记录 | `get_conflicts` | 获取已持久化的冲突记录 |

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
