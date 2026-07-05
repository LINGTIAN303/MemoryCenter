# 变更历史

本项目遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。变更格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)。

## [Unreleased]

### 计划中
- v2.4：WASM 组件（待生态成熟）+ Node/Go/Java 绑定

### v2.29 - Presets Create 全链路落地（2026-07-05）

#### 背景
v2.21 引入 PresetBuilder + 5 Profile 联动机制后，CombinedProfile 一直未被 archive/compaction 实际消费（仍用 `ArchiveConfig::default()` + 固定模板）。本版本让 PresetBuilder 真正影响 archive 行为，覆盖 core / HTTP API / MCP / Python 4 个层面。

#### 核心机制
- **预设生命周期**：即时计算（不持久化，无状态）
- **应用方式**：archive 内联参数（请求体新增可选 `preset` 字段，服务端 build 后应用 `archive_threshold` + `summary_template`）
- **优先级链**：用户 > scenario > model > 默认 400K
- **DRY 公共函数**：`hippocampus_presets::build_from_strings` 被 server / mcp / python 三端共享

#### 变更

##### 阶段 1：core 改造
- **`SummaryGenerator` trait**（`crates/hippocampus-core/src/generate.rs`）
  - 新增默认方法 `generate_summary_with_template(file, template)`，默认实现忽略 template 调用 `generate_summary`（向后兼容）
- **`Archiver` / `Compactor`**（`crates/hippocampus-core/src/archive.rs` / `compact.rs`）
  - 新增 `summary_template_override: Option<String>` 字段 + `with_summary_template_override` builder
  - `archive()` / `compaction` 根据 override 选择调用 `generate_summary_with_template(Some(tpl))` 或 `generate_summary()`
- **`HttpSummaryGenerator`**（`crates/hippocampus-llm/src/summary_generator.rs`）
  - 覆盖 `generate_summary_with_template`，提取公共 `call_llm` 方法

##### 阶段 2：HTTP API
- **`crates/hippocampus-server/src/presets.rs`**（新文件）
  - 4 个端点：`POST /api/v1/presets/build` / `GET /presets/agents` / `GET /presets/scenarios` / `GET /presets/models`
  - 公共函数 `build_combined_from_request` 复用 `build_from_strings`
- **`crates/hippocampus-server/src/handlers.rs`**
  - `ArchiveRequest` 新增 `preset: Option<PresetRequest>` 字段
  - archive handler 应用 `archive_threshold`（覆盖 `ArchiveConfig.token_threshold` + `force_truncate_limit` 按 3/2 比例放大）+ `summary_template_override`

##### 阶段 3：MCP 工具
- **`crates/hippocampus-presets/src/builder.rs`**
  - 新增公共函数 `build_from_strings` + `scenario_from_str`（供 server / mcp / python 复用）
- **`crates/hippocampus-mcp/src/lib.rs`**
  - `ArchiveParams` 新增 `preset: Option<PresetParams>` 字段（派生 Default 向后兼容）
  - `archive` tool 改造：解析 preset → build_from_strings → 应用 archive_threshold + summary_template_override
  - 新增 4 个 preset_* tool：`preset_build` / `preset_list_agents` / `preset_list_scenarios` / `preset_list_models`
  - 13 个新增测试覆盖 preset 应用链路

##### 阶段 4：Python 绑定
- **`crates/hippocampus-python/Cargo.toml`**：新增 3 个依赖（hippocampus-models / -windows / -skills）
- **`crates/hippocampus-python/src/lib.rs`**
  - 删除本地 `scenario_from_str`，改用 `hippocampus_presets::scenario_from_str`（DRY）
  - 新增辅助函数 `window_scheme_from_str`（6 种窗口预设名解析）
  - 新增辅助函数 `build_combined_from_preset`（Python dict → CombinedProfile）
  - `PyPresetBuilder` 补齐 4 个方法：`with_model` / `with_window` / `with_skill` / `with_skills`
  - `PyPresetBuilder.build()` 返回更完整字段：`model_name` / `window_scheme` / `window_trigger_threshold` / `skills`
  - `Hippocampus.archive()` 新增可选 `preset` 参数（`#[pyo3(signature = (turns, preset=None))]`）
  - 新增 3 个模块函数：`supported_models()` / `supported_skills()` / `supported_windows()`
  - 10 个新增测试覆盖 with_model / with_window / with_skill / with_skills / build_from_strings / archive with preset

