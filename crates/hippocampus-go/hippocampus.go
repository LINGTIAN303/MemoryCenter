// Package hippocampus 提供 Hippocampus 记忆库的 Go 绑定（v2.15 新增）。
//
// 通过 cgo 包装 hippocampus-ffi 的 C ABI，将 Rust 核心能力暴露为 Go API。
//
// # 架构
//
//   - Go ↔ cgo ↔ C ABI ↔ Rust（hippocampus-ffi crate）
//   - JSON 中间转换：Go string ↔ JSON ↔ Rust structs（与 Python/Node 绑定一致）
//   - 同步 API：C ABI 内部通过 tokio block_on 执行异步方法
//
// # 依赖
//
// 需先构建 hippocampus 动态库：
//
//	cargo build --release -p hippocampus-ffi
//
// 然后将动态库（hippocampus.dll / libhippocampus.so / libhippocampus.dylib）
// 放到 Go 可执行文件可访问的路径（PATH / LD_LIBRARY_PATH / DYLD_LIBRARY_PATH），
// 或与可执行文件同目录。可参考 build.bat / Makefile 自动完成该步骤。
//
// # 示例
//
//	hp, err := hippocampus.New("./data", "session-1", "")
//	if err != nil { log.Fatal(err) }
//	defer hp.Close()
//
//	summaryJSON, err := hp.Archive(turnsJSON)
//	if err != nil { log.Fatal(err) }
//	fmt.Println(summaryJSON)
package hippocampus

/*
// cgo 链接配置：
//   - -L${SRCDIR}：在包目录查找库文件（build 脚本负责生成/复制 lib 到此）
//   - -lhippocampus：链接 hippocampus 动态库的 import library
//
// 构建流程参见 build.bat / Makefile：
//
// Windows（mingw-w64 gcc，必须；MSVC 无法编译 Go cgo runtime）：
//   1. cargo build --release -p hippocampus-ffi
//   2. 复制 target/release/hippocampus.dll → 包目录
//   3. gendef hippocampus.dll + dlltool -d hippocampus.def -l libhippocampus.dll.a
//      生成 mingw 兼容的 import library
//   4. mingw ld 对中文路径支持不佳，build.bat 自动复制到 %TEMP% 跑测试
//   5. 运行时需 hippocampus.dll 在 PATH 或 exe 同目录
//
// Linux / macOS：
//   1. cargo build --release -p hippocampus-ffi
//   2. 复制 target/release/libhippocampus.so    → 包目录/（Linux）
//      复制 target/release/libhippocampus.dylib → 包目录/（macOS）
//   3. 运行时需将包目录加入 LD_LIBRARY_PATH / DYLD_LIBRARY_PATH

#cgo LDFLAGS: -L${SRCDIR} -lhippocampus

#include <stdlib.h>
#include <stdbool.h>

// 直接声明 C ABI（规避 cgo ${SRCDIR} 中文路径下 #include 头文件的问题，
// 同时减少对 hippocampus.h 物理路径的依赖，便于跨平台构建）。
//
// 与 crates/hippocampus-ffi/include/hippocampus.h 保持同步。

typedef struct HippocampusHandle HippocampusHandle;
typedef struct HippocampusResult HippocampusResult;

extern HippocampusHandle* hippocampus_new(
    const char* root_path,
    const char* session_id,
    const char* project_id
);
extern void hippocampus_free(HippocampusHandle* handle);
extern bool hippocampus_is_ok(const HippocampusResult* result);
extern char* hippocampus_get_data(const HippocampusResult* result);
extern char* hippocampus_get_error(const HippocampusResult* result);
extern void hippocampus_result_free(HippocampusResult* result);
extern void hippocampus_free_string(char* s);
extern HippocampusResult* hippocampus_archive(
    HippocampusHandle* handle,
    const char* turns_json
);
extern HippocampusResult* hippocampus_retrieve(
    HippocampusHandle* handle,
    const char* hook_id
);
extern HippocampusResult* hippocampus_get_summaries(HippocampusHandle* handle);
extern HippocampusResult* hippocampus_render_prompt(HippocampusHandle* handle);
extern HippocampusResult* hippocampus_run_compaction(
    HippocampusHandle* handle,
    unsigned int period
);

#define HIPPOCAMPUS_COMPACTION_WEEKLY  0
#define HIPPOCAMPUS_COMPACTION_MONTHLY 1
*/
import "C"

