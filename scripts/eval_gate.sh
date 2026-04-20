#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

BEFORE="${1:-notes/agent-eval/baselines/TRACE-EVAL-BASELINE-2026-04-18.md}"
AFTER="${2:-notes/agent-eval/reports/TRACE-EVAL-REPORT.md}"
STRICT="${GATE_STRICT:-0}"

echo "run trace_eval gate:"
echo "  before: $BEFORE"
echo "  after:  $AFTER"
echo "  strict: $STRICT"

ARGS=(
  --compare-before "$BEFORE"
  --compare-after "$AFTER"
  --gate
)

if [[ "$STRICT" == "1" ]]; then
  ARGS+=(--gate-strict)
fi

cargo run --bin trace_eval -- "${ARGS[@]}"