#### 测试
- hippocampus-python：19 测试通过（含 10 个新增）
- hippocampus-mcp：38 测试通过（含 13 个新增）
- hippocampus-presets：21 测试通过（含 2 个新增公共函数测试）
- 整个 workspace：426+ 测试通过，0 失败

#### 兼容性
- **HTTP API**：`ArchiveRequest.preset` 为 `Option`，旧请求不传 `preset` 字段保持原行为
- **MCP**：`ArchiveParams` 派生 `Default`，旧调用方用 `..Default::default()` 兼容
- **Python**：`Hippocampus.archive(turns, preset=None)` 默认 `None` 保持原行为
- **trait 默认方法**：旧 `SummaryGenerator` 实现自动忽略 template 参数

#### 涉及文件
- `crates/hippocampus-core/src/generate.rs`
- `crates/hippocampus-core/src/archive.rs`
- `crates/hippocampus-core/src/compact.rs`
- `crates/hippocampus-llm/src/summary_generator.rs`
- `crates/hippocampus-presets/src/builder.rs`
- `crates/hippocampus-presets/src/lib.rs`
- `crates/hippocampus-server/Cargo.toml`
- `crates/hippocampus-server/src/presets.rs`（新）
- `crates/hippocampus-server/src/lib.rs`
- `crates/hippocampus-server/src/handlers.rs`
- `crates/hippocampus-mcp/Cargo.toml`
- `crates/hippocampus-mcp/src/lib.rs`
- `crates/hippocampus-python/Cargo.toml`
- `crates/hippocampus-python/src/lib.rs`

### v2.28 - HybridDetector 字段级 merge 替代二选一丢弃（2026-07-05）

#### 背景
v2.27.1 标记的风险点：HybridDetector 合并 LLM 与启发式报告时，遇到 `(kind, new_fact)` 相同的冲突会直接丢弃 LLM 版本，可能丢失 LLM 在 `severity` / `description` / `existing_fact` 上的增量信息。本版本通过字段级 merge 解决。

#### 变更
- **`HybridDetector::detect`**（`crates/hippocampus-core/src/conflict.rs`）
  - 从「LLM 与启发式报告二选一丢弃 LLM」升级为「字段级 merge」
  - 当 LLM 与启发式检测到同一冲突（`kind` 相同 + `new_fact` 精确匹配或语义相似度 >= `dedup_threshold`）时
  - 字段级合并而非丢弃 LLM 版本：
    - `severity`：取更严重的（`Severity` derive `Ord`，Critical > Warning > Info）
    - `description`：优先 LLM（非空且更长时替换，避免空字符串覆盖）
    - `existing_fact`：优先 LLM（`Some` 时替换，`None` 不覆盖）
    - `kind` / `new_fact`：保持原值不变

#### 新增方法
- **`find_duplicate_index`**：返回重复冲突的索引（`Option<usize>`），替代原 `is_semantically_duplicate` 的布尔判断
  - 判定规则：`kind` 相同 + `new_fact` 精确匹配 或 语义相似度 >= `dedup_threshold`
- **`merge_conflict_fields`**：字段级合并两条冲突记录（按上述规则）

#### 测试
- 新增 9 个单元测试（`v2_28_merge_tests` 模块）：
  - 精确匹配触发 merge / 语义相似触发 merge / 不同 kind 不合并
  - severity 取 max / description 优先 LLM / existing_fact 优先 LLM
  - 空列表 / LLM 报告为空 / 启发式报告为空
