// Hippocampus Go 绑定测试套件（v2.15）
//
// 12 个测试覆盖：版本/操作列表/构造/带项目构造/String/Close 幂等/
// 空 JSON/无效 JSON/不存在 hook_id/空 summaries/无效 period/端到端工作流。
//
// 与 Node 测试（crates/hippocampus-node/src/lib.rs）对齐，保证跨语言行为一致。
//
// 运行方式（Windows）：build.bat
// 运行方式（Linux/macOS）：make test

package hippocampus

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// ============================================================================
// 测试辅助
// ============================================================================

// tempStorage 创建临时存储目录，返回绝对路径。
//
// t.TempDir() 会在测试结束时自动清理，无需手动 defer。
func tempStorage(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	storage := filepath.Join(dir, "data")
	if err := os.MkdirAll(storage, 0o755); err != nil {
		t.Fatalf("创建临时目录失败: %v", err)
	}
	return storage
}

// makeTurnsJSON 构造最小合法的 MessageTurn 数组 JSON。
//
// 与 Node 测试中的 turn 结构完全一致，保证跨语言测试数据相同。
func makeTurnsJSON() string {
	turns := []map[string]interface{}{{
		"id": "00000000-0000-0000-0000-000000000001",
		"user_message": map[string]interface{}{
			"text":        "你好",
			"attachments": []interface{}{},
			"tool_calls":  []interface{}{},
			"thinking":    nil,
		},
		"llm_message": map[string]interface{}{
			"text":        "你好！有什么可以帮你的？",
			"attachments": []interface{}{},
			"tool_calls":  []interface{}{},
			"thinking":    nil,
		},
		"tags":        []map[string]string{{"kind": "Text"}},
		"timestamp":   "2026-07-04T12:00:00Z",
		"token_count": 50,
	}}
	b, err := json.Marshal(turns)
	if err != nil {
		panic("构造测试 turns JSON 失败: " + err.Error())
	}
	return string(b)
}

// ============================================================================
// 模块级测试
// ============================================================================

// TestVersion 验证版本号非空。
func TestVersion(t *testing.T) {
	if Version == "" {
		t.Fatal("Version 不应为空")
	}
	t.Logf("版本号: %s", Version)
}

// TestOperations 验证支持的操作列表包含 5 个核心操作。
func TestOperations(t *testing.T) {
	ops := Operations()
	if len(ops) != 5 {
		t.Fatalf("应有 5 个操作，实际 %d: %v", len(ops), ops)
	}
	expected := []string{"archive", "retrieve", "summaries", "prompt", "compaction"}
	for _, exp := range expected {
		found := false
		for _, op := range ops {
			if op == exp {
				found = true
				break
			}
		}
		if !found {
			t.Errorf("操作列表缺少 %q，实际: %v", exp, ops)
		}
	}
}

// ============================================================================
// 构造与生命周期测试
// ============================================================================

// TestNewHippocampus 验证创建实例成功。
func TestNewHippocampus(t *testing.T) {
	storage := tempStorage(t)
	hp, err := New(storage, "sess-1", "")
	if err != nil {
		t.Fatalf("创建 Hippocampus 失败: %v", err)
	}
	defer hp.Close()

	if hp.handle == nil {
		t.Error("handle 不应为 nil")
	}
}

// TestNewHippocampusWithProjectID 验证带 project_id 创建实例。
func TestNewHippocampusWithProjectID(t *testing.T) {
	storage := tempStorage(t)
	hp, err := New(storage, "sess-proj", "proj-a")
	if err != nil {
		t.Fatalf("创建失败: %v", err)
	}
	defer hp.Close()
}

// TestHippocampusString 验证 String() 包含 "Hippocampus"。
func TestHippocampusString(t *testing.T) {
	storage := tempStorage(t)
	hp, _ := New(storage, "sess-str", "proj-x")
	defer hp.Close()

	s := hp.String()
	if !strings.Contains(s, "Hippocampus") {
		t.Errorf("String 应包含 Hippocampus，实际: %q", s)
	}
}

// TestHippocampusClose 验证 Close 幂等（多次调用不 panic）。
func TestHippocampusClose(t *testing.T) {
	storage := tempStorage(t)
	hp, _ := New(storage, "sess-close", "")

	// 多次 Close 应安全
	hp.Close()
	hp.Close()

	// Close 后 String 应返回 closed 状态
	s := hp.String()
	if !strings.Contains(s, "closed") {
		t.Errorf("Close 后 String 应标识 closed，实际: %q", s)
	}
}

// ============================================================================
// Archive 错误处理测试
// ============================================================================

// TestArchiveEmptyJSON 验证空 turnsJSON 返回错误。
func TestArchiveEmptyJSON(t *testing.T) {
	storage := tempStorage(t)
	hp, _ := New(storage, "sess-empty", "")
	defer hp.Close()

	// 空字符串应在 Go 层拦截（不调用 FFI）
	_, err := hp.Archive("")
	if err == nil {
		t.Error("空 turnsJSON 应返回错误")
	}

	// 空白字符串也应被拦截
	_, err = hp.Archive("   ")
	if err == nil {
		t.Error("空白 turnsJSON 应返回错误")
	}
}

