#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

BEFORE="${1:-notes/agent-eval/baselines/TRACE-EVAL-BASELINE-2026-04-18.md}"
AFTER="notes/agent-eval/reports/TRACE-EVAL-REPORT.md"
OUT="${2:-notes/agent-eval/reports/TRACE-EVAL-COMPARE.md}"

echo "[1/2] generate latest trace report..."
cargo run --bin trace_eval

echo "[2/2] compare baseline vs latest..."
cargo run --bin trace_eval -- \
  --compare-before "$BEFORE" \
  --compare-after "$AFTER" \
  --compare-output "$OUT"

echo "done: $OUT"

# 打印综合判定行，方便肉眼看
if grep -q "综合判定" "$OUT"; then
  echo "summary:"
  grep "综合判定" "$OUT" | tail -n 1
fi
