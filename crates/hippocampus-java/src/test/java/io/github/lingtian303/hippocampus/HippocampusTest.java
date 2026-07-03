package io.github.lingtian303.hippocampus;

import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.nio.file.Path;
import java.util.Arrays;
import java.util.List;

import static org.junit.jupiter.api.Assertions.*;

/**
 * Hippocampus Java 绑定的单元测试（v2.15-c 新增）。
 *
 * <p>共 12 个测试，覆盖版本/操作列表/创建/归档/检索/摘要/Prompt/周期任务/全链路，
 * 与 Go 绑定（{@code crates/hippocampus-go/hippocampus_test.go}）对齐。
 *
 * <p>运行测试前需确保 hippocampus 动态库已构建（{@code cargo build --release -p hippocampus-ffi}），
 * 且库路径已通过 pom.xml 的 surefire 插件配置（{@code jna.library.path}）注入。
 */
@DisplayName("Hippocampus Java 绑定测试")
class HippocampusTest {

    /** 临时目录（JUnit 5 自动创建和清理）。 */
    @TempDir
    Path tempDir;

    /**
     * 构造最小合法的 MessageTurn 数组 JSON（与 Go 测试保持一致）。
     *
     * <p>字段对应 Rust 端 {@code MessageTurn} 结构体：
     * id / user_message / llm_message / tags / timestamp / token_count。
     */
    private static String makeTurnsJson() {
        return "[{"
                + "\"id\":\"00000000-0000-0000-0000-000000000001\","
                + "\"user_message\":{"
                +   "\"text\":\"你好\","
                +   "\"attachments\":[],"
                +   "\"tool_calls\":[],"
                +   "\"thinking\":null"
                + "},"
                + "\"llm_message\":{"
                +   "\"text\":\"你好！有什么可以帮助你的吗？\","
                +   "\"attachments\":[],"
                +   "\"tool_calls\":[],"
                +   "\"thinking\":null"
                + "},"
                + "\"tags\":[{\"kind\":\"Text\"}],"
                + "\"timestamp\":\"2026-07-04T12:00:00Z\","
                + "\"token_count\":50"
                + "}]";
    }

    /** 构造临时存储路径。 */
    private String storagePath() {
        return tempDir.toString();
    }

    /* ====================================================================
     * 1. 常量与基础信息测试
     * ================================================================== */

    @Test
    @DisplayName("版本号应为 0.1.0")
    void testVersion() {
        System.out.println("版本号: " + Hippocampus.VERSION);
        assertEquals("0.1.0", Hippocampus.VERSION);
    }

    @Test
    @DisplayName("支持的操作列表应包含 5 个操作")
    void testOperations() {
        List<String> ops = Hippocampus.OPERATIONS;
        assertEquals(5, ops.size());
        assertTrue(ops.containsAll(Arrays.asList("archive", "retrieve", "summaries", "prompt", "compaction")));
    }

    /* ====================================================================
     * 2. 实例创建与生命周期测试
     * ================================================================== */

    @Test
    @DisplayName("创建实例（无 projectId）")
    void testNewHippocampus() {
        try (Hippocampus hp = Hippocampus.create(storagePath(), "session-1", null)) {
            assertNotNull(hp);
            System.out.println("创建成功: " + hp);
        }
    }

    @Test
    @DisplayName("创建实例（带 projectId）")
    void testNewHippocampusWithProjectId() {
        try (Hippocampus hp = Hippocampus.create(storagePath(), "session-2", "project-x")) {
            assertNotNull(hp);
        }
    }

    @Test
    @DisplayName("toString 应返回有效格式")
    void testHippocampusToString() {
        try (Hippocampus hp = Hippocampus.create(storagePath(), "session-3", null)) {
            assertEquals("Hippocampus(handle=valid)", hp.toString());
        }
    }

    @Test
    @DisplayName("close 后 toString 应显示 closed")
    void testHippocampusClose() {
        Hippocampus hp = Hippocampus.create(storagePath(), "session-4", null);
        hp.close();
        assertEquals("Hippocampus(closed)", hp.toString());
        // 多次 close 应安全（幂等）
        hp.close();
    }

    /* ====================================================================
     * 3. 错误处理测试
     * ================================================================== */

