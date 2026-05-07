.PHONY: trace-compare eval-gate eval-gate-strict eval-gate-json trace-soft-gate test-scripts test-gate-mode lint-scripts

BEFORE ?= notes/agent-eval/baselines/TRACE-EVAL-BASELINE-2026-04-18.md
AFTER ?= notes/agent-eval/reports/TRACE-EVAL-REPORT.md
COMPARE_OUT ?= notes/agent-eval/reports/TRACE-EVAL-COMPARE.md

trace-compare:
	./scripts/trace_compare.sh "$(BEFORE)" "$(COMPARE_OUT)"

eval-gate:
	./scripts/eval_gate.sh "$(BEFORE)" "$(AFTER)"

eval-gate-strict:
	GATE_STRICT=1 ./scripts/eval_gate.sh "$(BEFORE)" "$(AFTER)"

eval-gate-json:
	GATE_JSON=1 ./scripts/eval_gate.sh "$(BEFORE)" "$(AFTER)"

trace-soft-gate:
	GATE_JSON=1 ./scripts/eval_gate.sh "$(BEFORE)" "$(AFTER)" > trace-gate.json && ./scripts/trace_soft_gate.sh trace-gate.json

test-scripts:
	bash scripts/tests/test_trace_soft_gate.sh

test-gate-mode:
	bash scripts/tests/test_gate_mode.sh

lint-scripts:
	shellcheck scripts/*.sh scripts/tests/*.sh
