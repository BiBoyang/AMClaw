# Trace Evaluation Report

- generated: 2026-04-18T09:41:03.187594+00:00
- total traces: 20
- baseline_file: notes/agent-eval/baselines/EVAL-BASELINE-SAMPLES-2026-04-18.md
- baseline_run_ids: 20
- interesting traces: 15

## Summary Statistics

| metric | count | ratio |
| --- | ---: | ---: |
| total | 20 | 100% |
| success | 15 | 75.0% |
| with memory injected | 4 | 20.0% |
| with memory dropped | 2 | 10.0% |
| with session state | 3 | 15.0% |
| with context pack dropped | 3 | 15.0% |
| with llm fallback | 7 | 35.0% |
| with failures | 6 | 30.0% |

## Baseline Coverage

| metric | count | ratio |
| --- | ---: | ---: |
| baseline run ids | 20 | 100% |
| baseline hits in current trace set | 20 | 100.0% |
| baseline missing in current trace set | 0 | 0.0% |

## Failure Type Distribution

| failure_type | count | ratio |
| --- | ---: | ---: |
| tool_call_error | 2 | 10.0% |
| fallback_exhausted | 1 | 5.0% |
| llm_auth_error | 1 | 5.0% |
| planning_stall_or_drift | 1 | 5.0% |
| unknown_failure | 1 | 5.0% |

## Tool Use Statistics

| metric | count | ratio |
| --- | ---: | ---: |
| traces with tool calls | 17 | 85.0% |
| total tool calls | 28 | - |
| tool success | 23 | 82.1% |
| tool failure | 5 | 17.9% |

### Tool Error Type TopN

| error_type | count |
| --- | ---: |
| file not found | 1 |
| no match | 1 |
| path outside workspace: /etc/passwd | 1 |
| path outside workspace: /tmp/test.txt | 1 |
| worker panic: index out of bounds | 1 |

### Tool Call Count Distribution

| tool_calls | trace_count |
| --- | ---: |
| 1 | 10 |
| 2 | 4 |
| 3 | 2 |
| 4 | 1 |

## Planning / ReAct Statistics

| metric | value |
| --- | --- |
| step_count min / max / avg | 1 / 12 / 3.2 |
| unfinished_plan (failed + steps > 5) | 1 |
| stall_or_drift hits | 1 |

### Step Count Distribution

| step_range | trace_count | ratio |
| --- | ---: | ---: |
| 1 | 3 | 15.0% |
| 2 | 7 | 35.0% |
| 3-5 | 8 | 40.0% |
| 6-10 | 1 | 5.0% |
| 10+ | 1 | 5.0% |

## Recovery Statistics

| metric | count | ratio |
| --- | ---: | ---: |
| traces_with_recovery | 6 | 30.0% |
| recovery_attempt_count | 6 | - |
| recovery_success | 1 | 16.7% |
| recovery_failure | 5 | 83.3% |

### Recovery by Failure Type

| failure_type | attempt | success | failure |
| --- | ---: | ---: | ---: |
| tool_call_error | 2 | 1 | 1 |
| fallback_exhausted | 1 | 0 | 1 |
| unknown_failure | 1 | 0 | 1 |
| llm_auth_error | 1 | 0 | 1 |
| planning_stall_or_drift | 1 | 0 | 1 |

## Per-Trace Detail

