# 场景六推演：Agent 编程工具全流程（Codex + GPT-5.5 从零生产项目）

> 本文档推演用户使用 Codex CLI（GPT-5.5）从零开始生产一个完整项目的全过程，
> 详细记录每个阶段用户与 Agent 的交互、Hippocampus 在其中被调用的时机与作用。
>
> **用途**：
> - 帮助开发者理解 Hippocampus 在真实 Agent 编程工作流中的定位
> - 帮助用户（产品方）理解「记忆库」对长周期项目开发的价值
> - 为后续功能开发提供真实场景参照
>
> **关联文档**：
> - 场景设定见 [01-scenario-design.md](./01-scenario-design.md)
> - 内部调用链见 [02-internal-call-flow.md](./02-internal-call-flow.md)
> - 4 周演化推演见 [03-mcp-coding-assistant.md](./03-mcp-coding-assistant.md)

---

## 0. 推演总览

### 0.1 参与方

| 角色 | 身份 | 工具 |
|------|-----|------|
| 小李 | 独立开发者 | Codex CLI + Hippocampus MCP Server |
| Codex CLI | Agent 编程工具 | GPT-5.5 + MCP 客户端 |
| Hippocampus | 记忆库依赖 | MCP Server（stdio） |

### 0.2 项目设定

| 参数 | 值 | 说明 |
|------|----|----|
| 项目名 | `blog-backend` | 博客系统后端 API |
| 技术栈 | Rust + Axum 0.8 + SQLx + PostgreSQL | 与场景一一致 |
| 会话 ID | `blog-backend-session` | 跨会话复用 |
| 项目 ID | `blog-backend` | project 级聚合索引 |
| Agent 工具 | Codex CLI（GPT-5.5） | 支持 MCP 协议 |
| 预计周期 | 7 天（含跨会话） | 模拟真实开发节奏 |

### 0.3 Hippocampus 配置

| 配置项 | 值 | 说明 |
|--------|----|----|
| `token_threshold` | 200K | Codex 单次会话较短，阈值降低 |
| `force_truncate_limit` | 300K | 1.5 倍硬上限 |
| 接入方式 | MCP Server（stdio） | Codex 自动拉起子进程 |
| 触发模式 | Agent 自主决策 | Codex 判断达阈值后调用 archive |

### 0.4 全流程时间线

| 阶段 | 时间 | 会话 | 累计 token | Hippocampus 调用 |
|------|-----|------|-----------|-----------------|
| Day 1 上午 | 需求沟通 + 项目初始化 | S1 | 80K | archive（D1） |
| Day 1 下午 | 数据库 Schema 设计 | S2 | 150K | prompt → archive（D2） |
| Day 2 全天 | 用户认证模块（JWT + RBAC） | S3 | 280K | prompt → retrieve → archive（D3，含冲突检测） |
| Day 3 全天 | 文章 CRUD + 标签系统 | S4 | 220K | prompt → retrieve × 2 → archive（D4） |
| Day 4 全天 | 评论系统 + 点赞功能 | S5 | 180K | prompt → retrieve → archive（D5） |
| Day 5 全天 | 测试用例 + 性能优化 | S6 | 200K | prompt → retrieve × 3 → archive（D6） |
| Day 6 全天 | 部署 + 文档 + Bug 修复 | S7 | 120K | prompt → retrieve × 2 → archive（D7） |
| Day 7（周日 23:00） | 周期任务 | — | — | compaction(weekly) |

---

## 1. Day 1 上午：需求沟通 + 项目初始化

### 1.1 用户操作（08:00）

小李在终端启动 Codex CLI，配置好工作目录 `~/projects/blog-backend`，输入需求：

```
小李：我要用 Rust + Axum 开发一个博客系统后端 API。
     需要支持：用户注册登录、文章发布、评论、点赞、标签。
     用 PostgreSQL，要支持 JWT 认证。
```

### 1.2 Codex 初始化（08:01）

Codex CLI 启动时执行两件事：
1. 拉起 Hippocampus MCP Server 子进程（stdio 传输）
2. 调用 `prompt` tool 注入历史记忆索引

**调用链**：

```
Codex CLI 启动
  ↓ 拉起 MCP Server 子进程（hippocampus-mcp --root ~/.hippocampus/blog-backend）
MCP Server 就绪
  ↓ Codex 调用 prompt tool（会话开始必调）
Retriever::render_to_system_prompt()
  ↓ Storage::read_index(session_id="blog-backend-session", project_id="blog-backend", period=Daily/Weekly/Monthly)
  ↓ 三个周期索引文档均不存在（首次会话）
返回 ""
  ↓ Codex 收到空 prompt，知道是首次会话
```

