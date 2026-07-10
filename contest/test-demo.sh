#!/bin/bash
# Demo 功能验证脚本
API_KEY="trae-contest-demo-key-2026"
BASE="http://127.0.0.1:8766"
SID="trae-contest-test-001"

echo "=== 1. 归档测试 ==="
curl -s -X POST "$BASE/api/v1/sessions/$SID/archive" \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"turns":[{"id":"t1","user_message":{"text":"我叫小明，住在上海，今年25岁","attachments":[],"tool_calls":[],"thinking":null},"llm_message":{"text":"你好小明！上海是个好地方，很高兴认识你","attachments":[],"tool_calls":[],"thinking":null},"tags":[{"kind":"Text"}],"timestamp":"2026-07-10T10:00:00Z","token_count":50}],"project_id":null}' | python3 -m json.tool 2>/dev/null | head -20

echo ""
echo "=== 2. 摘要查询 ==="
curl -s "$BASE/api/v1/sessions/$SID/summaries" \
  -H "Authorization: Bearer $API_KEY" | python3 -m json.tool 2>/dev/null | head -20

echo ""
echo "=== 3. Prompt 召回 ==="
curl -s "$BASE/api/v1/sessions/$SID/prompt" \
  -H "Authorization: Bearer $API_KEY" | python3 -m json.tool 2>/dev/null | head -20

echo ""
echo "=== 4. 外部访问验证（8088 端口）==="
curl -s -o /dev/null -w "REST API: HTTP %{http_code}\n" http://162.211.183.236:8088/api/v1/sessions/$SID/summaries -H "Authorization: Bearer $API_KEY"
curl -s -o /dev/null -w "MCP 端点: HTTP %{http_code}\n" http://162.211.183.236:8088/mcp
