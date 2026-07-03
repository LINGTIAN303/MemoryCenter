package io.github.lingtian303.hippocampus;

import com.sun.jna.Pointer;

import java.io.Closeable;
import java.util.Arrays;
import java.util.List;

/**
 * Hippocampus 记忆库的 Java 绑定（v2.15-c 新增）。
 *
 * <p>通过 JNA 包装 hippocampus-ffi 的 C ABI，将 Rust 核心能力暴露为 Java API。
 *
 * <h3>架构</h3>
 * <ul>
 *   <li>Java ↔ JNA ↔ C ABI ↔ Rust（hippocampus-ffi crate）</li>
 *   <li>JSON 中间转换：Java String ↔ JSON ↔ Rust structs（与 Python/Node/Go 绑定一致）</li>
 *   <li>同步 API：C ABI 内部通过 tokio block_on 执行异步方法</li>
 * </ul>
 *
 * <h3>依赖</h3>
 * <p>需先构建 hippocampus 动态库：
 * <pre>{@code
 * cargo build --release -p hippocampus-ffi
 * }</pre>
 * <p>然后将动态库（hippocampus.dll / libhippocampus.so / libhippocampus.dylib）
 * 放到 Java 可访问的路径：
 * <ul>
 *   <li>设置 {@code jna.library.path} 系统属性指向库所在目录，或</li>
 *   <li>将库目录加入 {@code java.library.path}，或</li>
 *   <li>将库放到 PATH（Windows）/ LD_LIBRARY_PATH（Linux）/ DYLD_LIBRARY_PATH（macOS）</li>
 * </ul>
 *
 * <h3>示例</h3>
 * <pre>{@code
 * try (Hippocampus hp = Hippocampus.create("./data", "session-1", null)) {
 *     String summaryJson = hp.archive(turnsJson);
 *     System.out.println(summaryJson);
 * } // 自动 close() 释放资源
 * }</pre>
 *
 * <h3>线程安全</h3>
 * <p>C ABI 不保证句柄线程安全，多线程并发访问同一实例需自行加锁。
 */
public class Hippocampus implements Closeable {

    /** 绑定版本号（与 Rust crate 版本同步）。 */
    public static final String VERSION = "0.1.0";

    /** 周级合并参数（对应 C 宏 HIPPOCAMPUS_COMPACTION_WEEKLY）。 */
    public static final int COMPACTION_WEEKLY = HippocampusLibrary.HIPPOCAMPUS_COMPACTION_WEEKLY;

    /** 月级评分淘汰参数（对应 C 宏 HIPPOCAMPUS_COMPACTION_MONTHLY）。 */
    public static final int COMPACTION_MONTHLY = HippocampusLibrary.HIPPOCAMPUS_COMPACTION_MONTHLY;

    /** 支持的操作列表（与 Python/Node/Go 绑定一致）。 */
    public static final List<String> OPERATIONS = Arrays.asList(
            "archive", "retrieve", "summaries", "prompt", "compaction"
    );

    private static final HippocampusLibrary LIB = HippocampusLibrary.INSTANCE;

    private Pointer handle;

    private Hippocampus(Pointer handle) {
        this.handle = handle;
    }

    /**
     * 创建新的 Hippocampus 实例。
     *
     * @param rootPath   存储根目录路径（自动创建）
     * @param sessionId  会话 ID（一个实例绑定一个会话）
     * @param projectId  项目 ID，传 null 表示无项目隔离
     * @return Hippocampus 实例
     * @throws IllegalArgumentException 参数无效（空字符串）
     * @throws RuntimeException         runtime 创建失败（FFI 返回 null）
     */
    public static Hippocampus create(String rootPath, String sessionId, String projectId) {
        if (rootPath == null || rootPath.isEmpty()) {
            throw new IllegalArgumentException("rootPath 不能为空");
        }
        if (sessionId == null || sessionId.isEmpty()) {
            throw new IllegalArgumentException("sessionId 不能为空");
        }

        Pointer h = LIB.hippocampus_new(rootPath, sessionId, projectId);
        if (h == null) {
            throw new RuntimeException("创建 Hippocampus 失败（参数无效或 runtime 创建失败）");
        }
        return new Hippocampus(h);
    }

    /**
     * 释放 Hippocampus 实例资源。
     *
     * <p>调用后不得再使用该实例。多次调用安全（幂等）。
     * 建议用 try-with-resources 自动调用。
     */
    @Override
    public void close() {
        if (handle != null) {
            LIB.hippocampus_free(handle);
            handle = null;
        }
    }

    /**
     * 返回友好的字符串表示。
     */
    @Override
    public String toString() {
        return handle != null ? "Hippocampus(handle=valid)" : "Hippocampus(closed)";
    }

