# 场景一推演：AI 编程助手 MCP 集成（4 周完整演化）

> 本文档推演「场景一：AI 编程助手 MCP 集成」在 4 周时间内的完整使用流程，
> 跟踪记忆文件从生成、合并到淘汰的全生命周期，验证三级索引周期机制的有效性。
>
> **关联文档**：
> - 场景设定见 [01-scenario-design.md](./01-scenario-design.md)
> - 内部调用链见 [02-internal-call-flow.md](./02-internal-call-flow.md)

---

## 0. 推演总览

### 0.1 时间线

| 时间 | 事件 | Token 累计 | 触发动作 |
|------|------|-----------|----------|
| Day 1（周一） | 需求讨论 + 数据建模 | 80K | 首次归档（D1） |
| Day 2-3 | Product CRUD + Order 创建 | 330K | 二次归档（D2） |
| Day 5（周五） | 支付集成 + JWT 调整 | 600K | 硬上限触发截断（D3） |
| Week 1 末（周日 23:00） | 周期任务 | — | weekly_merge（3 个 daily → 1 个 weekly） |
| Week 2 | 持续开发（库存模块 + 优惠券） | 700K | 3 个 daily 文件 |
| Week 3 | 持续开发（用户权限 + 审计日志） | 650K | 3 个 daily 文件 |
| Week 4 末（月末 23:00） | 周期任务 | — | monthly_evict（3 个 weekly → 1 个 monthly） |

### 0.2 推演目标

1. **验证归档触发**：token_threshold=400K 软阈值 + force_truncate_limit=600K 硬上限的协作
2. **验证跨会话检索**：Week 2 的开发如何调用 Week 1 的 Product CRUD 细节
3. **验证 weekly_merge**：寒暄剥离 + 无损去重的实际效果
4. **验证 monthly_evict**：4 维评分淘汰 + 高价值 Turn 保留
5. **验证冲突检测**：JWT 过期时间从 1h 改为 24h 时触发 DirectContradict

---

## 1. Day 1：首次会话与归档

### 1.1 会话开始（08:00）

小林打开 Claude Code，MCP Server 被 stdio 拉起子进程。Claude Code 启动时自动调用 `prompt` tool 注入历史记忆索引。