import (
	"errors"
	"fmt"
	"strings"
	"unsafe"
)

// Version 返回绑定版本号（与 Rust crate 版本同步）。
const Version = "0.1.0"

// 周期任务参数常量（与 C ABI 宏对应）。
const (
	CompactionWeekly  = 0 // 周级合并
	CompactionMonthly = 1 // 月级评分淘汰
)

// Hippocampus 表示一个 Hippocampus 记忆库实例。
//
// 持有 C ABI 句柄，一个实例对应一个会话（session_id），不可跨会话复用。
// 使用完毕后必须调用 Close 释放资源，建议配合 defer 使用：
//
//	hp, err := hippocampus.New("/path/to/store", "session-1", "")
//	if err != nil { return err }
//	defer hp.Close()
//
// 线程安全：C ABI 不保证句柄线程安全，多 goroutine 并发访问同一实例需自行加锁。
type Hippocampus struct {
	handle unsafe.Pointer // *C.HippocampusHandle
}

// Operations 返回支持的操作列表（与 Python/Node 绑定一致）。
func Operations() []string {
	return []string{"archive", "retrieve", "summaries", "prompt", "compaction"}
}

// New 创建新的 Hippocampus 实例。
//
// 参数：
//   - rootPath: 存储根目录路径（自动创建）
//   - sessionID: 会话 ID（一个实例绑定一个会话）
//   - projectID: 项目 ID（空字符串表示无项目隔离）
//
// 返回：
//   - *Hippocampus: Hippocampus 实例
//   - error: 参数无效或 runtime 创建失败时返回错误
func New(rootPath, sessionID, projectID string) (*Hippocampus, error) {
	if rootPath == "" {
		return nil, errors.New("rootPath 不能为空")
	}
	if sessionID == "" {
		return nil, errors.New("sessionID 不能为空")
	}

	cRoot := C.CString(rootPath)
	defer C.free(unsafe.Pointer(cRoot))
	cSession := C.CString(sessionID)
	defer C.free(unsafe.Pointer(cSession))

	// projectID 为空时传 NULL（与 C ABI 约定一致）
	var cProject *C.char
	if projectID != "" {
		cProject = C.CString(projectID)
		defer C.free(unsafe.Pointer(cProject))
	}

	handle := C.hippocampus_new(cRoot, cSession, cProject)
	if handle == nil {
		return nil, errors.New("创建 Hippocampus 失败（参数无效或 runtime 创建失败）")
	}

	return &Hippocampus{handle: unsafe.Pointer(handle)}, nil
}

// Close 释放 Hippocampus 实例资源。
//
// 调用后不得再使用该实例。多次调用安全（幂等）。
func (h *Hippocampus) Close() {
	if h.handle == nil {
		return
	}
	C.hippocampus_free((*C.HippocampusHandle)(h.handle))
	h.handle = nil
}

// String 返回友好的字符串表示。
func (h *Hippocampus) String() string {
	if h.handle == nil {
		return "Hippocampus(closed)"
	}
	return "Hippocampus(handle=valid)"
}

// Archive 归档一批轮次为记忆文件，生成索引钩子。
//
// 参数：
//   - turnsJSON: MessageTurn 数组的 JSON 字符串
//
// 返回：
//   - string: SummaryView JSON（含 hook_id/memory_file_id/summary_title/tags 等）
//   - error: 失败时返回错误（含 FFI 错误消息）
func (h *Hippocampus) Archive(turnsJSON string) (string, error) {
	if h.handle == nil {
		return "", errors.New("Hippocampus 已关闭")
	}
	if strings.TrimSpace(turnsJSON) == "" {
		return "", errors.New("turnsJSON 不能为空")
	}

	cTurns := C.CString(turnsJSON)
	defer C.free(unsafe.Pointer(cTurns))

	result := C.hippocampus_archive((*C.HippocampusHandle)(h.handle), cTurns)
	return h.resultToString(result)
}

