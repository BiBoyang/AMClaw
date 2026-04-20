.PHONY: trace-compare eval-gate eval-gate-strict

BEFORE ?= notes/agent-eval/baselines/TRACE-EVAL-BASELINE-2026-04-18.md
AFTER ?= notes/agent-eval/reports/TRACE-EVAL-REPORT.md
COMPARE_OUT ?= notes/agent-eval/reports/TRACE-EVAL-COMPARE.md

trace-compare:
	./scripts/trace_compare.sh "$(BEFORE)" "$(COMPARE_OUT)"

eval-gate:
	./scripts/eval_gate.sh "$(BEFORE)" "$(AFTER)"

eval-gate-strict:
	GATE_STRICT=1 ./scripts/eval_gate.sh "$(BEFORE)" "$(AFTER)"