- 50 个 conflict 模块测试全部通过，无回归
- `is_semantically_duplicate` 保留为公共 API（不再被 detect 调用，有 dead_code 警告可忽略）

### v2.27.1 - batch_update/update_memory key_facts 注入统一（2026-07-05）

#### 修复
- **`update_memory` key_facts 注入**（`crates/hippocampus-server/src/handlers.rs`）
  - 改用 `find_hook_by_id` 获取完整 IndexHook
  - 若 `memory.updates` 为空，从 `IndexHook.summary.key_facts` 注入虚拟 `MemoryUpdateRecord`
  - 逐条 `add_fact` 保持事实粒度（替代 `join("\n")`）
- **`batch_update` key_facts 注入**（同上文件）
  - 同样改用 `find_hook_by_id` + key_facts 注入逻辑
  - 与 `detect_conflicts` / `update_memory` 行为对齐
  - 解决批量更新时 `historical_facts` 为空导致检测失效的问题

#### 风险点（v2.28 已解决）
- **HybridDetector 去重逻辑**：合并 LLM 与启发式报告时只比较 `(kind, new_fact)`
  - 可能丢失 LLM 在 `severity` / `description` / `existing_fact` 上的增量信息
  - 启发式优先级高于 LLM，LLM 版本可能被丢弃
  - 代码无 bug，属于设计取舍 —— **v2.28 通过字段级 merge 已解决**

### v2.27 - 服务器端 detect_conflicts HTTP 端点（2026-07-05）

#### 新增
- **`POST /api/v1/sessions/{sid}/memories/{hook_id}/detect-conflicts`**
  - 检测单次记忆更新的潜在冲突（不实际写入）
  - 与 MCP 端 `detect_conflicts` tool 行为一致
  - 复用 `UpdateMemoryRequest` 请求体，返回 `ConflictsResponse`
  - 使用 `find_hook_by_id` 从 IndexHook.key_facts 注入历史事实
- **生产环境 LLM 配置脚本**（`deploy/setup-llm-env.sh`）
  - Generator: SenseNova sensenova-6.7-flash-lite
  - Detector: DeepSeek deepseek-v4-flash
  - 备份 + sed 插入 + daemon-reload + 重启

#### 端点分工
| 端点 | 方法 | 行为 |
|------|------|------|
| `.../conflicts` | GET | 查询已持久化的冲突记录 |
| `.../memories/{hook_id}` | PATCH | 实际写入更新 + 检测 + 持久化冲突 |
| `.../detect-conflicts` | POST | 仅检测，不写入（预检测） |

### v2.26 - 自动部署配置（2026-07-05）

#### 新增
- **`deploy/setup-auto-deploy.sh`**：服务器端一次性配置脚本
  - 创建裸仓库 `/root/hippocampus.git`
  - 创建 post-receive hook
- **`deploy/post-receive.sh`**：自动部署 hook
  - 流程：checkout → cargo build → stop → cp → start → verify
  - 解决 "Text file busy" 问题：先 `systemctl stop` 再 `cp` 再 `start`
- **本地 production remote**：`__REDACTED_SERVER__:/root/hippocampus.git`
- 日常部署：`git push production main`（自动触发编译+重启，约 5 分钟）

### v2.25 - Detector 检测失效修复 + LLM 思考模式（2026-07-05）

#### 修复
- **v2.24: 关闭 LLM 思考模式**
  - 3 个 LLM 客户端请求体加 `"thinking": {"type": "disabled"}`
    - `crates/hippocampus-llm/src/detector.rs`
    - `crates/hippocampus-llm/src/scorer.rs`
    - `crates/hippocampus-llm/src/summary_generator.rs`
  - 根因：DeepSeek V4 Flash 默认启用思考模式，输出进入 `reasoning_content` 而 `content` 为空
  - 对不支持 thinking 的 API（OpenAI/SenseNova）无害，会被忽略