    @Test
    @DisplayName("archive 空 JSON 应抛出 IllegalArgumentException")
    void testArchiveEmptyJson() {
        try (Hippocampus hp = Hippocampus.create(storagePath(), "session-5", null)) {
            assertThrows(IllegalArgumentException.class, () -> hp.archive(""));
            assertThrows(IllegalArgumentException.class, () -> hp.archive("   "));
        }
    }

    @Test
    @DisplayName("archive 无效 JSON 应抛出 RuntimeException")
    void testArchiveInvalidJson() {
        try (Hippocampus hp = Hippocampus.create(storagePath(), "session-6", null)) {
            RuntimeException ex = assertThrows(RuntimeException.class, () -> hp.archive("invalid json"));
            System.out.println("预期错误: " + ex.getMessage());
            assertTrue(ex.getMessage().contains("解析") || ex.getMessage().toLowerCase().contains("parse"));
        }
    }

    @Test
    @DisplayName("retrieve 不存在的 hookId 应抛出 RuntimeException")
    void testRetrieveNonexistent() {
        try (Hippocampus hp = Hippocampus.create(storagePath(), "session-7", null)) {
            RuntimeException ex = assertThrows(RuntimeException.class,
                    () -> hp.retrieve("nonexistent-hook-id"));
            System.out.println("预期错误: " + ex.getMessage());
            assertTrue(ex.getMessage().contains("钩子") || ex.getMessage().toLowerCase().contains("hook"));
        }
    }

    @Test
    @DisplayName("summaries 空存储应返回 []")
    void testSummariesEmpty() {
        try (Hippocampus hp = Hippocampus.create(storagePath(), "session-8", null)) {
            String result = hp.summaries();
            assertEquals("[]", result);
        }
    }

    @Test
    @DisplayName("compaction 无效 period 应抛出 IllegalArgumentException")
    void testCompactionInvalidPeriod() {
        try (Hippocampus hp = Hippocampus.create(storagePath(), "session-9", null)) {
            IllegalArgumentException ex = assertThrows(IllegalArgumentException.class,
                    () -> hp.compaction("daily"));
            System.out.println("预期错误: " + ex.getMessage());
            assertTrue(ex.getMessage().contains("period"));
        }
    }

    /* ====================================================================
     * 4. 端到端工作流测试
     * ================================================================== */

    @Test
    @DisplayName("完整工作流：归档 → 摘要 → Prompt → 检索")
    void testArchiveFullWorkflow() {
        try (Hippocampus hp = Hippocampus.create(storagePath(), "session-10", null)) {
            // 1. 归档
            String summaryJson = hp.archive(makeTurnsJson());
            assertNotNull(summaryJson);
            assertTrue(summaryJson.contains("hook_id"));
            System.out.println("归档成功，summary 包含 hook_id");

            // 从 summary 中提取 hook_id（简单字符串匹配，避免引入 JSON 库）
            String hookId = extractJsonValue(summaryJson, "hook_id");
            assertNotNull(hookId, "无法从 summary 提取 hook_id");
            System.out.println("hook_id: " + hookId);

            // 2. 摘要列表（应有 1 条）
            String summaries = hp.summaries();
            assertNotNull(summaries);
            assertTrue(summaries.contains(hookId));
            System.out.println("摘要列表包含已归档的 hook_id");

            // 3. 渲染 Prompt
            String prompt = hp.prompt();
            assertNotNull(prompt);
            System.out.println("渲染 prompt 长度: " + prompt.length() + " 字符");

            // 4. 检索完整记忆
            String memoryJson = hp.retrieve(hookId);
            assertNotNull(memoryJson);
            assertTrue(memoryJson.contains("turns") || memoryJson.contains("session"));
            System.out.println("检索成功，memory 包含 turns/session 字段");
        }
    }

    /** 从 JSON 字符串中提取指定字段的字符串值（简单实现，避免引入 JSON 库依赖）。 */
    private static String extractJsonValue(String json, String field) {
        String key = "\"" + field + "\":\"";
        int start = json.indexOf(key);
        if (start < 0) {
            return null;
        }
        start += key.length();
        int end = json.indexOf("\"", start);
        if (end < 0) {
            return null;
        }
        return json.substring(start, end);
    }
}
