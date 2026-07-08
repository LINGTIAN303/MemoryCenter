# AGENTS.md — 本项目已接入 memory-center 记忆库

> 本文件由 Trae / Cursor / Claude Code 等 IDE 自动读取并注入 LLM 的 system prompt。
>
> MemoryCenter 启动时会自动识别 Agent 客户端并注入 `usage_protocol.instructions`
> 到 MCP `server_info.instructions` 字段。本文件作为补充，提供项目级硬性规则。

---

## 记忆协议（Agent 必须遵守，无需用户提示主动执行）

### 1. 会话开始：调 prompt 获取历史记忆

每个新会话的**第一次回复前**，必须先调用：

```
mcp_memory-center.prompt(session_id)
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
mcp_memory-center.archive(
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

### 3. 上下文即将被压缩：调 pre_compress_hook 完整归档

当 LLM 检测到以下压缩前兆时，**优先调用 `pre_compress_hook` 而非 `archive`**：

- 客户端提示"上下文即将压缩" / "context compression"
- 系统消息出现 "This session continues a previous conversation that lost its context"
- LLM 主观判断上下文已接近窗口上限（如累计 token 估算接近 200K）
- 用户手动触发压缩（如点击"压缩"按钮）

调用方式：
```
mcp_memory-center.pre_compress_hook(
    session_id,                  // 必填
    full_context,                // 必填，完整上下文字符串
    estimated_tokens,            // 可选，估算 token 数
    task_state_snapshot,         // 可选，任务状态快照（推荐传入）
    project_id                   // 可选
)
```

**与 archive 的区别**：
- `archive`：日常归档，传入结构化 turns 数组
- `pre_compress_hook`：压缩前一次性归档，传入完整上下文字符串，双轨处理（raw_context 原样保存 + 解析 turns 复用 Archiver）

**核心价值**：即使客户端压缩丢弃了原始轮次，memory-center 仍保留了完整的 raw_context 备份，可通过 retrieve 工具按需拉取。

### 4. 用户提到过去事件：先调 semantic_search 再回复

当用户消息中出现「之前」「上次」「还记得」「上次我们讨论的」「之前那个方案」等指代过去的词语时，**先调用**：

```
mcp_memory-center.semantic_search(query, session_id, top_k=5)
```

用用户原话作为 `query`，检索相关记忆。把检索结果作为上下文再回复用户。

### 5. 用户陈述与记忆可能矛盾：先调 detect_conflicts 检测

当用户陈述的事实与记忆中的记录可能冲突时（如用户说「我用的是 Python」但记忆里是 Rust），**先调用**：

```
mcp_memory-center.detect_conflicts(session_id, hook_id, added_facts, revised_facts, deprecated_facts)
```

检测冲突。若检测到冲突，向用户确认后再更新记忆。

### 6. 上下文被压缩后：执行压缩后行为协议

当系统消息中出现以下固定文本时：

```
This session continues a previous conversation that lost its context.
```

表明 Trae 客户端刚压缩了上下文，必须立即执行**压缩后行为协议**（详见 `docs/onboarding/rules/trae-memory-center-archive.md` 的「压缩后行为协议」章节）：

1. 归档压缩前未持久化的轮次（如有）
2. 调用 `prompt` 拉取 memory-center 一手记忆
3. 交叉校准 Summary 第8章节"Current Work"与 memory-center 记忆
4. 执行「Next Step 决策协议」：用 Pending todos 校准 Summary 第10章节建议

**核心原则**：
- memory-center 记忆优先级 > Trae Summary
- Pending todos 状态 > Summary 第10章节建议
- in_progress 任务必须从断点继续，禁止重复提问已完成决策

### 7. project_memory 反向写入：让 memory-center 记忆流入第7层 Memory Context

memory-center 维护一份 `project_memory.md` 副本（`projects/{project_id}/project_memory.md`），
LLM 通过 `update_project_memory` 工具更新副本后，用 Write 工具将内容写入 Trae 客户端的
memory 文件夹（如 `c:\Users\<user>\.trae-cn\memory\projects\<project>\project_memory.md`），
完成"反向写入"闭环——让 memory-center 记忆主动流入第7层 Memory Context。

#### 何时调用 update_project_memory

- **完成一个开发阶段时**：更新 `task_state` / `progress` 章节
- **关键架构决策时**：更新 `decisions` 章节
- **发现风险点时**：更新 `risks` 章节
- **用户说"记住这个"时**：立即更新对应章节

#### 调用方式

```
mcp_memory-center.update_project_memory(
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
<!-- MEMORY_CENTER:SECTION:task_state START -->
（memory-center 写入的内容）
<!-- MEMORY_CENTER:SECTION:task_state END -->

（用户手动写入的内容，不受 memory-center 影响）
```

同一 section 的内容会被覆盖（action=replace），不同 section 独立存在。

---

## session_id 约定

```
trae-{项目名}-{日期}
```

示例：
- `trae-memory-center-20260705`
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
| 上下文即将被压缩 | `pre_compress_hook` | 压缩前一次性完整归档（raw_context + 解析 turns 双轨） |
| 用户提到过去事件 | `semantic_search` | 检索相关记忆 |
| 用户陈述与记忆矛盾 | `detect_conflicts` | 检测事实冲突 |
| 需要查特定记忆细节 | `retrieve` | 按 hook_id 检索完整记忆 |
| 用户只提供短 ID（如 305b700e） | `find_hook_by_prefix` | 按前缀查找完整 hook_id（跨 session 检索） |
| 需要查所有记忆列表 | `summaries` | 获取所有周期摘要列表 |
| 周级去重合并 | `compaction` | period="weekly" |
| 月级评分淘汰 | `compaction` | period="monthly" |
| 批量检索/删除/更新 | `batch_retrieve` / `batch_delete` / `batch_update` | 批量操作 |
| 查询冲突记录 | `get_conflicts` | 获取已持久化的冲突记录 |
| **首次接入 MemoryCenter** | `install_rules` | 安装记忆协议规则到项目（支持 catpaw/trae/claude-code 三种客户端） |
| **上下文被压缩后** | `archive` + `prompt` | 归档断层轮次 + 拉取一手记忆校准 Summary，详见「压缩后行为协议」 |
| 完成开发阶段/关键决策/风险点 | `update_project_memory` | 更新 project_memory.md 副本指定章节 |
| 查看 project_memory 副本 | `get_project_memory` | 读取 memory-center 维护的 project_memory.md 完整内容 |
| 查询预设可选值 | `preset_list_agents` / `preset_list_scenarios` / `preset_list_models` | 列出内置 Agent / Scenario / ModelVariant |
| 预检预设效果 | `preset_build` | 即时构建 CombinedProfile，返回最终生效值 |

---

## MCP 传输模式

MemoryCenter MCP Server 支持两种传输模式：

| 模式 | 版本 | 适用场景 | 配置方式 |
|------|------|----------|----------|
| **stdio** | v2.3 | 本地 IDE（Claude Code / Cursor / Trae） | `command` + `env` |
| **Streamable HTTP** | v2.36 | 远程客户端（DeepSeek 网页端等） | `url` + `transport: "streamable-http"` |

### stdio 模式配置

```json
{
  "mcpServers": {
    "memory-center": {
      "command": "/path/to/memory-center-mcp",
      "env": { "MEMORY_CENTER_ROOT": "/path/to/memory/data" }
    }
  }
}
```

### Streamable HTTP 模式配置

```json
{
  "mcpServers": {
    "memory-center": {
      "url": "https://your-server/mcp",
      "transport": "streamable-http"
    }
  }
}
```

Streamable HTTP 模式环境变量：

| 环境变量 | 说明 | 默认值 |
|---------|------|--------|
| `MEMORY_CENTER_MCP_ENABLED` | 是否启用 MCP HTTP 端点 | `false` |
| `MEMORY_CENTER_MCP_STATEFUL` | 是否启用 session 模式 | `true` |
| `MEMORY_CENTER_MCP_ALLOWED_HOSTS` | 允许的 Host（DNS rebinding 防护） | `localhost,127.0.0.1,::1` |
| `MEMORY_CENTER_MCP_ALLOWED_ORIGINS` | 允许的 Origin（CORS 防护） | 空 |

---

## install_rules 远程模式

当 MCP server 无法访问客户端本地路径时（如 HTTPS MCP 模式下 server 在远程），`install_rules` 工具会返回模板内容让 LLM 用客户端的 Write 工具自行创建文件：

- **本地模式**（路径存在）：server 直接写入文件
- **远程模式**（路径不存在）：返回 `action=remote_template` + `files[]`（含 `relative_path` + `content` + `mode`）

远程模式下 LLM 需要解析返回的 `files` 数组，用 Write 工具按 `relative_path` 创建文件。`mode` 字段：
- `create`：创建新文件
- `append_with_markers`：带标记追加（已存在文件只更新标记区间）

---

## Preset 查询

memory-center 内置 5 个 Preset 维度，可通过以下工具查询可选值：

- `mcp_memory-center.preset_list_agents` — 11 个内置 Agent（ClaudeCode/Cursor/Trae/Codex 等）
- `mcp_memory-center.preset_list_scenarios` — 7 个内置 Scenario（coding/writing/research 等）
- `mcp_memory-center.preset_list_models` — 所有 ModelVariant
- `mcp_memory-center.preset_build` — 构建自定义 CombinedProfile

---

## 降级说明

memory-center 在以下情况会降级，但仍保持核心功能可用：

| 未配置 | 降级行为 |
|--------|----------|
| LLM 摘要生成器 | 启发式摘要（首条消息前 80 字符） |
| Embedder API | 仅关键词检索（BM25） |
| LLM 冲突检测器 | 启发式纯算法（三维度检测） |
| Agent 客户端未识别 | 不注入 usage_protocol，LLM 需依赖本 AGENTS.md 主动调用 |

---

## 参考文档

- [Trae 接入指南](docs/onboarding/trae.md)
- [架构文档](docs/ARCHITECTURE.md)
- [部署文档](docs/DEPLOY.md)