| run_id | success | baseline | steps | mem(r/i/d) | state | ctx_drop | failures | reasons | input |
| --- | --- | --- | ---: | --- | --- | --- | --- | --- | --- |
| `d4444444` | ✗ | ✓ | 3 | 0/0/0 | · | · | 1 | failed, has_failures, llm_fallback | 执行高级分析任务 |
| `d3333333` | ✗ | ✓ | 12 | 0/0/0 | · | · | 1 | failed, has_failures | 帮我完成一个复杂的多步骤任务 |
| `c2222222` | ✓ | ✓ | 4 | 2/0/2 | · | ✓ | 0 | memory_dropped, context_pack_dropped, memory_retrieved_but_none_injected | 根据之前的讨论修改代码 |
| `c3333333` | ✓ | ✓ | 3 | 0/0/0 | · | ✓ | 0 | context_pack_dropped | 继续 |
| `a1111111` | ✓ | ✓ | 3 | 2/2/0 | ✓ | · | 0 |  | 查我上次提到的项目计划 |
| `d5555555` | ✓ | ✓ | 4 | 0/0/0 | · | · | 1 | has_failures | 写入文件 /tmp/test.txt |
| `d6666666` | ✗ | ✓ | 1 | 0/0/0 | · | · | 1 | failed, has_failures | 做一些未知的事情 |
| `a2222222` | ✓ | ✓ | 1 | 0/0/0 | · | · | 0 |  | 你好 |
| `b1111111` | ✓ | ✓ | 2 | 0/0/0 | · | · | 0 | llm_fallback | 创建文件 demo/test.txt :: hello world |
| `b2222222` | ✓ | ✓ | 2 | 0/0/0 | · | · | 0 | llm_fallback | 写一段代码示例 |
| `a3333333` | ✓ | ✓ | 4 | 1/1/0 | · | · | 0 |  | 读取 config.toml 并告诉我数据库路径 |
| `a4444444` | ✓ | ✓ | 3 | 0/0/0 | ✓ | · | 0 |  | 继续上次的任务 |
| `c1111111` | ✓ | ✓ | 6 | 5/2/3 | · | ✓ | 0 | memory_dropped, context_pack_dropped | 总结这些长文档的核心观点：README.md DESIGN-0.1.0.md P... |
| `a5555555` | ✓ | ✓ | 5 | 3/3/0 | ✓ | · | 0 |  | 帮我整理今天的任务列表并写入 tasks.md |
| `d1111111` | ✗ | ✓ | 2 | 0/0/0 | · | · | 1 | failed, has_failures | 读取不存在的文件 /etc/passwd |
| `d2222222` | ✗ | ✓ | 1 | 0/0/0 | · | · | 1 | failed, has_failures | 帮我分析这段代码 |
| `ece3aad2` | ✓ | ✓ | 2 | 0/0/0 | · | · | 0 | llm_fallback | 读文件 README.md |
| `21cced62` | ✓ | ✓ | 2 | 0/0/0 | · | · | 0 | llm_fallback | 读文件 README.md |
| `9aa8cdec` | ✓ | ✓ | 2 | 0/0/0 | · | · | 0 | llm_fallback | 读文件 README.md |
| `9633a8ee` | ✓ | ✓ | 2 | 0/0/0 | · | · | 0 | llm_fallback | 读文件 README.md |

## Interesting Traces Deep Dive

### `d4444444-4444-4444-4444-444444444444`

- **user_input**: 执行高级分析任务
- **success**: false
- **in_baseline**: true
- **error**: 主链路失败且 fallback 无法收敛
- **duration**: 5000ms, **steps**: 3
- **memory**: retrieved=0, injected=0, dropped=0, total_chars=0
- **session_state**: false
- **context_pack**: dropped=false, reasons=[]
- **llm_calls**: total=2, success=0, failed=2
- **tool_calls**: total=0, success=0
- **recovery**: attempts=1, success=0, actions=[], results=[]
- **failure_types**: fallback_exhausted
- **interest_reasons**: failed, has_failures, llm_fallback

### `d3333333-3333-3333-3333-333333333333`

- **user_input**: 帮我完成一个复杂的多步骤任务
- **success**: false
- **in_baseline**: true
- **error**: 规划循环停滞：超过最大 replan 次数(5)
- **duration**: 5000ms, **steps**: 12
- **memory**: retrieved=0, injected=0, dropped=0, total_chars=0
- **session_state**: false
- **context_pack**: dropped=false, reasons=[]
- **llm_calls**: total=5, success=5, failed=0
- **tool_calls**: total=3, success=1
- **recovery**: attempts=1, success=0, actions=[], results=[]
- **failure_types**: planning_stall_or_drift
- **interest_reasons**: failed, has_failures

### `c2222222-2222-2222-2222-222222222222`

- **user_input**: 根据之前的讨论修改代码
- **success**: true
- **in_baseline**: true
- **duration**: 5000ms, **steps**: 4
- **memory**: retrieved=2, injected=0, dropped=2, total_chars=0
- **session_state**: false
- **context_pack**: dropped=true, reasons=["context_budget_exceeded", "dropped_sections: state, history_tail"]
- **llm_calls**: total=1, success=1, failed=0
- **tool_calls**: total=2, success=2
- **recovery**: attempts=0, success=0, actions=[], results=[]
- **interest_reasons**: memory_dropped, context_pack_dropped, memory_retrieved_but_none_injected

### `c3333333-3333-3333-3333-333333333333`

- **user_input**: 继续
- **success**: true
- **in_baseline**: true
- **duration**: 5000ms, **steps**: 3
- **memory**: retrieved=0, injected=0, dropped=0, total_chars=0
- **session_state**: false
- **context_pack**: dropped=true, reasons=["history_truncated", "older_turns_removed: 3"]
- **llm_calls**: total=1, success=1, failed=0
- **tool_calls**: total=1, success=1
- **recovery**: attempts=0, success=0, actions=[], results=[]
- **interest_reasons**: context_pack_dropped

