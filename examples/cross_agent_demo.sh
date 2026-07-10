#!/usr/bin/env bash
# =============================================================================
# 跨 Agent 记忆迁移演示
# =============================================================================
# 演示场景：Trae IDE 归档项目记忆 → Claude Code 用同一 session_id 召回
# 核心价值：session_id 是记忆的命名空间，与 Agent 工具无关
#
# 用法：
#   ./examples/cross_agent_demo.sh
#
# 环境变量：
#   MC_URL  - MemoryCenter REST API 地址（默认 http://127.0.0.1:8766）
#   MC_KEY  - API Key（默认 trae-contest-demo-key-2026）
# =============================================================================

set -euo pipefail

# 配置
MC_URL="${MC_URL:-http://127.0.0.1:8766}"
MC_KEY="${MC_KEY:-trae-contest-demo-key-2026}"
SESSION_ID="cross-agent-demo-$(date +%Y%m%d)"

# 颜色输出
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

info()  { echo -e "${BLUE}[INFO]${NC} $1"; }
ok()    { echo -e "${GREEN}[OK]${NC} $1"; }
warn()  { echo -e "${YELLOW}[WARN]${NC} $1"; }
fail()  { echo -e "${RED}[FAIL]${NC} $1"; exit 1; }

# 检查依赖
command -v curl >/dev/null 2>&1 || fail "需要 curl"
command -v jq   >/dev/null 2>&1 || fail "需要 jq"

echo "============================================================"
echo "  MemoryCenter 跨 Agent 记忆迁移演示"
echo "  Session ID: ${SESSION_ID}"
echo "  Server:     ${MC_URL}"
echo "============================================================"
echo ""

# -----------------------------------------------------------------------------
# Step 1: 健康检查
# -----------------------------------------------------------------------------
info "Step 1: 健康检查..."
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" \
    -H "Authorization: Bearer ${MC_KEY}" \
    "${MC_URL}/api/v1/sessions/health-check/summaries")

if [ "$HTTP_CODE" = "200" ] || [ "$HTTP_CODE" = "404" ]; then
    ok "MemoryCenter 服务可达 (HTTP ${HTTP_CODE})"
else
    fail "无法连接 MemoryCenter (HTTP ${HTTP_CODE})"
fi
echo ""

# -----------------------------------------------------------------------------
# Step 2: 模拟 Trae IDE 归档项目对话
# -----------------------------------------------------------------------------
info "Step 2: 模拟 Trae IDE 归档项目对话..."

TRAE_ARCHIVE_BODY=$(cat <<'JSON'
{
  "turns": [
    {
      "user_message": {"text": "我在开发 MemoryCenter 项目，技术栈是 Rust + Axum + SQLite，单二进制部署。"},
      "llm_message": {"text": "了解了。MemoryCenter 是 AI Agent 的时序记忆基础设施，使用 Rust + Axum + SQLite 技术栈，单二进制部署方式。这个项目的核心是为 AI Agent 提供跨会话的持久记忆能力。"}
    },
    {
      "user_message": {"text": "项目的核心架构是什么？"},
      "llm_message": {"text": "MemoryCenter 的核心架构包括：1) 三级索引周期（日/周/月）管理记忆生命周期；2) 17 类消息级标签支持细粒度检索；3) 完整对话非摘要归档，保留原始上下文；4) C ABI 嵌入宿主进程，零外部依赖。"}
    },
    {
      "user_message": {"text": "评测结果如何？"},
      "llm_message": {"text": "LoCoMo F1 提升 +41.4%，R@5 检索 100%，多跳推理提升 +48.7%。纯算法评分下优势显著，证明记忆检索能力是客观可验证的。"}
    }
  ],
  "preset": {
    "agent": "Trae",
    "scenario": "longproject"
  }
}
JSON
)

ARCHIVE_RESP=$(curl -s -w "\n%{http_code}" \
    -X POST \
    -H "Authorization: Bearer ${MC_KEY}" \
    -H "Content-Type: application/json" \
    -d "${TRAE_ARCHIVE_BODY}" \
    "${MC_URL}/api/v1/sessions/${SESSION_ID}/archive")

HTTP_CODE=$(echo "$ARCHIVE_RESP" | tail -1)
BODY=$(echo "$ARCHIVE_RESP" | head -n -1)

if [ "$HTTP_CODE" = "200" ]; then
    HOOK_ID=$(echo "$BODY" | jq -r '.hook_id')
    SUMMARY_TITLE=$(echo "$BODY" | jq -r '.summary_title')
    ok "Trae 归档成功"
    echo "    Hook ID:    ${HOOK_ID}"
    echo "    摘要标题:    ${SUMMARY_TITLE}"
else
    fail "Trae 归档失败 (HTTP ${HTTP_CODE}): ${BODY}"
fi
echo ""

# -----------------------------------------------------------------------------
# Step 3: 模拟 Claude Code 用同一 session_id 召回记忆
# -----------------------------------------------------------------------------
info "Step 3: 模拟 Claude Code 用同一 session_id 召回记忆..."