// Retrieve 按钩子 ID 检索完整记忆文件。
//
// 参数：
//   - hookID: 索引钩子 ID（UUID 字符串）
//
// 返回：
//   - string: MemoryFile JSON（含完整 turns 列表、session_id 等）
//   - error: 失败时返回错误
func (h *Hippocampus) Retrieve(hookID string) (string, error) {
	if h.handle == nil {
		return "", errors.New("Hippocampus 已关闭")
	}
	if hookID == "" {
		return "", errors.New("hookID 不能为空")
	}

	cHook := C.CString(hookID)
	defer C.free(unsafe.Pointer(cHook))

	result := C.hippocampus_retrieve((*C.HippocampusHandle)(h.handle), cHook)
	return h.resultToString(result)
}

// Summaries 获取所有周期（daily/weekly/monthly）的摘要视图列表。
//
// 返回：
//   - string: SummaryView 数组 JSON（按归档时间排序，旧→新）
//   - error: 失败时返回错误（空存储返回 "[]"，非错误）
func (h *Hippocampus) Summaries() (string, error) {
	if h.handle == nil {
		return "", errors.New("Hippocampus 已关闭")
	}

	result := C.hippocampus_get_summaries((*C.HippocampusHandle)(h.handle))
	return h.resultToString(result)
}

// Prompt 渲染摘要为 system prompt 文本。
//
// 返回：
//   - string: 渲染好的 prompt 文本（非 JSON，可直接注入 LLM system prompt）
//   - error: 失败时返回错误（无记忆时返回空字符串，非错误）
func (h *Hippocampus) Prompt() (string, error) {
	if h.handle == nil {
		return "", errors.New("Hippocampus 已关闭")
	}

	result := C.hippocampus_render_prompt((*C.HippocampusHandle)(h.handle))
	return h.resultToString(result)
}

// Compaction 触发周期任务（周级合并 / 月级评分淘汰）。
//
// 参数：
//   - period: "weekly"（周级合并）或 "monthly"（月级评分淘汰）
//
// 返回：
//   - string: CompactionResult JSON（memory_file_id/total_turns/total_tokens/hooks_count/period）
//   - error: 失败时返回错误
func (h *Hippocampus) Compaction(period string) (string, error) {
	if h.handle == nil {
		return "", errors.New("Hippocampus 已关闭")
	}

	var periodCode C.uint
	switch strings.ToLower(period) {
	case "weekly":
		periodCode = C.HIPPOCAMPUS_COMPACTION_WEEKLY
	case "monthly":
		periodCode = C.HIPPOCAMPUS_COMPACTION_MONTHLY
	default:
		return "", fmt.Errorf("无效的 period 值: %s（支持: weekly, monthly）", period)
	}

	result := C.hippocampus_run_compaction((*C.HippocampusHandle)(h.handle), periodCode)
	return h.resultToString(result)
}

// resultToString 将 C ABI 的 HippocampusResult 转为 Go (string, error)。
//
// 内部负责释放 result 和返回的字符串（遵循 C ABI 内存管理约定），
// 调用方无需手动释放。
func (h *Hippocampus) resultToString(result *C.HippocampusResult) (string, error) {
	if result == nil {
		return "", errors.New("FFI 返回 NULL（内存不足或参数无效）")
	}
	defer C.hippocampus_result_free(result)

	if !bool(C.hippocampus_is_ok(result)) {
		// 失败：读取错误消息
		cErr := C.hippocampus_get_error(result)
		if cErr != nil {
			defer C.hippocampus_free_string(cErr)
			return "", errors.New(C.GoString(cErr))
		}
		return "", errors.New("未知错误（FFI 返回失败但无错误消息）")
	}

	// 成功：读取数据
	cData := C.hippocampus_get_data(result)
	if cData != nil {
		defer C.hippocampus_free_string(cData)
		return C.GoString(cData), nil
	}

	// 成功但无数据（理论上不会发生，render_prompt 无记忆时返回空字符串）
	return "", nil
}
