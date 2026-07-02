/*
 * Hippocampus - C ABI Header
 *
 * Agent 记忆库依赖库的 C 接口定义
 *
 * 用法：
 *   1. 链接 hippocampus 动态库（hippocampus.dll / libhippocampus.so / libhippocampus.dylib）
 *   2. #include "hippocampus.h"
 *
 * 线程安全：
 *   - HippocampusHandle 不保证线程安全
 *   - 多线程访问同一 handle 需由调用方自行加锁
 *   - 建议每线程独立创建 handle
 *
 * 内存管理约定：
 *   - hippocampus_get_data / hippocampus_get_error 返回的字符串需用 hippocampus_free_string 释放
 *   - hippocampus_new 返回的句柄需用 hippocampus_free 释放
 *   - 所有 HippocampusResult* 需用 hippocampus_result_free 释放
 */

#ifndef HIPPOCAMPUS_H
#define HIPPOCAMPUS_H

#ifdef __cplusplus
extern "C" {
#endif

#include <stddef.h>
#include <stdbool.h>

/* 不透明句柄 */
typedef struct HippocampusHandle HippocampusHandle;
typedef struct HippocampusResult HippocampusResult;

/* 周期任务参数（用于 hippocampus_run_compaction） */
#define HIPPOCAMPUS_COMPACTION_WEEKLY  0
#define HIPPOCAMPUS_COMPACTION_MONTHLY 1

/* ============================================================================
 * 句柄生命周期
 * ========================================================================== */

/* 创建 Hippocampus 实例
 *
 * 一个句柄绑定一个会话（session_id），不可跨会话复用。
 *
 * @param root_path   存储根目录路径（UTF-8 编码，null 结尾）
 * @param session_id  会话 ID（UTF-8 编码，null 结尾）
 * @param project_id   项目 ID（UTF-8 编码，null 结尾），可为 NULL 表示无项目隔离
 * @return 句柄指针，失败返回 NULL（参数无效或 runtime 创建失败）
 */
HippocampusHandle* hippocampus_new(
    const char* root_path,
    const char* session_id,
    const char* project_id
);

/* 释放 Hippocampus 实例
 *
 * @param handle 句柄指针，可为 NULL
 */
void hippocampus_free(HippocampusHandle* handle);

/* ============================================================================
 * 结果处理
 * ========================================================================== */

/* 检查结果是否成功
 *
 * @param result 结果指针，可为 NULL（NULL 视为失败）
 * @return true 成功，false 失败或 NULL
 */
bool hippocampus_is_ok(const HippocampusResult* result);

/* 获取结果中的数据字符串（调用方需用 hippocampus_free_string 释放）
 *
 * 返回的字符串内容因操作而异：
 * - archive:        返回 SummaryView JSON（钩子摘要，含 hook_id）
 * - retrieve:      返回 MemoryFile JSON（完整记忆文件，含所有 turns）
 * - get_summaries: 返回 SummaryView 数组 JSON
 * - render_prompt:  返回渲染好的 prompt 文本（非 JSON，可直接注入 system prompt）
 * - run_compaction: 返回 CompactionResult JSON（合并后的记忆文件概况）
 *
 * @param result 结果指针，可为 NULL
 * @return 数据字符串指针，失败返回 NULL（调用方需释放）
 */
char* hippocampus_get_data(const HippocampusResult* result);

/* 获取结果中的错误消息（调用方需用 hippocampus_free_string 释放）
 *
 * @param result 结果指针，可为 NULL
 * @return 错误消息字符串指针，无错误返回 NULL（调用方需释放）
 */
char* hippocampus_get_error(const HippocampusResult* result);

/* 释放结果
 *
 * @param result 结果指针，可为 NULL
 */
void hippocampus_result_free(HippocampusResult* result);

/* 释放字符串
 *
 * 用于释放 hippocampus_get_data 或 hippocampus_get_error 返回的字符串。
 *
 * @param s 字符串指针，可为 NULL
 */
void hippocampus_free_string(char* s);

/* ============================================================================
 * 核心操作
 * ========================================================================== */

/* 归档上下文
 *
 * 将一批轮次（turns）归档为记忆文件，生成索引钩子。
 *
 * @param handle      实例句柄
 * @param turns_json  MessageTurn 数组的 JSON 字符串（UTF-8 编码，null 结尾）
 * @return 操作结果（成功时 data 为 SummaryView JSON，含 hook_id）
 */
HippocampusResult* hippocampus_archive(
    HippocampusHandle* handle,
    const char* turns_json
);

/* 检索记忆文件（按钩子 ID）
 *
 * @param handle   实例句柄
 * @param hook_id  索引钩子 ID（UUID 字符串）
 * @return 操作结果（成功时 data 为 MemoryFile JSON，含完整 turns）
 */
HippocampusResult* hippocampus_retrieve(
    HippocampusHandle* handle,
    const char* hook_id
);

/* 获取所有周期的摘要视图
 *
 * 实时读取 daily/weekly/monthly 三个周期的索引文档，合并所有钩子转为摘要视图。
 *
 * @param handle 实例句柄
 * @return 操作结果（成功时 data 为 SummaryView 数组 JSON，按时间排序旧→新）
 */
HippocampusResult* hippocampus_get_summaries(HippocampusHandle* handle);

/* 渲染摘要为 system prompt 文本
 *
 * 将所有周期的摘要钩子渲染为可直接注入 LLM system prompt 的文本。
 * 按周期分组（近期记忆/周度记忆/月度记忆），若无记忆返回空字符串。
 *
 * @param handle 实例句柄
 * @return 操作结果（成功时 data 为渲染好的 prompt 文本，非 JSON）
 */
HippocampusResult* hippocampus_render_prompt(HippocampusHandle* handle);

/* 触发周期任务（周级合并 / 月级评分淘汰）
 *
 * @param handle  实例句柄
 * @param period  HIPPOCAMPUS_COMPACTION_WEEKLY (0) 或 HIPPOCAMPUS_COMPACTION_MONTHLY (1)
 * @return 操作结果（成功时 data 为 CompactionResult JSON）
 */
HippocampusResult* hippocampus_run_compaction(
    HippocampusHandle* handle,
    unsigned int period
);

#ifdef __cplusplus
}
#endif

#endif /* HIPPOCAMPUS_H */
