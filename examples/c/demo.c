/*
 * Hippocampus C 调用示例
 *
 * 演示如何通过 C ABI 调用 Hippocampus 的 5 个核心操作：
 *   1. hippocampus_new / free          - 句柄生命周期
 *   2. hippocampus_archive              - 归档一批轮次
 *   3. hippocampus_get_summaries        - 获取摘要视图
 *   4. hippocampus_render_prompt        - 渲染 system prompt
 *   5. hippocampus_retrieve             - 按钩子 ID 检索完整记忆
 *
 * 编译（Linux/macOS）：
 *   gcc demo.c -o demo -L ../../target/release -lhippocampus -lpthread -ldl
 *   LD_LIBRARY_PATH=../../target/release ./demo
 *
 * 编译（Windows MSVC）：
 *   cl demo.c /I ../../crates/hippocampus-ffi/include /link ../../target/release/hippocampus.dll.lib
 *
 * 运行前请先执行：cargo build --release -p hippocampus-ffi
 */

#include "hippocampus.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* 构造一个最小的 MessageTurn JSON 数组（2 轮对话） */
static const char* build_turns_json(void) {
    return
        "["
        "  {"
        "    \"id\": \"550e8400-e29b-41d4-a716-446655440001\","
        "    \"user_message\": { \"text\": \"你好，介绍一下记忆库设计\", \"attachments\": [], \"tool_calls\": [], \"thinking\": null },"
        "    \"llm_message\":  { \"text\": \"Hippocampus 采用三级索引周期...\", \"attachments\": [], \"tool_calls\": [], \"thinking\": null },"
        "    \"tags\": [{\"kind\":\"Text\"},{\"kind\":\"CodeBlock\"}],"
        "    \"timestamp\": \"2026-07-02T14:30:00Z\","
        "    \"token_count\": 80"
        "  },"
        "  {"
        "    \"id\": \"550e8400-e29b-41d4-a716-446655440002\","
        "    \"user_message\": { \"text\": \"如何接入 C ABI?\", \"attachments\": [], \"tool_calls\": [], \"thinking\": null },"
        "    \"llm_message\":  { \"text\": \"链接 hippocampus 动态库后 #include 头文件...\", \"attachments\": [], \"tool_calls\": [], \"thinking\": null },"
        "    \"tags\": [{\"kind\":\"Text\"},{\"kind\":\"CodeBlock\"}],"
        "    \"timestamp\": \"2026-07-02T14:31:00Z\","
        "    \"token_count\": 60"
        "  }"
        "]";
}

/* 从 SummaryView JSON 中粗略提取 hook_id（用于演示 retrieve）
 * 真实场景建议用 jsmn / cJSON 等库解析 JSON。 */
static int extract_hook_id(const char* json, char* out, size_t out_size) {
    const char* key = "\"hook_id\":\"";
    const char* p = strstr(json, key);
    if (!p) return -1;
    p += strlen(key);
    size_t i = 0;
    while (*p && *p != '"' && i < out_size - 1) {
        out[i++] = *p++;
    }
    out[i] = '\0';
    return (i > 0) ? 0 : -1;
}

int main(void) {
    /* 1. 创建句柄 */
    HippocampusHandle* h = hippocampus_new(
        "./mem_data",         /* 存储根目录 */
        "demo-session-001",   /* 会话 ID */
        NULL                  /* project_id，NULL 表示无项目隔离 */
    );
    if (!h) {
        fprintf(stderr, "错误：创建句柄失败\n");
        return 1;
    }
    printf("[1] 句柄创建成功\n");

    /* 2. 归档一批轮次 */
    const char* turns_json = build_turns_json();
    HippocampusResult* r = hippocampus_archive(h, turns_json);
    if (!hippocampus_is_ok(r)) {
        char* err = hippocampus_get_error(r);
        fprintf(stderr, "[2] 归档失败：%s\n", err ? err : "(null)");
        hippocampus_free_string(err);
        hippocampus_result_free(r);
        hippocampus_free(h);
        return 1;
    }
    char* data = hippocampus_get_data(r);
    printf("[2] 归档成功，SummaryView:\n%s\n\n", data);

    /* 3. 从 SummaryView 提取 hook_id 用于后续 retrieve */
    char hook_id[128] = {0};
    if (extract_hook_id(data, hook_id, sizeof(hook_id)) != 0) {
        fprintf(stderr, "[3] 提取 hook_id 失败\n");
        hippocampus_free_string(data);
        hippocampus_result_free(r);
        hippocampus_free(h);
        return 1;
    }
    hippocampus_free_string(data);
    hippocampus_result_free(r);
    printf("[3] 提取 hook_id: %s\n\n", hook_id);

    /* 4. 获取所有周期摘要视图 */
    HippocampusResult* sr = hippocampus_get_summaries(h);
    if (hippocampus_is_ok(sr)) {
        char* sums = hippocampus_get_data(sr);
        printf("[4] 所有摘要视图:\n%s\n\n", sums);
        hippocampus_free_string(sums);
    }
    hippocampus_result_free(sr);

    /* 5. 渲染 system prompt（可直接注入 LLM） */
    HippocampusResult* pr = hippocampus_render_prompt(h);
    if (hippocampus_is_ok(pr)) {
        char* prompt = hippocampus_get_data(pr);
        printf("[5] 渲染的 system prompt:\n%s\n\n", prompt);
        hippocampus_free_string(prompt);
    }
    hippocampus_result_free(pr);

    /* 6. 按钩子 ID 检索完整记忆文件（模拟 LLM 通过 tool 调用） */
    HippocampusResult* rr = hippocampus_retrieve(h, hook_id);
    if (hippocampus_is_ok(rr)) {
        char* memory = hippocampus_get_data(rr);
        printf("[6] 检索到的完整记忆文件:\n%s\n", memory);
        hippocampus_free_string(memory);
    } else {
        char* err = hippocampus_get_error(rr);
        fprintf(stderr, "[6] 检索失败：%s\n", err ? err : "(null)");
        hippocampus_free_string(err);
    }
    hippocampus_result_free(rr);

    /* 7. 释放句柄 */
    hippocampus_free(h);
    printf("\n[7] 句柄已释放，演示完成\n");
    return 0;
}