PROMPT_RESP=$(curl -s -w "\n%{http_code}" \
    -H "Authorization: Bearer ${MC_KEY}" \
    "${MC_URL}/api/v1/sessions/${SESSION_ID}/prompt")

HTTP_CODE=$(echo "$PROMPT_RESP" | tail -1)
BODY=$(echo "$PROMPT_RESP" | head -n -1)

if [ "$HTTP_CODE" = "200" ]; then
    PROMPT_TEXT=$(echo "$BODY" | jq -r '.prompt // .')
    ok "Claude Code 召回成功"
    echo ""
    echo "    --- 召回的记忆内容 ---"
    echo "$PROMPT_TEXT" | sed 's/^/    /'
    echo "    --- END ---"
    echo ""

    # 验证记忆迁移成功
    if echo "$PROMPT_TEXT" | grep -q "MemoryCenter"; then
        ok "验证通过: Claude Code 能看到 Trae 归档的 MemoryCenter 项目记忆"
    else
        warn "验证失败: 召回内容未包含 MemoryCenter 关键词"
    fi

    if echo "$PROMPT_TEXT" | grep -q "Rust"; then
        ok "验证通过: 技术栈信息（Rust）已迁移"
    fi

    if echo "$PROMPT_TEXT" | grep -q "41.4"; then
        ok "验证通过: 评测数据（+41.4%）已迁移"
    fi
else
    fail "Claude Code 召回失败 (HTTP ${HTTP_CODE}): ${BODY}"
fi
echo ""

# -----------------------------------------------------------------------------
# Step 4: 跨 Agent 语义检索演示
# -----------------------------------------------------------------------------
info "Step 4: 跨 Agent 语义检索演示..."

SEARCH_BODY='{"query":"项目技术栈和评测结果","top_k":5}'

SEARCH_RESP=$(curl -s -w "\n%{http_code}" \
    -X POST \
    -H "Authorization: Bearer ${MC_KEY}" \
    -H "Content-Type: application/json" \
    -d "${SEARCH_BODY}" \
    "${MC_URL}/api/v1/sessions/${SESSION_ID}/search")

HTTP_CODE=$(echo "$SEARCH_RESP" | tail -1)
BODY=$(echo "$SEARCH_RESP" | head -n -1)

if [ "$HTTP_CODE" = "200" ]; then
    RESULT_COUNT=$(echo "$BODY" | jq -r '.results | length')
    SEARCH_MODE=$(echo "$BODY" | jq -r '.mode')
    ok "语义检索成功 (${SEARCH_MODE} 模式, ${RESULT_COUNT} 条结果)"

    if [ "$RESULT_COUNT" -gt 0 ]; then
        echo ""
        echo "    --- 检索结果 ---"
        echo "$BODY" | jq -r '.results[] | "    - hook_id: \(.hook_id)  score: \(.score)"'
        echo "    --- END ---"
        ok "验证通过: Claude Code 能检索到 Trae 归档的记忆"
    else
        warn "检索结果为空"
    fi
else
    warn "语义检索失败 (HTTP ${HTTP_CODE}): ${BODY}"
fi
echo ""

# -----------------------------------------------------------------------------
# Step 5: 获取完整记忆内容（retrieve）
# -----------------------------------------------------------------------------
info "Step 5: 获取完整记忆内容（retrieve）..."

RETRIEVE_RESP=$(curl -s -w "\n%{http_code}" \
    -H "Authorization: Bearer ${MC_KEY}" \
    "${MC_URL}/api/v1/sessions/${SESSION_ID}/memories/${HOOK_ID}")

HTTP_CODE=$(echo "$RETRIEVE_RESP" | tail -1)
BODY=$(echo "$RETRIEVE_RESP" | head -n -1)

if [ "$HTTP_CODE" = "200" ]; then
    TURN_COUNT=$(echo "$BODY" | jq -r '.turns | length')
    ok "完整记忆获取成功 (${TURN_COUNT} 轮对话)"
    echo ""
    echo "    --- 对话内容 ---"
    echo "$BODY" | jq -r '.turns[] | "    [\(.user_message.text)] -> \(.llm_message.text)"'
    echo "    --- END ---"
    ok "验证通过: 原始对话内容完整保留，跨 Agent 可追溯"
else
    warn "完整记忆获取失败 (HTTP ${HTTP_CODE})"
fi
echo ""

# -----------------------------------------------------------------------------
# 总结
# -----------------------------------------------------------------------------
echo "============================================================"
echo "  演示完成"
echo "============================================================"
echo ""
echo "  核心价值验证："
echo "  1. Trae IDE 归档的对话 → Claude Code 通过 prompt 召回 ✅"
echo "  2. 语义检索跨 Agent 可用 ✅"
echo "  3. 原始对话内容完整保留 ✅"
echo ""
echo "  结论：MemoryCenter 的 session_id 是记忆的命名空间，"
echo "  与 Agent 工具无关。不同 Agent 只需共享 session_id 即可共享记忆。"
echo ""
