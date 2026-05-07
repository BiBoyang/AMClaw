#!/usr/bin/env bash
set -uo pipefail

# trace_soft_gate.sh 回归测试
# 本地运行：bash scripts/tests/test_trace_soft_gate.sh

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT" || exit 1

# 确保测试在一致环境下运行，不受 CI 环境变量影响
unset GITHUB_STEP_SUMMARY

FAILED=0
PASSED=0

assert_contains() {
    local haystack="$1"
    local needle="$2"
    local msg="$3"
    if echo "$haystack" | grep -qF "$needle"; then
        PASSED=$((PASSED + 1))
    else
        FAILED=$((FAILED + 1))
        echo "  ❌ FAIL: $msg"
        echo "     expected to contain: '$needle'"
        echo "     got (first 200 chars): '$(echo "$haystack" | head -c 200)'"
    fi
}

TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

# ── 测试 1：valid JSON + PASS ──
echo "▶ test valid JSON with PASS verdict"
VALID_JSON="$TMPDIR/valid.json"
cat > "$VALID_JSON" <<'EOF'
{"overall":"PASS","reasons":["全部核心指标 PASS"],"state_updated":{"before_count":0,"after_count":0,"before_rate":null,"after_rate":null,"delta":null}}
EOF
OUTPUT=$(./scripts/trace_soft_gate.sh "$VALID_JSON" 2>&1)
assert_contains "$OUTPUT" "overall | PASS" "summary should contain PASS"
assert_contains "$OUTPUT" "before_count | 0" "summary should contain before_count"

# ── 测试 2：missing JSON file ──
echo "▶ test missing JSON file"
OUTPUT=$(./scripts/trace_soft_gate.sh "$TMPDIR/does-not-exist.json" 1 2>&1)
assert_contains "$OUTPUT" "produced no JSON output" "should warn on missing file"
assert_contains "$OUTPUT" "exit=1" "should include GATE_EXIT in warning"

# ── 测试 3：invalid JSON ──
echo "▶ test invalid JSON"
INVALID_JSON="$TMPDIR/invalid.json"
echo "not json at all" > "$INVALID_JSON"
OUTPUT=$(./scripts/trace_soft_gate.sh "$INVALID_JSON" 1 2>&1)
assert_contains "$OUTPUT" "produced invalid JSON" "should warn on invalid JSON"
assert_contains "$OUTPUT" "not json at all" "should show raw snippet"
assert_contains "$OUTPUT" "exit=1" "should include GATE_EXIT in warning"

# ── 测试 4：N/A verdict ──
echo "▶ test N/A verdict"
NA_JSON="$TMPDIR/na.json"
cat > "$NA_JSON" <<'EOF'
{"overall":"N/A","reasons":["sample too small"],"state_updated":{"before_count":0,"after_count":5,"before_rate":null,"after_rate":null,"delta":null}}
EOF
OUTPUT=$(./scripts/trace_soft_gate.sh "$NA_JSON" 2>&1)
assert_contains "$OUTPUT" "verdict is N/A" "should warn on N/A verdict"
assert_contains "$OUTPUT" "sample too small" "should include reasons"

# ── 汇总 ──
echo ""
if [ "$FAILED" -eq 0 ]; then
    echo "✅ All $PASSED tests passed"
    exit 0
else
    echo "❌ $FAILED failed, $PASSED passed"
    exit 1
fi
