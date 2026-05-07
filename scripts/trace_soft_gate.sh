#!/usr/bin/env bash
set -euo pipefail

# 解析 trace_eval --gate-json 的输出 JSON，生成 GITHUB_STEP_SUMMARY 和 warning。
# 本地可直接运行：./scripts/trace_soft_gate.sh trace-gate.json
# CI 调用：./scripts/trace_soft_gate.sh trace-gate.json "$GATE_EXIT"

TRACE_JSON_PATH="${1:-trace-gate.json}"
GATE_EXIT="${2:-}"
write_summary() {
    if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
        echo "$1" >> "$GITHUB_STEP_SUMMARY"
    else
        echo "$1"
    fi
}

write_summary "### Trace Eval Gate"
write_summary ""

if [ ! -s "$TRACE_JSON_PATH" ]; then
    if [ -n "$GATE_EXIT" ]; then
        write_summary "⚠️ gate output missing or empty (exit=${GATE_EXIT})"
        echo "::warning::trace eval gate produced no JSON output (exit=${GATE_EXIT}). check baseline/report files."
    else
        write_summary "⚠️ gate output missing or empty"
        echo "::warning::trace eval gate produced no JSON output. check baseline/report files."
    fi
elif ! jq -e . "$TRACE_JSON_PATH" > /dev/null 2>&1; then
    if [ -n "$GATE_EXIT" ]; then
        write_summary "⚠️ gate output is not valid JSON (exit=${GATE_EXIT})"
        echo "::warning::trace eval gate produced invalid JSON (exit=${GATE_EXIT})."
    else
        write_summary "⚠️ gate output is not valid JSON"
        echo "::warning::trace eval gate produced invalid JSON."
    fi
    write_summary '```'
    if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
        head -n 20 "$TRACE_JSON_PATH" >> "$GITHUB_STEP_SUMMARY" || true
    else
        head -n 20 "$TRACE_JSON_PATH" || true
    fi
    write_summary '```'
else
    OVERALL=$(jq -r '.overall // "N/A"' "$TRACE_JSON_PATH")
    REASONS=$(jq -r '(.reasons // []) | join(", ")' "$TRACE_JSON_PATH")
    BC=$(jq -r '.state_updated.before_count // "N/A"' "$TRACE_JSON_PATH")
    AC=$(jq -r '.state_updated.after_count // "N/A"' "$TRACE_JSON_PATH")
    BR=$(jq -r '.state_updated.before_rate // "null"' "$TRACE_JSON_PATH")
    AR=$(jq -r '.state_updated.after_rate // "null"' "$TRACE_JSON_PATH")
    D=$(jq -r '.state_updated.delta // "null"' "$TRACE_JSON_PATH")

    write_summary "| Field | Value |"
    write_summary "|---|---|"
    write_summary "| overall | ${OVERALL} |"
    write_summary "| before_count | ${BC} |"
    write_summary "| after_count | ${AC} |"
    write_summary "| before_rate | ${BR} |"
    write_summary "| after_rate | ${AR} |"
    write_summary "| delta | ${D} |"
    write_summary "| reasons | ${REASONS} |"

    if [ "$OVERALL" = "FAIL" ]; then
        echo "::warning::trace eval gate verdict is FAIL (soft gate, not blocking merge). reasons: ${REASONS}"
    elif [ "$OVERALL" = "WARN" ]; then
        echo "::warning::trace eval gate verdict is WARN. reasons: ${REASONS}"
    elif [ "$OVERALL" = "N/A" ]; then
        echo "::warning::trace eval gate verdict is N/A (not determinable). reasons: ${REASONS}"
    fi
fi
