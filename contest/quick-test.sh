#!/bin/bash
API_KEY="trae-contest-demo-key-2026"
BASE="http://127.0.0.1:8766"
SID="trae-contest-quick-001"

echo "=== ARCHIVE ==="
curl -s --max-time 30 -X POST "$BASE/api/v1/sessions/$SID/archive" \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d @/tmp/test-archive.json
echo ""

echo "=== SUMMARIES ==="
curl -s --max-time 10 "$BASE/api/v1/sessions/$SID/summaries" \
  -H "Authorization: Bearer $API_KEY"
echo ""

echo "=== PROMPT ==="
curl -s --max-time 10 "$BASE/api/v1/sessions/$SID/prompt" \
  -H "Authorization: Bearer $API_KEY"
echo ""

echo "=== EXTERNAL 8088 ==="
curl -s --max-time 10 -o /dev/null -w "8088 REST: %{http_code}\n" \
  "http://162.211.183.236:8088/api/v1/sessions/$SID/summaries" \
  -H "Authorization: Bearer $API_KEY"
curl -s --max-time 10 -o /dev/null -w "8088 MCP: %{http_code}\n" \
  "http://162.211.183.236:8088/mcp"

echo "=== DONE ==="