- **v2.25: 从 IndexHook 注入 key_facts**
  - `retrieve.rs` 新增 `find_hook_by_id()` 返回完整 IndexHook
  - `detect_conflicts`（MCP 端）读取 `IndexHook.summary.key_facts`
  - 作为虚拟 `MemoryUpdateRecord` 注入到 `memory.updates`
  - 解决 archive 只写 turns 不写 updates 的设计缺陷
- **v2.25.1: 事实粒度优化**
  - 逐条 `add_fact` 替代 `join("\n")`
  - 避免多条 key_facts 被合并成 1 条粗粒度事实

#### 验证
- 修复前：`detect_conflicts` 返回 `total=0, has_critical=false`
- 修复后：`total=1, has_critical=true`，`existing_fact`/`new_fact` 均为单条精确事实

### v2.24 - API Key 鉴权中间件 + 生产部署文档（2026-07-05）

#### 新增
- **API Key 鉴权中间件**（`crates/hippocampus-server/src/middleware/auth.rs`）
  - 从 `Authorization: Bearer <key>` 头提取 API Key 比对
  - 环境变量 `HIPPOCAMPUS_API_KEY` 驱动，未配置时跳过鉴权（向后兼容）
  - 常量时间比对（避免时序侧信道攻击）
  - 错误响应：401 UNAUTHORIZED / 403 FORBIDDEN
  - 4 个单元测试（同值/异值/异长/空）
- **部署文档**（`docs/DEPLOY.md`）
  - 完整生产部署指南：编译 → systemd 守护 → Nginx 反代 → 验证
  - 含故障排查、安全建议、运维操作、API 端点速查
- **E2E 测试脚本**（`deploy/test_e2e.py`）
  - 5 项端到端验证：归档/检索/摘要/Prompt 渲染/公网反代
  - 支持命令行参数或环境变量传入 API Key
- **Nginx 配置示例**（`deploy/nginx-hippocampus.conf` + `deploy/nginx-hippo-block.conf`）

#### 变更
- `lib.rs` create_router 应用 `middleware::from_fn(require_api_key)` 到所有路由
- `main.rs` 启动时打印 API Key 鉴权状态（已启用/未启用警告）
- 中间件模块独立成 `crates/hippocampus-server/src/middleware/`（遵循工程规范第 5 条）

#### 测试
- hippocampus-server：5 lib + 44 集成 = 49 测试全通过
- 服务器 E2E：归档/检索/摘要/Prompt/公网反代 5 项 200 OK

#### 部署验证
- 服务器：162.211.183.236（openworld.dpdns.org）
- 二进制：/opt/hippocampus-server/bin/hippocampus-server（9.2MB）
- systemd：hippocampus-server.service（active running, Restart=always）
- Nginx：`/hippo/` 子路径反代到 127.0.0.1:8765
- 公网入口：https://openworld.dpdns.org/hippo/api/v1/...

### 型号库更新（2026-07-04 核查官方文档）

#### 背景
核查 Anthropic / OpenAI / Google / DeepSeek / Alibaba / Meta / xAI 官方文档与 API 公告，发现内置型号库存在 3 个过期型号，其中 1 个**紧急**（DeepSeek V3/V3.2 将于 2026-07-24 停服）。同时新增 Anthropic 2026 年 5-6 月发布的 3 个新型号（Opus 4.8 / Fable 5 / Mythos 5）。

#### 删除（7 个旧型号）
- `claude_opus_4_5`（被 Opus 4.6 / Opus 4.8 替代）
- `claude_sonnet_4_5`（被 Sonnet 5 替代）
- `gemini_3_pro`（被 Gemini 3.1 Pro 替代，2026-02-20 发布）
- `deepseek_v3_2`（**2026-07-24 停服**，迁移至 V4）
- `deepseek_r1`（被 V4-Pro 思考链模式替代）
- `qwen_3`（被 Qwen3-Coder 替代，编程优化版）
- `llama_4`（拆分为 Scout / Maverick 两个变体）