    /**
     * 归档一批轮次为记忆文件，生成索引钩子。
     *
     * @param turnsJson MessageTurn 数组的 JSON 字符串
     * @return SummaryView JSON（含 hook_id/memory_file_id/summary_title/tags 等）
     * @throws IllegalStateException 实例已关闭
     * @throws IllegalArgumentException turnsJson 为空
     * @throws RuntimeException        FFI 失败（错误消息含在异常中）
     */
    public String archive(String turnsJson) {
        ensureOpen();
        if (turnsJson == null || turnsJson.trim().isEmpty()) {
            throw new IllegalArgumentException("turnsJson 不能为空");
        }
        Pointer result = LIB.hippocampus_archive(handle, turnsJson);
        return resultToString(result);
    }

    /**
     * 按钩子 ID 检索完整记忆文件。
     *
     * @param hookId 索引钩子 ID（UUID 字符串）
     * @return MemoryFile JSON（含完整 turns 列表、session_id 等）
     * @throws IllegalStateException 实例已关闭
     * @throws IllegalArgumentException hookId 为空
     * @throws RuntimeException        FFI 失败
     */
    public String retrieve(String hookId) {
        ensureOpen();
        if (hookId == null || hookId.isEmpty()) {
            throw new IllegalArgumentException("hookId 不能为空");
        }
        Pointer result = LIB.hippocampus_retrieve(handle, hookId);
        return resultToString(result);
    }

    /**
     * 获取所有周期（daily/weekly/monthly）的摘要视图列表。
     *
     * @return SummaryView 数组 JSON（按归档时间排序，旧→新）
     * @throws IllegalStateException 实例已关闭
     * @throws RuntimeException        FFI 失败（空存储返回 "[]"，非错误）
     */
    public String summaries() {
        ensureOpen();
        Pointer result = LIB.hippocampus_get_summaries(handle);
        return resultToString(result);
    }

    /**
     * 渲染摘要为 system prompt 文本。
     *
     * @return 渲染好的 prompt 文本（非 JSON，可直接注入 LLM system prompt）
     * @throws IllegalStateException 实例已关闭
     * @throws RuntimeException        FFI 失败（无记忆时返回空字符串，非错误）
     */
    public String prompt() {
        ensureOpen();
        Pointer result = LIB.hippocampus_render_prompt(handle);
        return resultToString(result);
    }

    /**
     * 触发周期任务（周级合并 / 月级评分淘汰）。
     *
     * @param period "weekly"（周级合并）或 "monthly"（月级评分淘汰）
     * @return CompactionResult JSON（memory_file_id/total_turns/total_tokens/hooks_count/period）
     * @throws IllegalStateException    实例已关闭
     * @throws IllegalArgumentException period 值无效
     * @throws RuntimeException         FFI 失败
     */
    public String compaction(String period) {
        ensureOpen();
        int periodCode;
        if ("weekly".equalsIgnoreCase(period)) {
            periodCode = COMPACTION_WEEKLY;
        } else if ("monthly".equalsIgnoreCase(period)) {
            periodCode = COMPACTION_MONTHLY;
        } else {
            throw new IllegalArgumentException(
                    "无效的 period 值: " + period + "（支持: weekly, monthly）");
        }
        Pointer result = LIB.hippocampus_run_compaction(handle, periodCode);
        return resultToString(result);
    }

    /* ====================================================================
     * 内部方法
     * ================================================================== */

    /** 检查实例是否已关闭。 */
    private void ensureOpen() {
        if (handle == null) {
            throw new IllegalStateException("Hippocampus 已关闭");
        }
    }

    /**
     * 将 C ABI 的 HippocampusResult 转为 Java String。
     *
     * <p>内部负责释放 result 和返回的字符串（遵循 C ABI 内存管理约定），
     * 调用方无需手动释放。
     *
     * @param result C ABI 返回的结果指针
     * @return 数据字符串
     * @throws RuntimeException FFI 失败或返回 null
     */
    private static String resultToString(Pointer result) {
        if (result == null) {
            throw new RuntimeException("FFI 返回 null（内存不足或参数无效）");
        }
        try {
            if (!LIB.hippocampus_is_ok(result)) {
                // 失败：读取错误消息
                Pointer cErr = LIB.hippocampus_get_error(result);
                if (cErr != null) {
                    try {
                        throw new RuntimeException(cErr.getString(0, "UTF-8"));
                    } finally {
                        LIB.hippocampus_free_string(cErr);
                    }
                }
                throw new RuntimeException("未知错误（FFI 返回失败但无错误消息）");
            }

            // 成功：读取数据
            Pointer cData = LIB.hippocampus_get_data(result);
            if (cData != null) {
                try {
                    return cData.getString(0, "UTF-8");
                } finally {
                    LIB.hippocampus_free_string(cData);
                }
            }

            // 成功但无数据（理论上不会发生，render_prompt 无记忆时返回空字符串）
            return "";
        } finally {
            LIB.hippocampus_result_free(result);
        }
    }
}
