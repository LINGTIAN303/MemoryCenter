package io.github.lingtian303.hippocampus;

import com.sun.jna.Library;
import com.sun.jna.Native;
import com.sun.jna.Pointer;

/**
 * Hippocampus C ABI 的 JNA 接口定义（v2.15-c 新增）。
 *
 * <p>通过 JNA 直接映射 hippocampus-ffi crate 的 C ABI，无需手写 JNI 样板代码。
 * 接口签名与 {@code crates/hippocampus-ffi/include/hippocampus.h} 保持同步。
 *
 * <h3>内存管理约定</h3>
 * <ul>
 *   <li>{@link #hippocampus_get_data} / {@link #hippocampus_get_error} 返回的 Pointer
 *       需用 {@link #hippocampus_free_string} 释放</li>
 *   <li>{@link #hippocampus_new} 返回的句柄需用 {@link #hippocampus_free} 释放</li>
 *   <li>所有 HippocampusResult Pointer 需用 {@link #hippocampus_result_free} 释放</li>
 * </ul>
 *
 * <h3>线程安全</h3>
 * <p>HippocampusHandle 不保证线程安全，多线程访问同一 handle 需由调用方加锁。
 *
 * <p>通常不直接使用本接口，建议通过 {@link Hippocampus} 封装类操作。
 */
public interface HippocampusLibrary extends Library {

    /**
     * JNA 单例实例：首次访问时加载 hippocampus 动态库。
     *
     * <p>通过 {@link #loadInstance()} 加载，强制使用 UTF-8 编码传递字符串到 C 端
     * （JNA 默认使用平台编码，Windows 中文系统为 GBK，会导致含中文的字符串
     * 传到 Rust 时被识别为非 UTF-8）。
     */
    HippocampusLibrary INSTANCE = loadInstance();

    /**
     * 加载 hippocampus 动态库并设置 UTF-8 编码。
     *
     * <p>必须在 {@code Native.load} 之前设置 {@code jna.encoding} 系统属性，
     * 否则 JNA 会使用平台默认编码（Windows 中文系统为 GBK）将 Java String
     * 转为 C {@code char*}，导致含中文的 turns_json 传到 Rust 端时触发
     * "turns_json 包含无效的 UTF-8" 错误。
     *
     * @return HippocampusLibrary 实例
     */
    static HippocampusLibrary loadInstance() {
        System.setProperty("jna.encoding", "UTF-8");
        return Native.load("hippocampus", HippocampusLibrary.class);
    }

    /** 周级合并（对应 C 宏 HIPPOCAMPUS_COMPACTION_WEEKLY）。 */
    int HIPPOCAMPUS_COMPACTION_WEEKLY = 0;

    /** 月级评分淘汰（对应 C 宏 HIPPOCAMPUS_COMPACTION_MONTHLY）。 */
    int HIPPOCAMPUS_COMPACTION_MONTHLY = 1;

    /* ====================================================================
     * 句柄生命周期
     * ================================================================== */

    /**
     * 创建 Hippocampus 实例。
     *
     * @param rootPath   存储根目录路径（UTF-8，自动由 JNA 转换为 C 字符串）
     * @param sessionId  会话 ID
     * @param projectId  项目 ID，传 null 表示无项目隔离
     * @return 句柄指针，失败返回 null
     */
    Pointer hippocampus_new(String rootPath, String sessionId, String projectId);

    /**
     * 释放 Hippocampus 实例。
     *
     * @param handle 句柄指针，可为 null
     */
    void hippocampus_free(Pointer handle);

    /* ====================================================================
     * 结果处理
     * ================================================================== */

    /**
     * 检查结果是否成功。
     *
     * @param result 结果指针，可为 null（null 视为失败）
     * @return true 成功，false 失败或 null
     */
    boolean hippocampus_is_ok(Pointer result);

    /**
     * 获取结果中的数据字符串。
     *
     * <p>返回的是 C 端原始 char* 指针，调用方需用
     * {@link #hippocampus_free_string} 释放，否则内存泄漏。
     * 建议用 {@code pointer.getString(0, "UTF-8")} 复制到 Java String 后立即释放。
     *
     * @param result 结果指针，可为 null
     * @return 数据字符串指针，失败返回 null
     */
    Pointer hippocampus_get_data(Pointer result);

    /**
     * 获取结果中的错误消息。
     *
     * <p>同 {@link #hippocampus_get_data}，返回的指针需手动释放。
     *
     * @param result 结果指针，可为 null
     * @return 错误消息指针，无错误返回 null
     */
    Pointer hippocampus_get_error(Pointer result);

    /**
     * 释放结果。
     *
     * @param result 结果指针，可为 null
     */
    void hippocampus_result_free(Pointer result);

    /**
     * 释放字符串（用于释放 {@link #hippocampus_get_data} /
     * {@link #hippocampus_get_error} 返回的指针）。
     *
     * @param s 字符串指针，可为 null
     */
    void hippocampus_free_string(Pointer s);

    /* ====================================================================
     * 核心操作
     * ================================================================== */

    /**
     * 归档一批轮次为记忆文件，生成索引钩子。
     *
     * @param handle     实例句柄
     * @param turnsJson  MessageTurn 数组的 JSON 字符串
     * @return 操作结果（成功时 {@link #hippocampus_get_data} 返回 SummaryView JSON）
     */
    Pointer hippocampus_archive(Pointer handle, String turnsJson);

    /**
     * 按钩子 ID 检索完整记忆文件。
     *
     * @param handle  实例句柄
     * @param hookId  索引钩子 ID（UUID 字符串）
     * @return 操作结果（成功时返回 MemoryFile JSON，含完整 turns）
     */
    Pointer hippocampus_retrieve(Pointer handle, String hookId);

    /**
     * 获取所有周期（daily/weekly/monthly）的摘要视图列表。
     *
     * @param handle 实例句柄
     * @return 操作结果（成功时返回 SummaryView 数组 JSON，按时间排序旧→新）
     */
    Pointer hippocampus_get_summaries(Pointer handle);

    /**
     * 渲染摘要为 system prompt 文本。
     *
     * @param handle 实例句柄
     * @return 操作结果（成功时返回渲染好的 prompt 文本，非 JSON）
     */
    Pointer hippocampus_render_prompt(Pointer handle);

    /**
     * 触发周期任务（周级合并 / 月级评分淘汰）。
     *
     * @param handle  实例句柄
     * @param period  {@link #HIPPOCAMPUS_COMPACTION_WEEKLY} 或
     *                {@link #HIPPOCAMPUS_COMPACTION_MONTHLY}
     * @return 操作结果（成功时返回 CompactionResult JSON）
     */
    Pointer hippocampus_run_compaction(Pointer handle, int period);
}