#### 新增（10 个新型号）
- `claude_opus_4_8`：2026-05 发布，200K 上下文，Opus 级稳定旗舰，API 普遍可用
- `claude_fable_5`：2026-06-10 发布，200K 上下文，Mythos 级防护版，7-02 全球恢复可用
- `claude_mythos_5`：2026-06-10 发布，200K 上下文，Mythos 级未防护版，面向特定合作方（与 Fable 5 共享底层模型）
- `claude_sonnet_5`：2026-06-30 发布，200K 上下文，Agent 默认模型，思考链
- `gemini_3_1_pro`：2026-02-20 发布，1M 上下文，推理能力 2x，ARC-AGI-2 77.1%
- `deepseek_v4_pro`：2026-04-24 发布预览版，1M 上下文，MoE 1.6T/49B 激活，MIT 开源
- `deepseek_v4_flash`：2026-04-24 发布预览版，1M 上下文，MoE 284B/13B 激活，轻量高效
- `qwen_3_coder`：2025-07-23 开源，256K 上下文（YaRN 可扩 1M），358 种编程语言
- `llama_4_scout`：2025-04 发布，1M 上下文（理论 10M），MoE 109B，轻量化
- `llama_4_maverick`：2025-04 发布，1M 上下文，MoE 400B，旗舰级

#### default_variant 映射变更
| 家族 | 旧默认 | 新默认 | 原因 |
|---|---|---|---|
| Claude | claude-opus-4.6 | **claude-opus-4.8** | API 普遍可用的稳定旗舰；Fable 5 曾因出口管制暂停，Mythos 5 面向合作方 |
| Gemini | gemini-3-pro | **gemini-3.1-pro** | 推理能力 2x |
| DeepSeek | deepseek-v3.2 | **deepseek-v4-pro** | V3.2 即将停服 |
| Qwen | qwen-3 | **qwen-3-coder** | 编程优化版 |
| Llama | llama-4 | **llama-4-scout** | 拆分变体，Scout 为轻量版 |

#### Claude 家族层级（2026-07 最新）
```text
Mythos 级（最高）: Fable 5（防护版）/ Mythos 5（未防护版）—— 共享底层模型
Opus 级（旗舰） : Opus 4.8（当前默认） / Opus 4.6
Sonnet 级（主力）: Sonnet 5
```

#### 破坏性变更
- **不向后兼容**：删除 7 个旧型号构造器，已使用旧型号的用户需迁移至新型号
- 迁移指引：
  - `claude_opus_4_5()` → `claude_opus_4_8()`（推荐）或 `claude_opus_4_6()`
  - `claude_sonnet_4_5()` → `claude_sonnet_5()`
  - `gemini_3_pro()` → `gemini_3_1_pro()`
  - `deepseek_v3_2()` / `deepseek_r1()` → `deepseek_v4_pro()`（思考链）或 `deepseek_v4_flash()`（轻量）
  - `qwen_3()` → `qwen_3_coder()`
  - `llama_4()` → `llama_4_scout()`（轻量）或 `llama_4_maverick()`（旗舰）

#### 测试
- variant.rs：15 个新型号测试，删除 4 个旧型号测试
- registry.rs：新增 3 个家族默认型号测试（DeepSeek V4 / Qwen Coder / Llama Scout）
- 总型号数：12 → 15（Claude 家族从 2 个扩展到 5 个）

## [0.3.0] - 2026-07-03

### v2.3 接口层扩展 + 差异化定位。新增 MCP server + 明确市场定位文档。

### 新增

#### v2.3 - MCP Server（Model Context Protocol 接口）
- 新增 `hippocampus-mcp` crate（rmcp 1.8 + tokio，stdio 传输）
- 5 个 MCP tools（供 Claude Code / Cursor / Trae / Codex CLI 调用）：
  - `archive`：归档对话轮次为记忆文件，返回摘要（含 hook_id）
  - `retrieve`：按 hook_id 检索完整记忆文件
  - `summaries`：获取所有周期摘要列表
  - `prompt`：渲染 system prompt 文本
  - `compaction`：触发周期任务（period: "weekly"/"monthly"）
