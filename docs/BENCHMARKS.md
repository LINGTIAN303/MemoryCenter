# 性能基准测试

Hippocampus 使用 [criterion](https://github.com/bheisrow/criterion.rs) 进行性能基准测试，覆盖归档、检索、周期任务三大核心操作。

## 运行

```bash
# 运行所有基准测试（约 2-5 分钟，取决于硬件）
cargo bench -p hippocampus-core

# 只运行归档基准
cargo bench -p hippocampus-core -- archive

# 只运行检索基准
cargo bench -p hippocampus-core -- retrieve

# 只运行周期任务基准
cargo bench -p hippocampus-core -- compaction
```

## 报告查看

运行完成后，HTML 报告生成在：

```
target/criterion/report/index.html
```

用浏览器打开即可查看：
- 各测试用例的平均耗时 + 方差
- 与上次运行的回归对比（criterion 自动保存基线）
- 性能分布直方图

## 测试场景

### 1. archive（归档）

测量不同 turn 数量下 `Archiver::archive()` 的耗时（含 Storage 写入 + 索引追加）。

| 用例 | 说明 |
|------|------|
| `archive/archive_10_turns` | 10 个 turn，每个 100 tokens |
| `archive/archive_50_turns` | 50 个 turn |
| `archive/archive_100_turns` | 100 个 turn |
| `archive/archive_500_turns` | 500 个 turn（压力测试） |

### 2. retrieve（检索）

预置 50 个记忆文件（每个含 10 个 turn），测量：

| 用例 | 说明 |
|------|------|
| `retrieve/get_summaries_50_files` | 读取所有周期索引并转为摘要视图 |
| `retrieve/render_prompt_50_files` | 渲染所有钩子为 system prompt 文本 |
| `retrieve/retrieve_memory_single` | 按单个 hook_id 检索完整记忆文件 |

### 3. compaction（周期任务）

| 用例 | 说明 |
|------|------|
| `compaction/weekly_merge_7_files` | 7 个 daily 文件合并为 1 个 weekly（含寒暄剥离） |
| `compaction/monthly_evict_4_weekly` | 4 个 weekly 文件评分淘汰（含 28 个 daily 预置） |

## 解读基准结果

### 典型预期

- **archive**：与 turn 数量近似线性关系（IO + 序列化是主开销）
- **get_summaries**：仅读取索引文件，不读记忆文件，应较快
- **render_prompt**：与 get_summaries 同量级，附加字符串拼接开销
- **retrieve_memory**：单次文件读取 + 反序列化
- **weekly_merge**：7 个文件读取 + 寒暄剥离 + 合并写入
- **monthly_evict**：最重，需先合并 4 组 weekly 再评分淘汰

### 回归检测

criterion 会自动保存上次运行结果。若本次运行比基线慢 5% 以上，终端会标注 `regression`。CI 中可加 `--save-baseline` 持久化基线：

```bash
# 首次保存基线
cargo bench -- --save-baseline main

# 后续对比
cargo bench -- --baseline main
```

## 注意事项

- 基准测试会创建临时目录，运行结束后自动清理
- 首次运行会编译 criterion，耗时较长（约 1 分钟）
- 测试结果受磁盘速度影响大（SSD vs HDD 差异显著）
- 如需对比 v2 性能优化效果，建议在同一硬件上运行并保存基线