### `d5555555-5555-5555-5555-555555555555`

- **user_input**: 写入文件 /tmp/test.txt
- **success**: true
- **in_baseline**: true
- **duration**: 5000ms, **steps**: 4
- **memory**: retrieved=0, injected=0, dropped=0, total_chars=0
- **session_state**: false
- **context_pack**: dropped=false, reasons=[]
- **llm_calls**: total=2, success=2, failed=0
- **tool_calls**: total=2, success=1
- **recovery**: attempts=1, success=1, actions=[], results=[]
- **failure_types**: tool_call_error
- **interest_reasons**: has_failures

### `d6666666-6666-6666-6666-666666666666`

- **user_input**: 做一些未知的事情
- **success**: false
- **in_baseline**: true
- **error**: 未知错误: worker panic
- **duration**: 5000ms, **steps**: 1
- **memory**: retrieved=0, injected=0, dropped=0, total_chars=0
- **session_state**: false
- **context_pack**: dropped=false, reasons=[]
- **llm_calls**: total=1, success=1, failed=0
- **tool_calls**: total=1, success=0
- **recovery**: attempts=1, success=0, actions=[], results=[]
- **failure_types**: unknown_failure
- **interest_reasons**: failed, has_failures

### `b1111111-1111-1111-1111-111111111111`

- **user_input**: 创建文件 demo/test.txt :: hello world
- **success**: true
- **in_baseline**: true
- **duration**: 5000ms, **steps**: 2
- **memory**: retrieved=0, injected=0, dropped=0, total_chars=0
- **session_state**: false
- **context_pack**: dropped=false, reasons=[]
- **llm_calls**: total=1, success=0, failed=1
- **tool_calls**: total=1, success=1
- **recovery**: attempts=0, success=0, actions=[], results=[]
- **interest_reasons**: llm_fallback

### `b2222222-2222-2222-2222-222222222222`

- **user_input**: 写一段代码示例
- **success**: true
- **in_baseline**: true
- **duration**: 5000ms, **steps**: 2
- **memory**: retrieved=0, injected=0, dropped=0, total_chars=0
- **session_state**: false
- **context_pack**: dropped=false, reasons=[]
- **llm_calls**: total=1, success=0, failed=1
- **tool_calls**: total=1, success=1
- **recovery**: attempts=0, success=0, actions=[], results=[]
- **interest_reasons**: llm_fallback

### `c1111111-1111-1111-1111-111111111111`

- **user_input**: 总结这些长文档的核心观点：README.md DESIGN-0.1.0.md PLAN.md NEXT-STEPS.md
- **success**: true
- **in_baseline**: true
- **duration**: 5000ms, **steps**: 6
- **memory**: retrieved=5, injected=2, dropped=3, total_chars=2400
- **session_state**: false
- **context_pack**: dropped=true, reasons=["memory_budget_exceeded", "section_priority_reorder"]
- **llm_calls**: total=2, success=2, failed=0
- **tool_calls**: total=4, success=4
- **recovery**: attempts=0, success=0, actions=[], results=[]
- **interest_reasons**: memory_dropped, context_pack_dropped

### `d1111111-1111-1111-1111-111111111111`

- **user_input**: 读取不存在的文件 /etc/passwd
- **success**: false
- **in_baseline**: true
- **error**: 工具执行失败: 路径超出工作区边界
- **duration**: 5000ms, **steps**: 2
- **memory**: retrieved=0, injected=0, dropped=0, total_chars=0
- **session_state**: false
- **context_pack**: dropped=false, reasons=[]
- **llm_calls**: total=1, success=1, failed=0
- **tool_calls**: total=1, success=0
- **recovery**: attempts=1, success=0, actions=[], results=[]
- **failure_types**: tool_call_error
- **interest_reasons**: failed, has_failures

### `d2222222-2222-2222-2222-222222222222`

- **user_input**: 帮我分析这段代码
- **success**: false
- **in_baseline**: true
- **error**: LLM 调用失败: 401 Unauthorized
- **duration**: 5000ms, **steps**: 1
- **memory**: retrieved=0, injected=0, dropped=0, total_chars=0
- **session_state**: false
- **context_pack**: dropped=false, reasons=[]
- **llm_calls**: total=1, success=0, failed=1
- **tool_calls**: total=0, success=0
- **recovery**: attempts=1, success=0, actions=[], results=[]
- **failure_types**: llm_auth_error
- **interest_reasons**: failed, has_failures

