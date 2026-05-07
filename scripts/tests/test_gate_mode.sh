#!/usr/bin/env bash
set -euo pipefail

# 验证 workflow 中 GATE_MODE=soft/hard 的行为矩阵。
# 本地直接运行：bash scripts/tests/test_gate_mode.sh

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT" || exit 1

TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

# 构造 4 种 verdict 的 mock JSON
cat > "$TMPDIR/PASS.json" <<'EOF'
{"overall":"PASS","reasons":["全部核心指标 PASS"],"state_updated":{"before_count":0,"after_count":0,"before_rate":null,"after_rate":null,"delta":null}}
EOF

cat > "$TMPDIR/WARN.json" <<'EOF'
{"overall":"WARN","reasons":["success_rate 退化 4.0pp"],"state_updated":{"before_count":2,"after_count":5,"before_rate":10.0,"after_rate":22.5,"delta":12.5}}
EOF

cat > "$TMPDIR/FAIL.json" <<'EOF'
{"overall":"FAIL","reasons":["success_rate 退化 6.0pp"],"state_updated":{"before_count":2,"after_count":5,"before_rate":10.0,"after_rate":22.5,"delta":12.5}}
EOF

cat > "$TMPDIR/NA.json" <<'EOF'
{"overall":"N/A","reasons":["样本不足"],"state_updated":{"before_count":0,"after_count":0,"before_rate":null,"after_rate":null,"delta":null}}
EOF

# 模拟 workflow gate 步骤的核心逻辑
run_gate_step() {
    local mode=$1
    local json=$2
    local gate_exit=$3

    # 非法值校验（与 workflow 一致）
    if [ "$mode" != "soft" ] && [ "$mode" != "hard" ]; then
        echo "::error::Invalid GATE_MODE: $mode (expected: soft or hard)"
        return 2
    fi

    # trace_soft_gate.sh 负责 summary/warning，失败不阻断
    ./scripts/trace_soft_gate.sh "$json" "$gate_exit" >/dev/null 2>&1 || true

    # 条件退出码（与 workflow 一致）
    if [ "$mode" = "hard" ]; then
        return "$gate_exit"
    fi
    return 0
}

# 行为矩阵测试
echo "▶ Gate Mode Behavior Matrix"
for mode in soft hard; do
    for verdict in PASS WARN FAIL N/A; do
        json="$TMPDIR/${verdict}.json"
        gate_exit=0
        [ "$verdict" = "FAIL" ] && gate_exit=1
        [ "$verdict" = "N/A" ] && gate_exit=2
        # WARN 默认（非 strict）exit 0，与策略文档 Hard Gate 表格一致

        set +e
        run_gate_step "$mode" "$json" "$gate_exit"
        actual_exit=$?
        set -e

        expected=0
        [ "$mode" = "hard" ] && expected=$gate_exit

        if [ "$actual_exit" = "$expected" ]; then
            echo "  ✅ $mode × $verdict (GATE_EXIT=$gate_exit) -> exit $actual_exit"
        else
            echo "  ❌ $mode × $verdict (GATE_EXIT=$gate_exit) -> expected $expected, got $actual_exit"
            exit 1
        fi
    done
done

# 非法值测试
echo ""
echo "▶ Invalid GATE_MODE"
set +e
run_gate_step "invalid" "$TMPDIR/PASS.json" 0
actual=$?
set -e
if [ "$actual" = 2 ]; then
    echo "  ✅ invalid mode -> exit 2"
else
    echo "  ❌ invalid mode -> expected 2, got $actual"
    exit 1
fi

echo ""
echo "✅ All gate mode tests passed"