**Codex 行为**：收到空 prompt，不注入任何历史记忆，直接基于用户需求开始工作。

### 1.3 Codex 工作过程（08:01-12:00）

Codex 执行以下任务：
1. `cargo init` 创建项目
2. 添加 Cargo.toml 依赖（axum 0.8 / sqlx / tokio / serde / jsonwebtoken）
3. 创建目录结构（`src/handlers/` / `src/models/` / `src/middleware/` / `migrations/`）
4. 编写 README.md 与初始 .gitignore
5. 初始化 git 仓库 + 首次 commit

期间小李不断与 Codex 交互：
- "再加一个 dotenv 配置"
- "项目用 MIT 协议"
- "main.rs 写个 hello world"

### 1.4 Codex 主动归档（12:00）

到 12:00 时，Codex 内部 token 计数器达 80K。Codex 知道接近阈值（虽然未达 200K，但小李要午休，Codex 主动建议归档）：

```
Codex：当前会话已积累 80K token，建议归档以便跨会话延续。是否归档？
小李：好
```

Codex 调用 `archive` tool：

**调用链**：[02-internal-call-flow.md §1 archive 调用链](./02-internal-call-flow.md#1-archive-调用链归档)

```
Codex 调用 archive tool
  ↓ turns_json（约 60 个 MessageTurn，含需求讨论 + 代码生成 + 用户反馈）
MCP Server: archive
  ↓
Archiver::push_turn() × 60
  ↓
Archiver::archive()
  ↓ 生成 MemoryFile（id=uuid-d1, period=Daily, total_tokens=80000, tags=[Text, CodeBlock, URL, Plan]）
Storage::write_memory()  ← 原子写入
  ↓
IndexHook::from_memory_file()
  ↓ summary.title="博客系统后端需求讨论与项目初始化"
Storage::append_hook()  ← session 级 daily 索引
  ↓
Storage::append_project_hook()  ← project 级 daily 索引（v2.4 双写）
  ↓
返回 SummaryView JSON（含 hook_id=uuid-h1）
  ↓
Codex 记录 hook_id 到本地会话状态
```

**返回的 SummaryView**（Codex 内部记录）：

```json
{
  "hook_id": "aa11bb22-cc33-dd44-ee55-ff66778899001",
  "memory_id": "bb22cc33-dd44-ee55-ff66-778899001122",
  "summary_title": "博客系统后端需求讨论与项目初始化",
  "tags": ["文本消息", "代码块", "URL", "计划"],
  "archived_at": "2026-07-04T12:00:00Z",
  "period": "daily",
  "token_count": 80000,
  "is_rich": false
}
```

**小李感知**：看到 Codex 输出「已归档 60 个轮次，hook_id=aa11bb22...」。

---

## 2. Day 1 下午：数据库 Schema 设计

### 2.1 新会话开始（14:00）

小李午休后开新会话。Codex CLI 启动时自动调用 `prompt` tool。

**返回的 prompt 文本**：

```markdown
# 可用记忆索引

以下是可用的历史记忆摘要，可直接基于此信息回答用户问题：

## 近期记忆（daily）

- **博客系统后端需求讨论与项目初始化**[文本消息, 代码块, URL, 计划]（80000 tokens, at 2026-07-04T12:00:00Z）
  - 记忆 ID: `aa11bb22-cc33-dd44-ee55-ff66778899001`

## 周度记忆（weekly）

（无）

## 月度记忆（monthly）

（无）
```

**Codex 行为**：知道上午已讨论需求与初始化项目，本轮专注数据库 Schema 设计。

### 2.2 用户交互（14:00-18:00）

```
小李：基于上午的需求，设计数据库 Schema
Codex：好的，参考上午的记忆，我设计如下表结构：
       - users（id/email/password_hash/role/created_at）
       - posts（id/author_id/title/content/status/published_at）
       - tags（id/name）
       - post_tags（post_id/tag_id）
       - comments（id/post_id/author_id/content/parent_id/created_at）
       - likes（id/user_id/target_id/target_type）
```

Codex 生成 SQLx migration 文件 `migrations/20260704_init.sql`，并执行 `sqlx migrate run`。

### 2.3 跨会话记忆延续的关键

**没有 Hippocampus 时**：小李需要重新告诉 Codex 上午讨论的需求（产品定位、技术栈选择、模块划分）。

**有 Hippocampus 时**：Codex 通过 prompt tool 看到上午的摘要「博客系统后端需求讨论与项目初始化」，知道：
- 技术栈：Rust + Axum + SQLx + PostgreSQL
- 模块：用户/文章/评论/点赞/标签
- 认证：JWT

直接基于此上下文设计 Schema，无需重新沟通。

### 2.4 归档（18:00）

会话结束，Codex 调用 `archive` tool，生成第二个 daily 记忆文件。

```
MemoryFile（id=uuid-d2, period=Daily, total_tokens=150000, tags=[Text, CodeBlock, URL, Plan]）
summary.title="数据库 Schema 设计与 migration 生成"
```

---

## 3. Day 2：用户认证模块（含冲突检测场景）

### 3.1 会话开始（09:00）

Codex 调用 `prompt` tool，返回 2 个 daily 钩子：

```markdown
## 近期记忆（daily）

- **博客系统后端需求讨论与项目初始化**[...]（80000 tokens, at 2026-07-04T12:00:00Z）
  - 记忆 ID: `aa11bb22-...-001`
- **数据库 Schema 设计与 migration 生成**[...]（150000 tokens, at 2026-07-04T18:00:00Z）
  - 记忆 ID: `aa11bb22-...-002`
```

### 3.2 用户交互（09:00-12:00）

```
小李：实现用户注册接口
Codex：好的，先创建 User model 与 handler：
       [生成 src/models/user.rs]
       [生成 src/handlers/auth.rs]
       实现 POST /api/v1/auth/register，包含：邮箱校验、密码 Argon2 哈希、入库
```

### 3.3 关键时刻：retrieve 调用（10:30）

小李要求 Codex 参考上午的 Schema 设计实现 User model：

```
小李：User model 要和上午的 users 表对齐
Codex：好的，让我先检索上午的 Schema 细节
```

Codex 调用 `retrieve` tool：

**调用链**：[02-internal-call-flow.md §2 retrieve 调用链](./02-internal-call-flow.md#2-retrieve-调用链检索)

```
Codex 调用 retrieve tool，传入 hook_id="aa11bb22-...-002"
  ↓
Retriever::retrieve_memory("aa11bb22-...-002")
  ↓ 遍历 daily 索引（找到匹配 hook）
  ↓ Storage::read_memory(memory_id=uuid-d2)
  ↓
返回完整 MemoryFile JSON（含 90 个 turn，150K token）
  ↓
Codex 提取 Schema 相关 turn（约 5 个 turn，8K token）
  ↓
基于历史 Schema 生成对齐的 User model：
  pub struct User {
      pub id: Uuid,
      pub email: String,
      pub password_hash: String,
      pub role: String,
      pub created_at: DateTime<Utc>,
  }
```

**关键点**：retrieve 返回完整 MemoryFile，Codex 只提取相关部分注入 GPT-5.5 上下文，避免 150K token 全部塞入。

### 3.4 冲突检测场景（14:00）

小李决定调整密码哈希算法：

```
小李：密码哈希从 Argon2 改为 bcrypt（性能更好）
```

Codex 知道上午的 turn 中记录了「使用 Argon2 哈希密码」，先调用 `detect_conflicts` 预检测：

**调用链**：[hippocampus-mcp/src/lib.rs detect_conflicts tool](../../crates/hippocampus-mcp/src/lib.rs)

```
Codex 调用 detect_conflicts tool
  ↓
入参：
  - hook_id="aa11bb22-...-002"（数据库 Schema 设计的钩子）
  - revised_facts=["密码哈希算法：Argon2 → bcrypt"]
  - added_facts=["密码哈希使用 bcrypt"]
  - deprecated_facts=["密码哈希使用 Argon2"]
  ↓
HeuristicDetector::detect(...)
  ↓
"Argon2" vs "bcrypt" 模式匹配
  ↓ 算法名差异 + "哈希算法" 关键词匹配
  ↓
报告 ConflictReport {
  has_conflicts: true,
  has_critical: false,  // 非关键冲突（哈希算法变更不破坏数据）
  conflicts: [{
    kind: DirectContradict,
    existing_fact: "密码哈希使用 Argon2",
    new_fact: "密码哈希使用 bcrypt",
    reason: "算法名差异：Argon2 vs bcrypt，模式「哈希算法」匹配",
    severity: Warning
  }]
}
```

**Codex 决策**：`has_critical=false`，无需用户确认即可继续。Codex 调用 `batch_update` 持久化冲突记录：

```
Codex 调用 batch_update tool
  ↓
入参：
  - updates=[{
      hook_id: "aa11bb22-...-002",
      revised_facts: ["密码哈希算法：Argon2 → bcrypt"],
      deprecated_facts: ["密码哈希使用 Argon2"]
    }]
  ↓
逐条 read_memory → detect_conflicts → update_memory_with_conflicts
  ↓ 冲突记录随 MemoryUpdateRecord 持久化
返回更新结果
```

**Codex 继续工作**：基于新决策（bcrypt）实现密码哈希逻辑。

### 3.5 归档（18:00）

会话结束，Codex 调用 `archive` tool，生成第三个 daily 记忆文件：

```
MemoryFile（id=uuid-d3, period=Daily, total_tokens=280000, tags=[Text, CodeBlock, ToolCall, Thinking]）
summary.title="用户认证模块实现（含 bcrypt 哈希决策）"
```

**注意 tags 新增**：
- `ToolCall`：含 retrieve + detect_conflicts + batch_update 调用记录
- `Thinking`：含 Codex 的内部推理（为何调用 retrieve、如何决策冲突）

---

## 4. Day 3：文章 CRUD + 标签系统

### 4.1 会话开始（09:00）

Codex 调用 `prompt` tool，返回 3 个 daily 钩子。

### 4.2 多次 retrieve 调用

#### 第一次 retrieve（10:00）

```
小李：实现文章 CRUD，参考 users 表的 User model 风格
Codex：好的，先检索 users 表的 model 实现
```

Codex 调用 `retrieve` tool，传入 hook_id=Day 2 的钩子（用户认证模块），获取 User model 代码风格，生成对齐的 Post model。

#### 第二次 retrieve（14:00）

```
小李：实现标签系统，要和 posts 表关联
Codex：好的，先检索 posts 表的 Schema
```

Codex 调用 `retrieve` tool，传入 hook_id=Day 1 下午的钩子（数据库 Schema 设计），获取 posts 与 tags 关联表 Schema。

**关键点**：Codex 主动判断需要哪些历史细节，并通过 retrieve 按需加载，避免一次性塞入所有历史。

### 4.3 归档（18:00）

```
MemoryFile（id=uuid-d4, period=Daily, total_tokens=220000, tags=[Text, CodeBlock, ToolCall]）
summary.title="文章 CRUD + 标签系统实现"
```

---

## 5. Day 4：评论系统 + 点赞功能

### 5.1 会话开始（09:00）

Codex 调用 `prompt` tool，返回 4 个 daily 钩子。

### 5.2 retrieve 调用（10:00）

```
小李：评论要支持嵌套（parent_id），参考之前的 comments 表设计
Codex：好的，先检索 comments 表 Schema
```

Codex 调用 `retrieve` tool，传入 hook_id=Day 1 下午的钩子，获取 comments 表的 parent_id 自引用设计。

### 5.3 归档（18:00）

```
MemoryFile（id=uuid-d5, period=Daily, total_tokens=180000, tags=[Text, CodeBlock, ToolCall]）
summary.title="评论系统 + 点赞功能实现"
```

---

## 6. Day 5：测试用例 + 性能优化

### 6.1 会话开始（09:00）

Codex 调用 `prompt` tool，返回 5 个 daily 钩子。

### 6.2 多次 retrieve 调用

#### retrieve × 3（全天）

```
小李：为所有 handler 写单元测试
Codex：好的，先检索所有 handler 实现
  ↓ retrieve Day 2（用户认证）
  ↓ retrieve Day 3（文章 CRUD）
  ↓ retrieve Day 4（评论 + 点赞）
```

**关键点**：Codex 主动调用 `batch_retrieve` tool 一次性获取多个钩子，减少多次单条 retrieve 的开销。

**调用链**：

```
Codex 调用 batch_retrieve tool
  ↓ hook_ids_json=["aa11bb22-...-003", "aa11bb22-...-004", "aa11bb22-...-005"]
  ↓
Retriever 逐个 retrieve_memory（内部循环）
  ↓ 返回 3 个 MemoryFile JSON 数组
  ↓
Codex 提取每个文件的 handler 代码（约 30 个 turn，60K token）
  ↓
基于历史代码生成对应单元测试
```

### 6.3 性能优化（14:00）

```
小李：文章列表查询太慢，加索引
Codex：好的，基于 Schema 检索现有索引
  ↓ retrieve Day 1 下午（Schema 设计）
  ↓
发现 posts 表缺 author_id 与 status 索引
  ↓
生成新 migration: 20260708_add_indexes.sql
```

### 6.4 归档（18:00）

```
MemoryFile（id=uuid-d6, period=Daily, total_tokens=200000, tags=[Text, CodeBlock, ToolCall, Thinking]）
summary.title="测试用例编写 + 性能优化（索引补充）"
```

---

## 7. Day 6：部署 + 文档 + Bug 修复

### 7.1 会话开始（09:00）

Codex 调用 `prompt` tool，返回 6 个 daily 钩子。

### 7.2 retrieve 调用（10:00）

```
小李：写部署文档，包含所有环境变量
Codex：好的，先检索所有配置相关代码
  ↓ retrieve Day 1 上午（dotenv 配置）
  ↓ retrieve Day 2（JWT 配置）
  ↓
提取配置项：DATABASE_URL/JWT_SECRET/JWT_EXPIRE_HOURS/SMTP_HOST 等
  ↓
生成 docs/deployment.md
```

### 7.3 Bug 修复（14:00）

小李测试时发现评论嵌套查询性能问题：

```
小李：评论嵌套查询 N+1 问题，参考之前的 comments Schema 优化
Codex：好的，先检索 comments 表 Schema
  ↓ retrieve Day 1 下午（Schema 设计）
  ↓
发现 parent_id 缺索引
  ↓
生成新 migration + 优化查询逻辑（使用 CTE 递归查询）
```

### 7.4 归档（18:00）

```
MemoryFile（id=uuid-d7, period=Daily, total_tokens=120000, tags=[Text, CodeBlock, URL, ToolCall]）
summary.title="部署文档 + Bug 修复（评论嵌套查询优化）"
```

---

## 8. Day 7（周日 23:00）：周期任务

### 8.1 weekly_merge 触发

小李配置的定时任务触发 `compaction` tool，period="weekly"。

**调用链**：[02-internal-call-flow.md §5.2 weekly_merge](./02-internal-call-flow.md#52-weekly_merge-调用链)

### 8.2 执行过程

#### 输入：7 个 daily 记忆文件

| 文件 | token | turn 数 | tags |
|------|-------|---------|------|
| uuid-d1（需求 + 初始化） | 80K | 60 | Text, CodeBlock, URL, Plan |
| uuid-d2（Schema 设计） | 150K | 90 | Text, CodeBlock, URL, Plan |
| uuid-d3（用户认证） | 280K | 140 | Text, CodeBlock, ToolCall, Thinking |
| uuid-d4（文章 + 标签） | 220K | 110 | Text, CodeBlock, ToolCall |
| uuid-d5（评论 + 点赞） | 180K | 95 | Text, CodeBlock, ToolCall |
| uuid-d6（测试 + 优化） | 200K | 100 | Text, CodeBlock, ToolCall, Thinking |
| uuid-d7（部署 + Bug） | 120K | 65 | Text, CodeBlock, URL, ToolCall |
| **合计** | **1.23M** | **660** | — |

#### 寒暄剥离

| 来源 | 总 turn | 寒暄 turn | 保留 turn |
|------|---------|----------|-----------|
| 7 个 daily | 660 | 68 | 592 |

被剥离的寒暄示例：「好」「嗯」「收到」「明白」「对」「是的」「谢谢」「再见」等。

#### 输出：1 个 weekly 记忆文件

```
MemoryFile（id=uuid-w1, period=Weekly, total_tokens=1180000, turns=592）
summary.title="周度合并（7 个记忆）"
abstract_text="本周合并了 7 个日级记忆：博客系统后端需求讨论与项目初始化；数据库 Schema 设计与 migration 生成；用户认证模块实现（含 bcrypt 哈希决策）；文章 CRUD + 标签系统实现；评论系统 + 点赞功能实现；测试用例编写 + 性能优化（索引补充）；部署文档 + Bug 修复（评论嵌套查询优化）"
key_facts=[
  "博客系统后端需求讨论与项目初始化",
  "数据库 Schema 设计与 migration 生成",
  "用户认证模块实现（含 bcrypt 哈希决策）",
  "文章 CRUD + 标签系统实现",
  "评论系统 + 点赞功能实现",
  "测试用例编写 + 性能优化（索引补充）",
  "部署文档 + Bug 修复（评论嵌套查询优化）"
]
tags=[Text, CodeBlock, URL, Plan, ToolCall, Thinking]
```

### 8.3 周期任务后的 prompt 渲染

下一周小李开始新会话（如继续优化），Codex 调用 `prompt` tool：

```markdown
# 可用记忆索引

以下是可用的历史记忆摘要，可直接基于此信息回答用户问题：

## 近期记忆（daily）

- **博客系统后端需求讨论与项目初始化**[...]（80000 tokens, at 2026-07-04T12:00:00Z）
  - 记忆 ID: `aa11bb22-...-001`
- **数据库 Schema 设计与 migration 生成**[...]（150000 tokens, at 2026-07-04T18:00:00Z）
  - 记忆 ID: `aa11bb22-...-002`
- ...（共 7 个 daily 钩子）

## 周度记忆（weekly）

- **周度合并（7 个记忆）**[文本消息, 代码块, URL, 计划, 工具调用, 思考过程]（1180000 tokens, at 2026-07-10T23:00:00Z）
  - 记忆 ID: `cc33dd44-...-001`
  - 摘要：本周合并了 7 个日级记忆：...
  - 关键事实：
    - 博客系统后端需求讨论与项目初始化
    - 数据库 Schema 设计与 migration 生成
    - 用户认证模块实现（含 bcrypt 哈希决策）
    - 文章 CRUD + 标签系统实现
    - 评论系统 + 点赞功能实现
    - 测试用例编写 + 性能优化（索引补充）
    - 部署文档 + Bug 修复（评论嵌套查询优化）
  - 关键实体：CodeBlock, Text, URL, Plan, ToolCall, Thinking

## 月度记忆（monthly）

（无）
```

---

## 9. Hippocampus 在全流程中的作用总结

### 9.1 按阶段梳理

| 阶段 | 用户痛点 | Hippocampus 作用 | 调用 tool |
|------|---------|----------------|-----------|
| 项目初始化 | 长会话 token 超限 | 归档保存完整对话 | archive |
| 跨会话延续 | 新会话不知历史 | prompt 注入摘要索引 | prompt |
| 历史细节追溯 | 需要参考前几天的代码 | retrieve 按需加载完整 MemoryFile | retrieve |
| 决策变更 | 改用新方案（如 Argon2→bcrypt） | detect_conflicts 预检测冲突 | detect_conflicts + batch_update |
| 批量追溯 | 写测试需要所有 handler 代码 | batch_retrieve 一次性获取多个 | batch_retrieve |
| 周期清理 | daily 文件过多 | weekly_merge 寒暄剥离 + 合并 | compaction |

### 9.2 调用频次统计

| Tool | Day 1 | Day 2 | Day 3 | Day 4 | Day 5 | Day 6 | 合计 |
|------|-------|-------|-------|-------|-------|-------|------|
| prompt | 2 | 1 | 1 | 1 | 1 | 1 | 7 |
| archive | 2 | 1 | 1 | 1 | 1 | 1 | 7 |
| retrieve | 0 | 1 | 2 | 1 | 3 | 2 | 9 |
| batch_retrieve | 0 | 0 | 0 | 0 | 1 | 0 | 1 |
| detect_conflicts | 0 | 1 | 0 | 0 | 0 | 0 | 1 |
| batch_update | 0 | 1 | 0 | 0 | 0 | 0 | 1 |
| compaction | 0 | 0 | 0 | 0 | 0 | 0 | 1（周日） |
| **合计** | 4 | 5 | 4 | 3 | 6 | 4 | **27** |

### 9.3 价值量化对比

| 维度 | 无 Hippocampus | 有 Hippocampus | 节省 |
|------|---------------|---------------|------|
| 跨会话上下文重述 | 每次会话重述 50K token | 0 token（prompt 自动注入） | 7 × 50K = 350K |
| 历史细节追溯 | 全量历史塞入（1.23M token） | 按需 retrieve（9 × 15K = 135K） | 1.095M |
| 决策冲突避免 | 用户人工记忆 + 易出错 | detect_conflicts 自动检测 | 1 次关键冲突避免 |
| LLM 上下文负载 | 1.23M（超出窗口） | 7 × 200K = 1.4M 但分批 | 单次始终 <300K |

### 9.4 Hippocampus 在 Agent 工作流中的定位

```
┌──────────────────────────────────────────────────────────────┐
│ 用户（小李）                                                   │
│   ↓ 输入需求 / 反馈 / 决策                                      │
└──────────────────────────────────────────────────────────────┘
                              ↓
┌──────────────────────────────────────────────────────────────┐
│ Agent 编程工具（Codex CLI + GPT-5.5）                          │
│   - 代码生成 / 修改 / 测试                                      │
│   - 内部 token 计数器                                           │
│   ↓ 达阈值或会话结束                                            │
└──────────────────────────────────────────────────────────────┘
        ↑↓ prompt          ↑↓ retrieve         ↑↓ archive
┌──────────────────────────────────────────────────────────────┐
│ Hippocampus MCP Server                                         │
│   - prompt：会话开始注入摘要索引                                │
│   - retrieve：按需加载完整 MemoryFile                          │
│   - archive：归档完整对话（非摘要）                             │
│   - detect_conflicts：决策变更预检测                            │
│   - compaction：周期清理（寒暄剥离 + 评分淘汰）                  │
└──────────────────────────────────────────────────────────────┘
                              ↓
┌──────────────────────────────────────────────────────────────┐
│ Storage（LocalStorage）                                        │
│   sessions/blog-backend-session/projects/blog-backend/        │
│   ├── daily/         ← 7 个日级记忆文件                         │
│   ├── weekly/        ← 1 个周级记忆文件                         │
│   └── index/         ← 索引文档（session 级 + project 级双写）   │
└──────────────────────────────────────────────────────────────┘
```

---

## 10. 关键交互模式详解

### 10.1 「会话开始」交互模式

**触发**：每次 Codex CLI 启动新会话。

**流程**：

```
1. Codex 启动 → 拉起 Hippocampus MCP Server
2. Codex 调用 prompt tool（自动，无需用户感知）
3. Hippocampus 返回历史摘要索引
4. Codex 将摘要注入 GPT-5.5 system prompt
5. GPT-5.5 基于历史上下文开始响应新需求
```

**用户感知**：无（完全透明）。

### 10.2 「历史细节追溯」交互模式

**触发**：用户要求参考历史代码/Schema/决策。

**流程**：

```
1. 用户："参考上午的 Schema 设计"
2. Codex 识别需要历史细节
3. Codex 先调用 summaries tool 获取所有摘要
4. LLM 识别 "Schema 设计" 匹配某个 summary_title
5. Codex 调用 retrieve tool，传入 hook_id
6. Hippocampus 返回完整 MemoryFile
7. Codex 提取相关 turn 注入 LLM 上下文
8. GPT-5.5 基于历史细节生成对齐代码
```

**用户感知**：Codex 输出「正在检索历史记忆...」+「找到 [数据库 Schema 设计] 记忆」。

### 10.3 「决策变更」交互模式

**触发**：用户改变技术方案（如换库、改配置）。

**流程**：

```
1. 用户："密码哈希从 Argon2 改为 bcrypt"
2. Codex 知道历史中记录了 Argon2
3. Codex 调用 detect_conflicts tool 预检测
4. Hippocampus 返回 ConflictReport
5. 若 has_critical=true → Codex 提示用户确认
   若 has_critical=false → Codex 自主继续
6. Codex 调用 batch_update tool 持久化冲突记录
7. Codex 基于新决策继续工作
```

**用户感知**：Codex 输出「检测到与历史记忆冲突：Argon2 → bcrypt，已记录变更」。

### 10.4 「主动归档」交互模式

**触发**：会话结束或 token 接近阈值。

**流程**：

```
1. Codex 内部 token 计数器 ≥ 阈值（或用户结束会话）
2. Codex 询问用户："是否归档当前会话？"
3. 用户确认
4. Codex 调用 archive tool，传入所有 turns
5. Hippocampus 生成 MemoryFile + IndexHook
6. Codex 记录 hook_id 到本地状态
```

**用户感知**：Codex 输出「已归档 N 个轮次，hook_id=xxx」。

---

## 11. 与场景一的差异

| 维度 | 场景一（Claude Code） | 场景六（Codex + GPT-5.5） |
|------|---------------------|--------------------------|
| Agent 工具 | Claude Code | Codex CLI |
| LLM | Claude Sonnet 4.5 | GPT-5.5 |
| 项目周期 | 4 周（持续开发） | 7 天（从零到上线） |
| 会话频率 | 每天 1-2 次 | 每天 1 次 |
| 单会话 token | 80K-600K | 120K-280K |
| token 阈值 | 400K | 200K |
| 冲突检测 | 1 次（JWT） | 1 次（Argon2→bcrypt） |
| retrieve 频次 | 1 次 | 9 次 + 1 次 batch |
| 关注点 | 4 周长期演化 | 单项目全流程 |

**结论**：场景六更聚焦于「单项目从零到上线」的短期高密度开发，验证 Hippocampus 在频繁跨会话、多次历史追溯、多次决策变更场景下的价值。

---

## 12. 关键验证点

- [x] **Codex 启动时自动调用 prompt**（§1.2）
  - 验证：首次会话返回空字符串，非首次返回历史摘要

- [x] **跨会话延续无需用户重述**（§2.3）
  - 验证：Day 1 下午基于上午的摘要直接设计 Schema

- [x] **retrieve 按需加载历史细节**（§3.3）
  - 验证：Day 2 通过 hook_id 检索 Day 1 的 Schema 设计

- [x] **冲突检测在决策变更时触发**（§3.4）
  - 验证：Argon2 → bcrypt 触发 DirectContradict（Warning 级）

- [x] **batch_retrieve 减少多次调用开销**（§6.2）
  - 验证：Day 5 一次性获取 3 个钩子编写测试

- [x] **weekly_merge 寒暄剥离**（§8.2）
  - 验证：660 turn → 592 turn，剥离 68 个寒暄

---

## 13. 推演发现的改进点

### 13.1 已记录的改进方向

| 编号 | 改进点 | 当前行为 | 期望行为 | 优先级 | 实现状态 |
|------|-------|---------|---------|-------|---------|
| IMP-06 | Agent 主动判断 retrieve 时机 | LLM 识别关键词 | 提供「相关性预筛选」接口，减少 LLM 推理 | 中 | ✅ v2.16 批次1（a92c5bd） |
| IMP-07 | prompt 渲染按相关性排序 | 按时间排序 | 按当前会话主题相关性排序 | 中 | ✅ v2.16 批次1（a92c5bd） |
| IMP-08 | batch_retrieve 并发执行 | 串行循环 | tokio 并发 + 限流 | 高 | ✅ v2.16 批次3（feb5f16） |
| IMP-09 | 冲突检测的语义级匹配 | 启发式 + LLM 可选 | 默认 LLM 语义检测 | 低 | ✅ v2.14 已实现（环境变量注入，无新代码） |
| IMP-10 | 归档时 LLM 生成更丰富 summary | 从首个 user_message 提取 | LLM 生成 abstract + key_facts | 中 | ✅ v2.16 批次2（49e63a4） |

### 13.2 风险点

| 编号 | 风险 | 影响 | 缓解措施 |
|------|-----|------|---------|
| RISK-05 | Codex token 计数与 Hippocampus 不一致 | 阈值触发不准 | Codex 主动调用 archive，Hippocampus 被动接收 |
| RISK-06 | retrieve 返回大 MemoryFile（150K+） | 注入 LLM 时截断 | Codex 应用层分页或摘要 |
| RISK-07 | 多次 retrieve 累积 token 超限 | 单会话 token 失控 | Codex 监控累计 token 并归档 |

---

## 14. 维护说明

### 14.1 文档更新触发条件

- Codex CLI 或 GPT-5.5 行为变化时
- Hippocampus MCP tool 新增或变更时
- 用户反馈真实使用流程与本推演不符时

### 14.2 关联代码引用

| 章节 | 代码位置 | 用途 |
|------|---------|------|
| §1.2 prompt | [retrieve.rs:156-240](../../crates/hippocampus-core/src/retrieve.rs#L156-L240) | render_to_system_prompt() |
| §1.4 archive | [archive.rs:138-200](../../crates/hippocampus-core/src/archive.rs#L138-L200) | Archiver::archive() |
| §3.3 retrieve | [retrieve.rs:248-269](../../crates/hippocampus-core/src/retrieve.rs#L248-L269) | retrieve_memory() |
| §3.4 detect_conflicts | [hippocampus-mcp/src/lib.rs](../../crates/hippocampus-mcp/src/lib.rs) | detect_conflicts tool |
| §3.4 batch_update | [hippocampus-mcp/src/lib.rs](../../crates/hippocampus-mcp/src/lib.rs) | batch_update tool |
| §6.2 batch_retrieve | [hippocampus-mcp/src/lib.rs](../../crates/hippocampus-mcp/src/lib.rs) | batch_retrieve tool |
| §8.2 weekly_merge | [compact.rs:136-269](../../crates/hippocampus-core/src/compact.rs#L136-L269) | Compactor::weekly_merge() |

### 14.3 真实使用复现方法

如需在 Codex CLI 中接入 Hippocampus：

1. 安装 hippocampus-mcp 二进制
2. 在 Codex 配置文件中添加 MCP Server：

```json
{
  "mcpServers": {
    "hippocampus": {
      "command": "hippocampus-mcp",
      "env": {
        "HIPPOCAMPUS_ROOT": "~/.hippocampus/blog-backend"
      }
    }
  }
}
```

3. 启动 Codex CLI，开始开发
4. Codex 会自动调用 prompt（会话开始）+ archive（会话结束）
5. 用户主动提示「参考历史」时，Codex 调用 retrieve