### `ece3aad2-a621-4619-ab3b-93abba3ea3c2`

- **user_input**: 读文件 README.md
- **success**: true
- **in_baseline**: true
- **duration**: 3ms, **steps**: 2
- **memory**: retrieved=0, injected=0, dropped=0, total_chars=0
- **session_state**: false
- **context_pack**: dropped=false, reasons=[]
- **llm_calls**: total=1, success=0, failed=1
- **tool_calls**: total=1, success=1
- **recovery**: attempts=0, success=0, actions=[], results=[]
- **interest_reasons**: llm_fallback

### `21cced62-914e-4453-b2dc-352d363c67e7`

- **user_input**: 读文件 README.md
- **success**: true
- **in_baseline**: true
- **duration**: 5ms, **steps**: 2
- **memory**: retrieved=0, injected=0, dropped=0, total_chars=0
- **session_state**: false
- **context_pack**: dropped=false, reasons=[]
- **llm_calls**: total=1, success=0, failed=1
- **tool_calls**: total=1, success=1
- **recovery**: attempts=0, success=0, actions=[], results=[]
- **interest_reasons**: llm_fallback

### `9aa8cdec-ed66-4731-94a4-638e57ab2976`

- **user_input**: 读文件 README.md
- **success**: true
- **in_baseline**: true
- **duration**: 4ms, **steps**: 2
- **memory**: retrieved=0, injected=0, dropped=0, total_chars=0
- **session_state**: false
- **context_pack**: dropped=false, reasons=[]
- **llm_calls**: total=1, success=0, failed=1
- **tool_calls**: total=1, success=1
- **recovery**: attempts=0, success=0, actions=[], results=[]
- **interest_reasons**: llm_fallback

### `9633a8ee-6479-45ba-94a0-ac3831af5727`

- **user_input**: 读文件 README.md
- **success**: true
- **in_baseline**: true
- **duration**: 4ms, **steps**: 2
- **memory**: retrieved=0, injected=0, dropped=0, total_chars=0
- **session_state**: false
- **context_pack**: dropped=false, reasons=[]
- **llm_calls**: total=1, success=0, failed=1
- **tool_calls**: total=1, success=1
- **recovery**: attempts=0, success=0, actions=[], results=[]
- **interest_reasons**: llm_fallback

## Failure Taxonomy Annotation Template

对以上 interesting traces 进行人工评审时，可按下表标注：

| run_id | primary_failure | severity | notes |
| --- | --- | --- | --- |
| `d4444444` | (待填) | (low/mid/high) | (待填) |
| `d3333333` | (待填) | (low/mid/high) | (待填) |
| `c2222222` | (待填) | (low/mid/high) | (待填) |
| `c3333333` | (待填) | (low/mid/high) | (待填) |
| `d5555555` | (待填) | (low/mid/high) | (待填) |
| `d6666666` | (待填) | (low/mid/high) | (待填) |
| `b1111111` | (待填) | (low/mid/high) | (待填) |
| `b2222222` | (待填) | (low/mid/high) | (待填) |
| `c1111111` | (待填) | (low/mid/high) | (待填) |
| `d1111111` | (待填) | (low/mid/high) | (待填) |
| `d2222222` | (待填) | (low/mid/high) | (待填) |
| `ece3aad2` | (待填) | (low/mid/high) | (待填) |
| `21cced62` | (待填) | (low/mid/high) | (待填) |
| `9aa8cdec` | (待填) | (low/mid/high) | (待填) |
| `9633a8ee` | (待填) | (low/mid/high) | (待填) |

### Failure Taxonomy

- `llm_auth_error`: 模型调用鉴权失败（如 401）
- `llm_transport_error`: 模型调用网络/传输失败（超时、连接失败等）
- `tool_call_error`: 工具调用失败（路径、参数、执行错误）
- `context_overtrim`: 上下文裁剪过度导致关键信息缺失
- `memory_conflict`: 记忆冲突/降级导致信息不一致
- `session_state_missing_or_stale`: SessionState 缺失、过期或不一致
- `planning_stall_or_drift`: 规划循环停滞、重规划过多、轨迹漂移
- `done_rule_validation_fail`: 工具成功但收敛判定失败
- `fallback_exhausted`: 主链路失败后 fallback 仍未收敛
- `unknown_failure`: 未命中以上分类的失败

---
*本报告由 trace_eval 自动生成，人工评审后请将标注结果补充到上表中。*