**调用链**：[02-internal-call-flow.md §4 prompt 调用链](./02-internal-call-flow.md#4-prompt-调用链渲染-system-prompt)

```
调用方：Claude Code 启动初始化
  ↓
MCP Server: prompt tool
  ↓
Retriever::render_to_system_prompt()
  ↓
Storage::read_index(session_id="shop-backend-session", project_id="shop-backend", period=Daily/Weekly/Monthly)
  ↓
三个周期索引文档均不存在（首次会话）
  ↓
返回空字符串 ""
```

**返回结果**：`""`（空字符串）

Claude Code 收到空 prompt，知道这是首次会话，无需注入历史记忆。

### 1.2 开发过程（08:00-14:00）

小林与 Claude Code 讨论 shop-backend 需求：

- 商品表设计（PostgreSQL schema）
- 订单表设计
- 用户认证方案（JWT）
- API 路由规划

到 14:00 时累计 token 数 80K，远低于 400K 阈值。但小林要午休，主动归档保存进度。

### 1.3 主动归档（14:00）

小林通过 Claude Code 调用 `archive` tool，传入本轮所有 turns（约 40 个 MessageTurn）。

**调用链**：[02-internal-call-flow.md §1 archive 调用链](./02-internal-call-flow.md#1-archive-调用链归档)

```
调用方：Claude Code（小林点击"保存进度"）
  ↓ turns_json（40 个 MessageTurn 的 JSON 数组）
MCP Server: archive tool
  ↓
Archiver::new(config, storage, "shop-backend-session", Some("shop-backend"))
  ↓
Archiver::push_turn() × 40
  ↓ pending_turns=[40 turns], current_tokens=80000
Archiver::archive()
  ↓ 生成 MemoryFile（id=uuid-d1, period=Daily, total_tokens=80000）
Storage::write_memory()  ← 原子写入 sessions/shop-backend-session/projects/shop-backend/daily/20260704T140000_uuid-d1.json
  ↓
IndexHook::from_memory_file()
  ↓ 生成 IndexHook（hook_id=uuid-h1, memory_id=uuid-d1, summary.title="商品表设计讨论"）
Storage::append_hook()  ← session 级 daily 索引文档
  ↓
Storage::append_project_hook()  ← v2.4 双写，project 级 daily 索引文档
  ↓
返回 SummaryView JSON
```

**返回的 SummaryView**（archive tool 的返回值）：

```json
{
  "hook_id": "550e8400-e29b-41d4-a716-446655440001",
  "memory_id": "a3f5c2d1-1234-5678-9abc-def012345678",
  "summary_title": "商品表设计讨论",
  "abstract_text": null,
  "key_facts": [],
  "key_entities": [],
  "clue_anchors": [],
  "tags": ["文本消息", "代码块", "URL"],
  "archived_at": "2026-07-04T14:00:00Z",
  "period": "daily",
  "token_count": 80000,
  "is_rich": false
}
```

**Claude Code 行为**：拿到 hook_id 后，将其记录到本地会话状态，后续如需追溯可调用 `retrieve` tool。

### 1.4 午休后继续（15:00）

小林继续开发，Claude Code 在新会话开始时再次调用 `prompt` tool：

**返回的 prompt 文本**：

```markdown
# 可用记忆索引

以下是可用的历史记忆摘要，可直接基于此信息回答用户问题：

## 近期记忆（daily）

- **商品表设计讨论**[文本消息, 代码块, URL]（80000 tokens, at 2026-07-04T14:00:00Z）
  - 记忆 ID: `550e8400-e29b-41d4-a716-446655440001`

## 周度记忆（weekly）

（无）

## 月度记忆（monthly）

（无）
```

**分级渲染说明**：daily 钩子默认只显示标题行，因 tags 中含「代码块」（HIGH_VALUE_TAGS），但因 key_facts 为空（日级钩子），不展开关键事实。详见 [retrieve.rs:156-240](../../crates/hippocampus-core/src/retrieve.rs#L156-L240)。

---

## 2. Day 2-3：Product CRUD 实现

### 2.1 持续累积（Day 2 09:00 - Day 3 18:00）

小林实现 Product CRUD + Order 创建，期间不断与 Claude Code 交互：
- 创建 `Product` 模型 + SQLx 查询
- 实现 `POST /api/v1/products` 创建商品
- 实现 `GET /api/v1/products/:id` 查询商品
- 实现 Order 创建逻辑（含库存校验）

到 Day 3 18:00 时累计 330K token，仍未达 400K 阈值。小林结束工作，主动归档。

### 2.2 第二次归档（Day 3 18:00）

**归档过程**：与 Day 1 相同的调用链，生成第二个 daily 记忆文件。

**Storage 目录结构演化**：

```
sessions/
└── shop-backend-session/
    └── projects/
        └── shop-backend/
            ├── daily/
            │   ├── 20260704T140000_uuid-d1.json    ← Day 1（80K）
            │   └── 20260705T180000_uuid-d2.json    ← Day 2-3（250K）
            └── index/
                ├── daily_session.json              ← session 级 daily 索引（2 个钩子）
                └── daily_project.json              ← project 级 daily 索引（2 个钩子）
```

**daily_session.json 索引文档**（2 个钩子）：

```json
{
  "session_id": "shop-backend-session",
  "project_id": "shop-backend",
  "period": "daily",
  "hooks": [
    {
      "id": "550e8400-e29b-41d4-a716-446655440001",
      "memory_id": "a3f5c2d1-1234-5678-9abc-def012345678",
      "summary": {
        "title": "商品表设计讨论",
        "abstract_text": null,
        "key_facts": [],
        "key_entities": [],
        "clue_anchors": []
      },
      "tags": ["Text", "CodeBlock", "URL"],
      "archived_at": "2026-07-04T14:00:00Z",
      "period": "Daily",
      "token_count": 80000
    },
    {
      "id": "550e8400-e29b-41d4-a716-446655440002",
      "memory_id": "b4g6d3e2-2345-6789-abcd-ef1234567890",
      "summary": {
        "title": "Product CRUD 实现",
        "abstract_text": null,
        "key_facts": [],
        "key_entities": [],
        "clue_anchors": []
      },
      "tags": ["Text", "CodeBlock", "ToolCall"],
      "archived_at": "2026-07-05T18:00:00Z",
      "period": "Daily",
      "token_count": 250000
    }
  ]
}
```

---

## 3. Day 5：硬上限触发截断

### 3.1 支付集成开发（Day 4 09:00 - Day 5 16:00）

Day 4-5 小林开发支付集成 + 调整 JWT 配置：
- 接入 Stripe 支付网关
- 实现 `POST /api/v1/orders/:id/pay` 支付接口
- 调整 JWT 过期时间从 1 小时改为 24 小时（用户反馈频繁登录）
- 处理支付回调 webhook

Day 5 16:00 时累计 token 达到 600K，触发硬上限。

### 3.2 硬上限截断机制

**调用链**：[archive.rs:138-200](../../crates/hippocampus-core/src/archive.rs#L138-L200)

```rust
// Archiver::archive() 关键逻辑
let was_over_limit = self.current_tokens >= self.config.force_truncate_limit;
// ... 生成 MemoryFile ...
if was_over_limit {
    memory_file.mark_truncated();  // 设置 truncated=true
}
```

**触发条件**：
- `current_tokens=600000 >= force_truncate_limit=600000` → `was_over_limit=true`
- 不等待当前轮次完成（硬上限优先级高于 wait_for_turn_completion）

**返回的 SummaryView**（注意 `tags` 中多了「状态」，因为含 truncated 标记）：

```json
{
  "hook_id": "550e8400-e29b-41d4-a716-446655440003",
  "memory_id": "c5h7e4f3-3456-789a-bcde-f23456789012",
  "summary_title": "支付集成 + JWT 调整",
  "abstract_text": null,
  "key_facts": [],
  "key_entities": [],
  "clue_anchors": [],
  "tags": ["文本消息", "代码块", "工具调用", "状态"],
  "archived_at": "2026-07-08T16:00:00Z",
  "period": "daily",
  "token_count": 600000,
  "is_rich": false
}
```

### 3.3 MemoryFile 中的 truncated 字段

写入 Storage 的 MemoryFile JSON 中包含：

```json
{
  "id": "c5h7e4f3-3456-789a-bcde-f23456789012",
  "schema_version": 1,
  "archived_at": "2026-07-08T16:00:00Z",
  "session_id": "shop-backend-session",
  "project_id": "shop-backend",
  "turns": [...],
  "tags": ["Text", "CodeBlock", "ToolCall", "Status"],
  "total_tokens": 600000,
  "truncated": true,
  "period": "Daily",
  "access_count": 0,
  "importance": 0
}
```

**意义**：`truncated=true` 标记该记忆文件因超硬上限被强制截断，后续 LLM 检索时可知此文件不完整。

---

## 4. Day 5：冲突检测场景（JWT 过期时间变更）

### 4.1 场景描述

Day 5 下午，小林发现 Week 1 Day 1 的记忆文件中记录了「JWT 过期时间 = 1 小时」，但现在他决定改为 24 小时。在归档本轮（含新决策）前，先调用 `detect_conflicts` tool 预检测冲突。

### 4.2 调用 detect_conflicts tool

**入参**：

```json
{
  "session_id": "shop-backend-session",
  "hook_id": "550e8400-e29b-41d4-a716-446655440001",
  "added_facts": ["JWT 过期时间 = 24 小时"],
  "revised_facts": ["JWT 过期时间 = 1 小时 → 24 小时"],
  "deprecated_facts": [],
  "project_id": "shop-backend"
}
```

**调用链**：

```
MCP Server: detect_conflicts tool
  ↓
HeuristicDetector::detect(...)  ← 默认启发式检测器（无注入 detector 时降级）
  ↓
遍历 hook_id 对应的 MemoryFile，提取既有事实
  ↓
对 (kind, new_fact) 元组去重
  ↓
逐对比较 added_facts 与既有事实
  ↓
"JWT 过期时间 = 24 小时" vs 既有 "JWT 过期时间 = 1 小时"
  ↓
反义词词典匹配：无反义词触发
  ↓
数字差异检测：1 vs 24 → 数字不同
  ↓
模式匹配：「过期时间」「过期」「过期时间 = X」模式命中
  ↓
报告 ConflictReport { kind: DirectContradict, ... }
```

### 4.3 返回的 ConflictReport

```json
{
  "has_conflicts": true,
  "has_critical": true,
  "conflicts": [
    {
      "kind": "DirectContradict",
      "existing_fact": "JWT 过期时间 = 1 小时",
      "new_fact": "JWT 过期时间 = 24 小时",
      "reason": "数字差异：1 vs 24，模式「过期时间」匹配",
      "severity": "Critical"
    }
  ]
}
```

### 4.4 Agent 决策

Claude Code 收到冲突报告后，根据 `has_critical=true` 决定：

1. **不直接归档**：先提示小林确认决策
2. **小林确认**：「是的，改为 24 小时，1 小时已废弃」
3. **Agent 调用 batch_update**：传入 `deprecated_facts=["JWT 过期时间 = 1 小时"]`
4. **归档新轮次**：含「JWT 过期时间 = 24 小时」的新决策

**冲突记录持久化**：随 `MemoryUpdateRecord` 保存到 hook_id 对应的 MemoryFile，不丢失历史演进。

---

## 5. Week 1 末：weekly_merge 执行

### 5.1 触发时机（周日 23:00）

小林配置的定时任务触发 `compaction` tool，period="weekly"。

**调用链**：[02-internal-call-flow.md §5.2 weekly_merge](./02-internal-call-flow.md#52-weekly_merge-调用链)

### 5.2 执行过程

#### 步骤 1：列出所有 daily 记忆文件

```rust
let daily_paths = self.storage.list_memories(
    "shop-backend-session",
    Some("shop-backend"),
    ArchivePeriod::Daily
).await?;
// 结果：3 个文件路径
// - 20260704T140000_uuid-d1.json (80K)
// - 20260705T180000_uuid-d2.json (250K)
// - 20260708T160000_uuid-d3.json (600K，truncated)
```

#### 步骤 2：读取并过滤寒暄 turn

```rust
let mut all_turns = Vec::new();
let mut removed_count = 0;

for path in &daily_paths {
    let file = self.storage.read_memory(path).await?;
    for turn in &file.turns {
        if Self::is_chitchat(turn) {
            removed_count += 1;  // 寒暄剥离
        } else {
            all_turns.push(turn.clone());
        }
    }
}
```

**寒暄剥离统计**：

| 来源 | 总 turn 数 | 寒暄 turn 数 | 保留 turn 数 |
|------|-----------|-------------|-------------|
| uuid-d1（80K） | 40 | 6 | 34 |
| uuid-d2（250K） | 120 | 12 | 108 |
| uuid-d3（600K） | 280 | 25 | 255 |
| **合计** | **440** | **43** | **397** |

**被剥离的寒暄示例**：
- 「你好」「嗯」「哦」「好的」「谢谢」「再见」「收到」「明白」「对」「是的」

#### 步骤 3：生成合并后的 MemoryFile（Weekly）

```rust
let merged_memory = MemoryFile::new(
    "shop-backend-session".to_string(),
    Some("shop-backend".to_string()),
    all_turns,  // 397 个 turn
    ArchivePeriod::Weekly,
);
```

**merged_memory 概况**：
- `id`: `uuid-w1`（新 UUID）
- `total_tokens`: 880000（80K + 250K + 600K - 寒暄部分约 30K ≈ 880K）
- `tags`: `["Text", "CodeBlock", "URL", "ToolCall", "Status"]`（3 个 daily 的并集）
- `truncated`: `false`（合并后不继承子文件的 truncated 标记）
- `period`: `Weekly`

#### 步骤 4：写入 Storage

```
sessions/shop-backend-session/projects/shop-backend/weekly/20260710T230000_uuid-w1.json
```

#### 步骤 5：合并索引文档

读取 daily 索引文档（3 个钩子），生成 weekly 索引文档（1 个钩子，但 summary 更丰富）。

```rust
let abstract_text = Some(format!("本周合并了 {} 个日级记忆：{}",
    daily_doc.hooks.len(),
    daily_doc.hooks.iter().map(|h| h.summary.title.clone())
        .collect::<Vec<_>>().join("；")));
// "本周合并了 3 个日级记忆：商品表设计讨论；Product CRUD 实现；支付集成 + JWT 调整"
```

#### 步骤 6：写入 weekly 索引

**weekly_session.json 索引文档**：

```json
{
  "session_id": "shop-backend-session",
  "project_id": "shop-backend",
  "period": "weekly",
  "hooks": [
    {
      "id": "660f9511-f30c-52e5-b827-557766551111",
      "memory_id": "uuid-w1",
      "summary": {
        "title": "周度合并（3 个记忆）",
        "abstract_text": "本周合并了 3 个日级记忆：商品表设计讨论；Product CRUD 实现；支付集成 + JWT 调整",
        "key_facts": [
          "商品表设计讨论",
          "Product CRUD 实现",
          "支付集成 + JWT 调整"
        ],
        "key_entities": ["CodeBlock", "Text", "URL", "ToolCall", "Status"],
        "clue_anchors": []
      },
      "tags": ["Text", "CodeBlock", "URL", "ToolCall", "Status"],
      "archived_at": "2026-07-10T23:00:00Z",
      "period": "Weekly",
      "token_count": 880000
    }
  ]
}
```

### 5.3 weekly_merge 后的 Storage 目录

```
sessions/
└── shop-backend-session/
    └── projects/
        └── shop-backend/
            ├── daily/                                    ← daily 文件保留（不删除）
            │   ├── 20260704T140000_uuid-d1.json
            │   ├── 20260705T180000_uuid-d2.json
            │   └── 20260708T160000_uuid-d3.json
            ├── weekly/
            │   └── 20260710T230000_uuid-w1.json         ← 新生成
            └── index/
                ├── daily_session.json                   ← 保留（3 个钩子）
                ├── daily_project.json
                ├── weekly_session.json                  ← 新生成（1 个钩子）
                └── weekly_project.json
```

**设计要点**：daily 文件保留不删除，方便回溯。retrieve 时优先匹配 weekly 钩子，但 daily 钩子仍可检索。

### 5.4 prompt 渲染变化

Week 2 周一早上小林开始新会话，调用 `prompt` tool：

**返回的 prompt 文本**（分级渲染）：

```markdown
# 可用记忆索引

以下是可用的历史记忆摘要，可直接基于此信息回答用户问题：

## 近期记忆（daily）

- **商品表设计讨论**[文本消息, 代码块, URL]（80000 tokens, at 2026-07-04T14:00:00Z）
  - 记忆 ID: `550e8400-e29b-41d4-a716-446655440001`
- **Product CRUD 实现**[文本消息, 代码块, 工具调用]（250000 tokens, at 2026-07-05T18:00:00Z）
  - 记忆 ID: `550e8400-e29b-41d4-a716-446655440002`
- **支付集成 + JWT 调整**[文本消息, 代码块, 工具调用, 状态]（600000 tokens, at 2026-07-08T16:00:00Z）
  - 记忆 ID: `550e8400-e29b-41d4-a716-446655440003`

## 周度记忆（weekly）

- **周度合并（3 个记忆）**[文本消息, 代码块, URL, 工具调用, 状态]（880000 tokens, at 2026-07-10T23:00:00Z）
  - 记忆 ID: `660f9511-f30c-52e5-b827-557766551111`
  - 摘要：本周合并了 3 个日级记忆：商品表设计讨论；Product CRUD 实现；支付集成 + JWT 调整
  - 关键事实：
    - 商品表设计讨论
    - Product CRUD 实现
    - 支付集成 + JWT 调整
  - 关键实体：CodeBlock, Text, URL, ToolCall, Status

## 月度记忆（monthly）

（无）
```

**渲染规则**：weekly 钩子 `is_rich=true`（因含 abstract_text），自动展开 abstract + key_facts + key_entities。详见 [retrieve.rs:156-240](../../crates/hippocampus-core/src/retrieve.rs#L156-L240)。

---

## 6. Week 2-3：持续累积

### 6.1 Week 2 开发内容

- 库存模块（Stock model + 库存校验逻辑）
- 优惠券系统（Coupon model + 折扣计算）
- 订单状态机（pending → paid → shipped → completed）

每周生成 3 个 daily 文件，累计约 700K token。Week 2 末触发 weekly_merge，生成第 2 个 weekly 文件。

### 6.2 Week 3 开发内容

- 用户权限系统（RBAC + role/permission 表）
- 审计日志（audit_log 表 + 中间件记录）
- API 限流（tower::limit + Redis 计数）

每周生成 3 个 daily 文件，累计约 650K token。Week 3 末触发 weekly_merge，生成第 3 个 weekly 文件。

### 6.3 跨会话检索示例（Week 2 周二）

小林在 Week 2 实现库存校验时，需要参考 Week 1 的 Order 创建逻辑（库存扣减部分）。

**调用链**：[02-internal-call-flow.md §2 retrieve 调用链](./02-internal-call-flow.md#2-retrieve-调用链检索)

```
小林："参考上周 Order 创建的库存扣减逻辑"
  ↓
Claude Code 先调用 summaries tool 获取所有摘要
  ↓
Retriever::get_summaries() → 返回 6 个 SummaryView（3 daily + 1 weekly + 0 monthly，Week 2 的 daily 已有 1 个）
  ↓
LLM 识别 "Order 创建" 匹配 summary_title="Product CRUD 实现"
  ↓
Claude Code 调用 retrieve tool，传入 hook_id="550e8400-e29b-41d4-a716-446655440002"
  ↓
Retriever::retrieve_memory("550e8400-...-446655440002")
  ↓ 遍历 daily 索引（找到）→ 读取 uuid-d2.json
  ↓
返回完整 MemoryFile JSON（含 120 个 turn，250K token）
  ↓
Claude Code 提取库存扣减相关 turn（约 8 个 turn，15K token）注入上下文
  ↓
基于历史细节实现新的库存校验逻辑
```

**关键点**：retrieve 返回完整 MemoryFile，但 Claude Code 只提取相关部分注入 LLM 上下文，避免 250K token 全部塞入。

### 6.4 access_count 自增

每次 retrieve 成功，对应的 MemoryFile `access_count` 应自增（v2 路线图，当前实现需应用层维护）。

Week 2-3 期间各 weekly 文件的 access_count 演化：

| 记忆文件 | Week 2 末 access_count | Week 3 末 access_count | Week 4 末 access_count |
|---------|----------------------|----------------------|----------------------|
| uuid-w1（Week 1） | 5 | 8 | 10 |
| uuid-w2（Week 2） | 0 | 3 | 6 |
| uuid-w3（Week 3） | — | 0 | 2 |

---

## 7. Week 4 末：monthly_evict 执行

### 7.1 触发时机（月末 23:00）

7 月 31 日 23:00，定时任务触发 `compaction` tool，period="monthly"。

**调用链**：[02-internal-call-flow.md §5.3 monthly_evict](./02-internal-call-flow.md#53-monthly_evict-调用链)

### 7.2 执行过程

#### 步骤 1：列出所有 weekly 记忆文件

```rust
let weekly_paths = self.storage.list_memories(
    "shop-backend-session",
    Some("shop-backend"),
    ArchivePeriod::Weekly
).await?;
// 结果：3 个文件路径
// - 20260710T230000_uuid-w1.json (880K, archived_at=2026-07-10)
// - 20260717T230000_uuid-w2.json (700K, archived_at=2026-07-17)
// - 20260724T230000_uuid-w3.json (650K, archived_at=2026-07-24)
```

#### 步骤 2：4 维评分

**评分公式**：[score.rs:152-168](../../crates/hippocampus-core/src/score.rs#L152-L168)

```rust
// 时效性：score = 100 * 0.5^(age_days / 7)
// 访问频率：score = (access_count / 10) * 100
// 用户显式标记：score = importance（0-100）
// 主题相关性：v2 实现，当前固定 50（权重 0 不影响）
```

**评分计算**（参考时间 2026-07-31 23:00）：

| 记忆文件 | archived_at | age_days | 时效性 | access_count | 访问频率 | importance | 用户标记 | 总分 |
|---------|------------|----------|-------|-------------|---------|------------|---------|------|
| uuid-w1 | 2026-07-10 | 21 | 100×0.5^(21/7)=12.5 | 10 | 100 | 60 | 60 | (12.5+100+60)/3=57.5 |
| uuid-w2 | 2026-07-17 | 14 | 100×0.5^(14/7)=25.0 | 6 | 60 | 50 | 50 | (25+60+50)/3=45.0 |
| uuid-w3 | 2026-07-24 | 7 | 100×0.5^(7/7)=50.0 | 2 | 20 | 40 | 40 | (50+20+40)/3=36.7 |

**评分权重**（默认）：timeliness=1/3, access_frequency=1/3, user_marked=1/3, topic_relevance=0

**评分结果**：
- uuid-w1: **57.5**（最高分，选为主记忆）
- uuid-w2: 45.0
- uuid-w3: 36.7

#### 步骤 3：选主记忆 + 高价值 Turn 保留

```rust
// 主记忆：uuid-w1（Week 1，含商品表设计 + Product CRUD + 支付集成）
let (mut main_memory, main_score) = scored.remove(0);

// 从 uuid-w2、uuid-w3 中提取高价值 Turn
let mut high_value_turns = Vec::new();
for (file, _) in &scored {
    for turn in &file.turns {
        if Self::is_high_value_turn(turn) {
            high_value_turns.push(turn.clone());
        }
    }
}
```

**高价值 Turn 判定**：[compact.rs:414-431](../../crates/hippocampus-core/src/compact.rs#L414-L431)

含以下标签的 turn 被保留：
- `Tag::ToolCall`（工具调用信息）
- `Tag::Thinking`（思考过程）
- `Tag::AgentTool`（Agent 工具使用记录）
- `Tag::CodeBlock`（代码块）
- `Tag::FileAttachment`（附件信息）
- `Tag::Image`（图片）
- `Tag::Video`（视频）

**高价值 Turn 提取统计**：

| 来源 | 总 turn 数 | 高价值 turn 数 | 保留 token 数 |
|------|-----------|--------------|-------------|
| uuid-w2（库存 + 优惠券） | 350 | 85 | 180K |
| uuid-w3（权限 + 审计 + 限流） | 320 | 72 | 150K |
| **合计** | — | **157** | **330K** |

#### 步骤 4：生成 monthly MemoryFile

```rust
// 追加高价值 Turn 到主记忆
for turn in high_value_turns {
    main_memory.turns.push(turn);
}
main_memory.total_tokens = main_memory.turns.iter().map(|t| t.token_count).sum();
main_memory.period = ArchivePeriod::Monthly;
```

**main_memory 概况**（monthly）：
- `id`: `uuid-m1`（新 UUID）
- `turns`: 397（Week 1 主记忆）+ 157（高价值）= **554 个 turn**
- `total_tokens`: 880K + 330K = **1.21M**
- `tags`: `["Text", "CodeBlock", "URL", "ToolCall", "Status", "Plan"]`（并集）
- `period`: `Monthly`

#### 步骤 5：写入 Storage + 合并索引

```
sessions/shop-backend-session/projects/shop-backend/monthly/20260731T230000_uuid-m1.json
```

**monthly_session.json 索引文档**：

```json
{
  "session_id": "shop-backend-session",
  "project_id": "shop-backend",
  "period": "monthly",
  "hooks": [
    {
      "id": "770f0622-g41d-63f6-c938-668877662222",
      "memory_id": "uuid-m1",
      "summary": {
        "title": "2026-07: 电商后台开发",
        "abstract_text": "本月合并了 3 个周级记忆：周度合并（3 个记忆）；周度合并（库存+优惠券+状态机）；周度合并（权限+审计+限流）",
        "key_facts": [
          "商品表设计讨论",
          "Product CRUD 实现",
          "支付集成 + JWT 调整",
          "库存模块",
          "优惠券系统",
          "用户权限系统",
          "审计日志",
          "API 限流"
        ],
        "key_entities": ["CodeBlock", "Text", "URL", "ToolCall", "Status", "Plan"],
        "clue_anchors": ["JWT", "PostgreSQL", "Axum", "Stripe", "RBAC", "Redis"]
      },
      "tags": ["Text", "CodeBlock", "URL", "ToolCall", "Status", "Plan"],
      "archived_at": "2026-07-31T23:00:00Z",
      "period": "Monthly",
      "token_count": 1210000
    }
  ]
}
```

### 7.3 monthly_evict 后的 Storage 目录

```
sessions/
└── shop-backend-session/
    └── projects/
        └── shop-backend/
            ├── daily/      ← 保留（9 个文件，可追溯）
            ├── weekly/     ← 保留（3 个文件，可追溯）
            ├── monthly/
            │   └── 20260731T230000_uuid-m1.json    ← 新生成
            └── index/
                ├── daily_session.json              ← 保留
                ├── weekly_session.json             ← 保留
                └── monthly_session.json            ← 新生成（1 个钩子）
```

**设计要点**：monthly_evict 不删除 weekly 文件，只是合并生成 monthly。但 retrieve 时优先匹配 monthly 钩子。

### 7.4 prompt 渲染变化

8 月 1 日早上小林开始新会话，调用 `prompt` tool：

**返回的 prompt 文本**：

```markdown
# 可用记忆索引

以下是可用的历史记忆摘要，可直接基于此信息回答用户问题：

## 近期记忆（daily）

（无，8 月新会话）

## 周度记忆（weekly）

- **周度合并（3 个记忆）**[...]（880000 tokens, at 2026-07-10T23:00:00Z）
  - 记忆 ID: `660f9511-f30c-52e5-b827-557766551111`
  - 摘要：本周合并了 3 个日级记忆：...
- **周度合并（库存+优惠券+状态机）**[...]（700000 tokens, at 2026-07-17T23:00:00Z）
  - 记忆 ID: `660f9511-f30c-52e5-b827-557766552222`
- **周度合并（权限+审计+限流）**[...]（650000 tokens, at 2026-07-24T23:00:00Z）
  - 记忆 ID: `660f9511-f30c-52e5-b827-557766553333`

## 月度记忆（monthly）

- **2026-07: 电商后台开发**[文本消息, 代码块, URL, 工具调用, 状态, 计划]（1210000 tokens, at 2026-07-31T23:00:00Z）
  - 记忆 ID: `770f0622-g41d-63f6-c938-668877662222`
  - 摘要：本月合并了 3 个周级记忆：周度合并（3 个记忆）；周度合并（库存+优惠券+状态机）；周度合并（权限+审计+限流）
  - 关键事实：
    - 商品表设计讨论
    - Product CRUD 实现
    - 支付集成 + JWT 调整
    - 库存模块
    - 优惠券系统
    - 用户权限系统
    - 审计日志
    - API 限流
  - 关键实体：CodeBlock, Text, URL, ToolCall, Status, Plan
  - 线索锚点：JWT, PostgreSQL, Axum, Stripe, RBAC, Redis
```

**渲染规则**：monthly 钩子 `is_rich=true`，全展开（abstract + key_facts + key_entities + clue_anchors）。详见 [retrieve.rs:156-240](../../crates/hippocampus-core/src/retrieve.rs#L156-L240)。

---

## 8. 4 周记忆演化总结

### 8.1 记忆文件演化表

| 阶段 | daily 文件 | weekly 文件 | monthly 文件 | 总 token | 总 turn |
|------|-----------|------------|-------------|---------|---------|
| Week 1 末 | 3（930K） | 1（880K） | 0 | 1.81M | 440→397（剥离 43 寒暄） |
| Week 2 末 | 6（1.63M） | 2（1.58M） | 0 | 3.21M | 790→710（剥离 80 寒暄） |
| Week 3 末 | 9（2.28M） | 3（2.23M） | 0 | 4.51M | 1110→1000（剥离 110 寒暄） |
| Week 4 末 | 9（2.28M） | 3（2.23M） | 1（1.21M） | 5.72M | 1000→554（淘汰 446 低价值） |

### 8.2 LLM 上下文负载对比

| 场景 | 无 Hippocampus | 有 Hippocampus（仅 prompt） | 有 Hippocampus（prompt + 按需 retrieve） |
|------|---------------|--------------------------|---------------------------------------|
| Week 1 首次会话 | 0 token | 0 token（空 prompt） | 0 token |
| Week 2 周二 | 1.63M token（全量历史） | ~500 token（6 个摘要行） | ~15K token（1 个 retrieve + 摘要） |
| Week 4 末（月末） | 4.51M token（超出窗口） | ~800 token（13 个摘要行） | ~15K token（1 个 retrieve + 摘要） |

**结论**：
- **无 Hippocampus**：4 周后历史 token 远超任何 LLM 上下文窗口（4.51M）
- **仅 prompt**：始终 <1K token，但只有摘要级信息
- **prompt + retrieve**：按需加载，单次 ~15K token，完美适配 200K 上下文窗口

### 8.3 三级索引周期效果

| 周期 | 操作 | 输入 | 输出 | 效果 |
|------|------|------|------|------|
| 天级 | 归档（archive） | 1 轮会话 turns | 1 个 daily 文件 | 完整保真归档，非摘要 |
| 周级 | 合并（weekly_merge） | N 个 daily 文件 | 1 个 weekly 文件 | 寒暄剥离 + 无损去重 |
| 月级 | 淘汰（monthly_evict） | N 个 weekly 文件 | 1 个 monthly 文件 | 评分选主 + 高价值保留 |

### 8.4 关键指标对比

| 指标 | Week 1 末 | Week 4 末（monthly 后） | 变化 |
|------|----------|----------------------|------|
| 记忆文件总数 | 4（3 daily + 1 weekly） | 13（9 daily + 3 weekly + 1 monthly） | +9 |
| 总 token 数 | 1.81M | 5.72M | +216% |
| LLM 上下文 prompt token | ~200 | ~800 | +300% |
| 检索 1 次的 token 开销 | ~15K（单个 daily） | ~15K（单个 monthly） | 持平 |
| 寒暄剥离数 | 43 | 110 | +67 |
| 高价值 Turn 保留数 | — | 157 | — |

---

## 9. 关键验证点回归

### 9.1 验证点清单

- [x] **首次会话 `prompt` 返回空字符串**（§1.1）
  - 验证：三个周期索引文档均不存在 → 返回 `""`

- [x] **跨会话 `retrieve` 按需加载历史细节**（§6.3）
  - 验证：Week 2 通过 hook_id 检索 Week 1 的 Product CRUD 完整 MemoryFile

- [x] **硬上限截断标记 `truncated=true`**（§3.2）
  - 验证：600K token 触发 `force_truncate_limit`，MemoryFile 中 `truncated=true`

- [x] **weekly_merge 寒暄剥离 + 无损去重**（§5.2）
  - 验证：440 turn → 397 turn，剥离 43 个寒暄 turn

- [x] **monthly_evict 4 维评分淘汰 + 高价值 Turn 保留**（§7.2）
  - 验证：3 个 weekly 评分选 uuid-w1 为主，从 w2/w3 保留 157 个高价值 turn

- [x] **冲突检测（JWT 过期时间变更触发 DirectContradict）**（§4）
  - 验证：detect_conflicts 返回 `has_critical=true`，kind=`DirectContradict`

### 9.2 未验证但已设计的能力

- [ ] **多 Agent 通过 project_id 共享记忆**（场景三）
- [ ] **CachedStorage 缓存命中率**（场景二）
- [ ] **HybridRetriever 语义检索效果**（场景三）
- [ ] **WASM 组件嵌入浏览器**（v2.16+ 路线图）

---

## 10. 推演发现的潜在改进点

### 10.1 已记录的改进方向

| 编号 | 改进点 | 当前行为 | 期望行为 | 优先级 | 实现状态 |
|------|-------|---------|---------|-------|---------|
| IMP-01 | access_count 自增 | 应用层维护 | retrieve 成功后自动自增 | 中 | ✅ v2.16 批次1（a92c5bd） |
| IMP-02 | daily 文件清理 | 永不删除（可追溯） | 提供 `cleanup_daily` 配置项 | 低 | ✅ v2.16 批次3（feb5f16） |
| IMP-03 | 评分维度扩展 | 3 维（topic_relevance=0） | 接入 LLM 实现主题相关性 | 中 | ✅ v2.16 批次2（49e63a4） |
| IMP-04 | 寒暄词典扩展 | 内置 ~20 个模式 | 支持用户自定义词典 | 低 | ✅ v2.16 批次1（a92c5bd） |
| IMP-05 | 月级 clue_anchors 生成 | 从 tags 提取 | LLM 生成更精准锚点 | 中 | ✅ v2.16 批次2（49e63a4） |

### 10.2 风险点

| 编号 | 风险 | 影响 | 缓解措施 |
|------|-----|------|---------|
| RISK-01 | monthly 文件过大（1.21M token） | retrieve 时返回数据量大 | 应用层分页或流式返回 |
| RISK-02 | weekly 文件长期累积 | 存储空间增长 | 提供 `purge_weekly_after_monthly` 选项 |
| RISK-03 | 寒暄剥离误判 | 误删有效 turn | 「嗯，这里有问题」可能被误判，需扩展规则 |
| RISK-04 | 评分权重固定 | 不同场景需求不同 | 暴露 `ScoreWeights` 配置项 |

---

## 11. 维护说明

### 11.1 文档更新触发条件

- 推演场景的实际验证结果与文档不符时
- 新增周期任务或检索机制时
- 评分维度扩展（如接入 LLM topic_relevance）时
- 冲突检测逻辑变更时

### 11.2 关联代码引用

| 章节 | 代码位置 | 用途 |
|------|---------|------|
| §1 归档 | [archive.rs:138-200](../../crates/hippocampus-core/src/archive.rs#L138-L200) | Archiver::archive() |
| §2 检索 | [retrieve.rs:248-269](../../crates/hippocampus-core/src/retrieve.rs#L248-L269) | Retriever::retrieve_memory() |
| §3 硬上限 | [archive.rs:138-200](../../crates/hippocampus-core/src/archive.rs#L138-L200) | was_over_limit 判定 |
| §4 冲突检测 | [conflict.rs](../../crates/hippocampus-core/src/conflict.rs) | ConflictDetector trait |
| §5 weekly_merge | [compact.rs:136-269](../../crates/hippocampus-core/src/compact.rs#L136-L269) | Compactor::weekly_merge() |
| §7 monthly_evict | [compact.rs:285-404](../../crates/hippocampus-core/src/compact.rs#L285-L404) | Compactor::monthly_evict() |
| §7 评分 | [score.rs:152-168](../../crates/hippocampus-core/src/score.rs#L152-L168) | DefaultScorer::score() |

### 11.3 复现推演的方法

如需在测试中复现本推演：

1. 使用 `LocalStorage` 创建临时目录
2. 按 Day 1-5 的时间线调用 `archiver.archive()` 生成 3 个 daily 文件
3. 调用 `compactor.weekly_merge()` 生成 weekly 文件
4. 按 Week 2-3 时间线继续归档 + weekly_merge
5. 调用 `compactor.monthly_evict()` 验证评分淘汰
6. 用 `retriever.get_summaries()` 验证索引演化
7. 用 `retriever.render_to_system_prompt()` 验证 prompt 渲染

参考测试：[crates/hippocampus-core/tests/integration_test.rs](../../crates/hippocampus-core/tests/integration_test.rs)