- 每个 tool 内部创建 LocalStorage，无状态设计
- 错误映射：Core Error → `McpError::invalid_params` / `McpError::internal_error`
- stdio 传输入口（main.rs），通过环境变量 `HIPPOCAMPUS_ROOT` 配置存储根目录
- 11 个 MCP 集成测试（archive/retrieve/summaries/prompt/compaction 全链路 + 会话隔离 + 完整工作流 + 错误处理）
- CI 新增 `mcp-integration-test` job

#### 差异化定位文档（动作 3）
- 新增 `docs/POSITIONING.md`：竞品对比矩阵 + 蓝海象限图 + 四大护城河分析
- 覆盖 12 个主流竞品全景对标（agentmemory/Zep/Letta/Mem0 等）
- 三个直接竞品深度对比（agentmemory ~23k stars / Zep-Graphiti / Letta-MemGPT）
- 明确放弃方向：不做 RAG 向量库、不做角色记忆、不做知识图谱
- README.md 首屏新增定位章节，突出"强时序+极简部署"蓝海象限

### 变更
- workspace `rust-version` 从 1.83 升至 1.85（rmcp 1.7+ 要求 edition 2024）
- workspace 新增依赖：`rmcp = { version = "1.7", features = ["schemars", "transport-io"] }`
- workspace members 新增 `crates/hippocampus-mcp`

### 测试
- 总计 120 测试全部通过（51 单元 + 6 集成 + 17 FFI + 14 HTTP + 1 server 单元 + 11 MCP + 20 Python）
- clippy 0 警告

## [0.2.0] - 2026-07-03

### v2 接口层扩展。在 MVP 基础上新增 HTTP REST API 服务 + Python 原生绑定。

### 新增

#### v2.1 - HTTP/Axum REST API 服务（commit 7f333b0）
- 新增 `hippocampus-server` crate（Axum 0.8 + tower-http 0.7）
- 5 个 REST 端点（路径前缀 `/api/v1/sessions/{sid}/...`）：
  - `POST /archive`：归档一批轮次
  - `GET /memories/{hook_id}`：按钩子 ID 检索
  - `GET /summaries`：获取所有周期摘要
  - `GET /prompt`：渲染 system prompt 文本
  - `POST /compaction`：触发周期任务（period: "weekly"/"monthly"）
- 无状态设计：每次请求创建 LocalStorage，天然支持水平扩展
- 统一错误响应：`{error:{code,message}}`，code 为 `BAD_REQUEST`/`NOT_FOUND`/`INTERNAL_ERROR`
- 环境变量配置：`HIPPOCAMPUS_HOST`（默认 127.0.0.1）/ `HIPPOCAMPUS_PORT`（默认 8765）/ `HIPPOCAMPUS_ROOT`（默认 ./data）
- 14 个 HTTP 集成测试（reqwest 客户端 + 随机端口 TestServer）
- CI 新增 `http-integration-test` job

#### v2.2 - Python 原生绑定（commit a3ed611）
- 新增 `hippocampus-python` crate（PyO3 0.29 + maturin build backend，cdylib）
- `Hippocampus` pyclass：OOP 风格 + `__enter__`/`__exit__` 上下文管理器 + `__repr__`
- 5 个方法（与 FFI/HTTP 一一对应）：`archive` / `retrieve` / `summaries` / `prompt` / `compaction`
- 数据类型映射：dict 字典（JSON 中间转换，零样板代码，无需额外 PyClass）
- 错误映射：Core Error → `PyValueError`
- 模块级函数：`version()` / `operations()`
- 内部 tokio Runtime（`current_thread`，block_on 执行 Core 异步方法）
- 20 个 pytest 集成测试（模块级/生命周期/archive/summaries/retrieve/prompt/compaction/隔离性/完整工作流）
- CI 新增 `python-integration-test` job（maturin build + pip install + pytest）

