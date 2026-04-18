# Trace Evaluation Report

- generated: 2026-04-18T02:33:30.357317+00:00
- total traces: 4
- interesting traces: 4

## Summary Statistics

| metric | count | ratio |
| --- | ---: | ---: |
| total | 4 | 100% |
| success | 4 | 100.0% |
| with memory injected | 0 | 0.0% |
| with memory dropped | 0 | 0.0% |
| with session state | 0 | 0.0% |
| with context pack dropped | 0 | 0.0% |
| with llm fallback | 4 | 100.0% |
| with failures | 0 | 0.0% |

## Per-Trace Detail

| run_id | success | steps | mem(r/i/d) | state | ctx_drop | failures | reasons | input |
| --- | --- | ---: | --- | --- | --- | --- | --- | --- |
| `ece3aad2` | ✓ | 2 | 0/0/0 | · | · | 0 | llm_fallback | 读文件 README.md |
| `21cced62` | ✓ | 2 | 0/0/0 | · | · | 0 | llm_fallback | 读文件 README.md |
| `9aa8cdec` | ✓ | 2 | 0/0/0 | · | · | 0 | llm_fallback | 读文件 README.md |
| `9633a8ee` | ✓ | 2 | 0/0/0 | · | · | 0 | llm_fallback | 读文件 README.md |

## Interesting Traces Deep Dive

### `ece3aad2-a621-4619-ab3b-93abba3ea3c2`

- **user_input**: 读文件 README.md
- **success**: true
- **duration**: 3ms, **steps**: 2
- **memory**: retrieved=0, injected=0, dropped=0, total_chars=0
- **session_state**: false
- **context_pack**: dropped=false, reasons=[]
- **llm_calls**: total=1, success=0, failed=1
- **tool_calls**: total=1, success=1
- **interest_reasons**: llm_fallback

### `21cced62-914e-4453-b2dc-352d363c67e7`

- **user_input**: 读文件 README.md
- **success**: true
- **duration**: 5ms, **steps**: 2
- **memory**: retrieved=0, injected=0, dropped=0, total_chars=0
- **session_state**: false
- **context_pack**: dropped=false, reasons=[]
- **llm_calls**: total=1, success=0, failed=1
- **tool_calls**: total=1, success=1
- **interest_reasons**: llm_fallback

### `9aa8cdec-ed66-4731-94a4-638e57ab2976`

- **user_input**: 读文件 README.md
- **success**: true
- **duration**: 4ms, **steps**: 2
- **memory**: retrieved=0, injected=0, dropped=0, total_chars=0
- **session_state**: false
- **context_pack**: dropped=false, reasons=[]
- **llm_calls**: total=1, success=0, failed=1
- **tool_calls**: total=1, success=1
- **interest_reasons**: llm_fallback

### `9633a8ee-6479-45ba-94a0-ac3831af5727`

- **user_input**: 读文件 README.md
- **success**: true
- **duration**: 4ms, **steps**: 2
- **memory**: retrieved=0, injected=0, dropped=0, total_chars=0
- **session_state**: false
- **context_pack**: dropped=false, reasons=[]
- **llm_calls**: total=1, success=0, failed=1
- **tool_calls**: total=1, success=1
- **interest_reasons**: llm_fallback

## Failure Taxonomy Annotation Template

对以上 interesting traces 进行人工评审时，可按下表标注：

| run_id | primary_failure | severity | notes |
| --- | --- | --- | --- |
| `ece3aad2` | (待填) | (low/mid/high) | (待填) |
| `21cced62` | (待填) | (low/mid/high) | (待填) |
| `9aa8cdec` | (待填) | (low/mid/high) | (待填) |
| `9633a8ee` | (待填) | (low/mid/high) | (待填) |

### Failure Taxonomy

- `forgot_known_fact`: 系统明知但本次未使用
- `missed_retrieval`: 应该检索到记忆但没检索到
- `wrong_retrieval`: 检索到了不相关记忆
- `overcompressed_summary`: session summary 丢失了关键信息
- `state_drift`: session state 与实际情况不一致
- `repeated_work`: 重复执行了已完成的步骤
- `llm_error`: LLM 调用失败或返回无效
- `tool_error`: 工具执行失败
- `other`: 其他

---
*本报告由 trace_eval 自动生成，人工评审后请将标注结果补充到上表中。*