// TestArchiveInvalidJSON 验证无效 JSON 返回错误（FFI 层反序列化失败）。
func TestArchiveInvalidJSON(t *testing.T) {
	storage := tempStorage(t)
	hp, _ := New(storage, "sess-invalid", "")
	defer hp.Close()

	_, err := hp.Archive("not a json")
	if err == nil {
		t.Error("无效 JSON 应返回错误")
	}
	t.Logf("预期错误: %v", err)
}

// ============================================================================
// Retrieve 错误处理测试
// ============================================================================

// TestRetrieveNonexistent 验证检索不存在的 hook_id 返回错误。
func TestRetrieveNonexistent(t *testing.T) {
	storage := tempStorage(t)
	hp, _ := New(storage, "sess-retrieve", "")
	defer hp.Close()

	_, err := hp.Retrieve("nonexistent-hook-id")
	if err == nil {
		t.Error("检索不存在的 hook_id 应返回错误")
	}
	t.Logf("预期错误: %v", err)
}

// ============================================================================
// Summaries 测试
// ============================================================================

// TestSummariesEmpty 验证空存储返回 "[]"（非错误）。
func TestSummariesEmpty(t *testing.T) {
	storage := tempStorage(t)
	hp, _ := New(storage, "sess-summ", "")
	defer hp.Close()

	got, err := hp.Summaries()
	if err != nil {
		t.Fatalf("Summaries 失败: %v", err)
	}
	if strings.TrimSpace(got) != "[]" {
		t.Errorf("空存储应返回 []，实际: %q", got)
	}
}

// ============================================================================
// Compaction 错误处理测试
// ============================================================================

// TestCompactionInvalidPeriod 验证无效 period 返回错误（Go 层拦截，不调用 FFI）。
func TestCompactionInvalidPeriod(t *testing.T) {
	storage := tempStorage(t)
	hp, _ := New(storage, "sess-comp", "")
	defer hp.Close()

	_, err := hp.Compaction("daily")
	if err == nil {
		t.Error("无效 period 应返回错误")
	}
	t.Logf("预期错误: %v", err)
}

// ============================================================================
// 端到端工作流测试
// ============================================================================

// TestArchiveFullWorkflow 端到端验证 archive → retrieve → summaries → prompt。
//
// 与 Node 测试 test_hippocampus_archive_full_workflow 对齐。
func TestArchiveFullWorkflow(t *testing.T) {
	storage := tempStorage(t)
	hp, err := New(storage, "sess-e2e", "")
	if err != nil {
		t.Fatalf("创建失败: %v", err)
	}
	defer hp.Close()

	// 1. archive
	summaryJSON, err := hp.Archive(makeTurnsJSON())
	if err != nil {
		t.Fatalf("Archive 失败: %v", err)
	}
	var summary map[string]interface{}
	if err := json.Unmarshal([]byte(summaryJSON), &summary); err != nil {
		t.Fatalf("解析 summary JSON 失败: %v\n原始 JSON: %s", err, summaryJSON)
	}
	hookID, ok := summary["hook_id"].(string)
	if !ok || hookID == "" {
		t.Fatalf("summary 缺少 hook_id 或类型错误: %v", summary)
	}
	t.Logf("归档成功，hook_id: %s", hookID)

	// 2. retrieve
	memoryJSON, err := hp.Retrieve(hookID)
	if err != nil {
		t.Fatalf("Retrieve 失败: %v", err)
	}
	var memory map[string]interface{}
	if err := json.Unmarshal([]byte(memoryJSON), &memory); err != nil {
		t.Fatalf("解析 memory JSON 失败: %v", err)
	}
	if memory["session_id"] != "sess-e2e" {
		t.Errorf("session_id 应为 sess-e2e，实际: %v", memory["session_id"])
	}
	turns, ok := memory["turns"].([]interface{})
	if !ok {
		t.Fatalf("memory.turns 应为数组，实际: %T", memory["turns"])
	}
	if len(turns) != 1 {
		t.Errorf("应有 1 个 turn，实际 %d", len(turns))
	}

	// 3. summaries
	summariesJSON, err := hp.Summaries()
	if err != nil {
		t.Fatalf("Summaries 失败: %v", err)
	}
	var summaries []map[string]interface{}
	if err := json.Unmarshal([]byte(summariesJSON), &summaries); err != nil {
		t.Fatalf("解析 summaries JSON 失败: %v", err)
	}
	if len(summaries) != 1 {
		t.Errorf("应有 1 条摘要，实际 %d", len(summaries))
	}

	// 4. prompt
	prompt, err := hp.Prompt()
	if err != nil {
		t.Fatalf("Prompt 失败: %v", err)
	}
	if prompt == "" {
		t.Error("prompt 不应为空（已归档 1 条记忆）")
	}
	t.Logf("渲染 prompt 长度: %d 字符", len(prompt))
}
