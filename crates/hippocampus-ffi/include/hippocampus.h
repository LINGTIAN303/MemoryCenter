/*
 * Hippocampus - C ABI Header
 *
 * Agent 记忆库依赖库的 C 接口定义
 *
 * 用法：
 *   1. 链接 hippocampus 动态库（hippocampus.dll / libhippocampus.so / libhippocampus.dylib）
 *   2. #include "hippocampus.h"
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

/* 周期层级枚举（与 Rust ArchivePeriod 对应） */
typedef enum {
    HIPPOCAMPUS_PERIOD_DAILY = 0,
    HIPPOCAMPUS_PERIOD_WEEKLY = 1,
    HIPPOCAMPUS_PERIOD_MONTHLY = 2,
} HippocampusPeriod;

/* ============================================================================
 * 句柄生命周期
 * ========================================================================== */

/* 创建 Hippocampus 实例
 * @param root_path 存储根目录路径（UTF-8 编码，null 结尾）
 * @return 句柄指针，失败返回 NULL
 */
HippocampusHandle* hippocampus_new(const char* root_path);

/* 释放 Hippocampus 实例 */
void hippocampus_free(HippocampusHandle* handle);

/* ============================================================================
 * 结果处理
 * ========================================================================== */

/* 检查结果是否成功 */
bool hippocampus_is_ok(const HippocampusResult* result);

/* 获取结果数据（JSON 字符串，调用方需释放）
 * 返回的字符串内容因操作而异：
 * - archive: 返回记忆文件 JSON
 * - retrieve: 返回完整记忆文件 JSON
 * - get_summaries: 返回摘要视图数组 JSON
 */
char* hippocampus_get_data(const HippocampusResult* result);

/* 获取错误消息（调用方需释放） */
char* hippocampus_get_error(const HippocampusResult* result);

/* 释放结果 */
void hippocampus_result_free(HippocampusResult* result);

/* 释放字符串 */
void hippocampus_free_string(char* s);

/* ============================================================================
 * 核心操作
 * ========================================================================== */

/* 归档上下文
 * @param handle 实例句柄
 * @param context_json 上下文 JSON 字符串（用户消息 + LLM 消息）
 * @return 操作结果
 */
HippocampusResult* hippocampus_archive(HippocampusHandle* handle, const char* context_json);

/* 检索记忆文件
 * @param hook_id 索引钩子 ID
 * @return 操作结果（成功时 data 为记忆文件 JSON）
 */
HippocampusResult* hippocampus_retrieve(HippocampusHandle* handle, const char* hook_id);

/* 获取摘要视图（用于注入 system prompt）
 * @return 操作结果（成功时 data 为摘要视图数组 JSON）
 */
HippocampusResult* hippocampus_get_summaries(HippocampusHandle* handle);

/* 触发周期任务
 * @param period 0=周级合并, 1=月级评分淘汰
 * @return 操作结果
 */
HippocampusResult* hippocampus_run_compaction(HippocampusHandle* handle, unsigned int period);

#ifdef __cplusplus
}
#endif

#endif /* HIPPOCAMPUS_H */