### 变更
- workspace `rust-version` 从 1.75 升至 1.83（PyO3 0.29 要求）
- workspace 新增依赖：`pyo3 = { version = "0.29", features = ["extension-module"] }`
- `.gitignore` 新增 Python 相关忽略项（`.venv/` / `__pycache__/` / `*.pyc` 等）

### 测试
- 总计 109 测试全部通过（51 单元 + 6 集成 + 17 FFI + 14 HTTP + 1 server 单元 + 20 Python）
- clippy 0 警告

## [0.1.0] - 2026-07-02

### MVP 首个可用版本。核心库 + C ABI 动态库完整实现。

### 新增

#### P0 - 项目骨架
- Cargo workspace 双 crate 架构（`hippocampus-core` + `hippocampus-ffi`）
- GitHub Actions CI 矩阵（Windows/Linux/macOS × x86_64/aarch64）

#### P1 - 核心数据模型 + 存储层
- `MemoryFile` / `IndexHook` / `IndexDocument` / `MessageTurn` / `Tag` 等核心数据结构
- 17 类细粒度标签（`Tag` enum，含 `Other(String)` 兜底扩展）
- `Storage` trait + `LocalStorage` 默认实现（RwLock + 原子写入 temp+rename）
- 文件命名：daily=日期_时间戳（毫秒级）/ weekly=YYYY-Www / monthly=YYYY-MM
- 14 个单元测试通过

#### P2 - 核心逻辑（归档/索引/检索）
- `Archiver`：归档触发检测 + 全封装 `archive()`（写入 Storage + 追加 daily 索引）
- `Retriever`：3 个核心方法（`get_summaries` / `render_to_system_prompt` / `retrieve_memory`）
- 混合检索机制：摘要钩子注入 system prompt + tool 主动检索
- `Tag` Display 中文输出（17 类标签）
- 跨模块集成测试 `full_flow.rs`（6 个场景）
- 26 单元测试 + 6 集成测试通过

#### P3 - 周期任务
- `DefaultScorer`：3 维启发式评分（时效性半衰期 7 天 + 访问频率 10 次满分 + importance）
- `Compactor::weekly_merge()`：周级无损去重合并（寒暄剥离 3 条规则）
- `Compactor::monthly_evict()`：月级评分淘汰（高价值 Turn 保留）
- 移除 `IndexManager`（职责已被 Storage/Retriever/Compactor 完全覆盖）
- 51 单元测试 + 6 集成测试通过

#### P4 - C ABI 动态库
- 5 个核心 C ABI 操作：
  - `hippocampus_archive`：归档一批轮次，返回 SummaryView JSON
  - `hippocampus_retrieve`：按钩子 ID 检索完整记忆文件
  - `hippocampus_get_summaries`：获取所有周期摘要视图
  - `hippocampus_render_prompt`：渲染摘要为 system prompt 文本
  - `hippocampus_run_compaction`：触发周期任务（周合并/月淘汰）
- 内部 tokio Runtime（current_thread，block_on 异步 Core 方法）
- C 头文件 `hippocampus.h` 完整定义
- 17 个 FFI 集成测试（句柄生命周期 / null 参数 / 全链路 / 内存安全）
- 74 测试全部通过（51 单元 + 6 集成 + 17 FFI）

#### P5 - 文档与示例
- 完整 README（含快速开始 / C 调用示例 / Python ctypes 示例）
- ARCHITECTURE.md 架构文档（数据流 / 模块职责）
- examples/ 示例项目（C + Python ctypes）
- 跨语言集成测试（C 程序实际加载动态库验证）
- 性能基准测试（criterion）

### 关键设计决策
- **完整上下文归档**（非摘要）：避免信息损失
- **单写多读 + RwLock**：读无锁，写串行化
- **可插拔 trait**：Storage / Scorer / Migrator 均可替换
- **JSON 序列化**：MVP 可调试优先，v2 支持 MessagePack
- **单线程 FFI 模型**：handle 不保证线程安全，调用方串行化
