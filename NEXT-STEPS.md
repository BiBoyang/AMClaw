# NEXT-STEPS

当前这份文件只记录"接下来最值得做什么"，不重复描述已经完成的能力。当前真实状态请看 `PLAN.md`。

## 本阶段收口（截至 2026-04-12）

以下主线可视为已完成并进入稳定维护：

- Plan-aware ReAct 主链路（含失败语义与最小 watchdog）
- 通用 HTTP 归档最小 summary（规则法）与 `summary` 落库
- `page_kind` 五分类（`error_page/article/index_like/link_post/webpage`）
- reporter / 日报对 `summary` 的展示接入
- 发布流程与文档结构整理（`notes/`、`sessions/`）
- Memory v3：`search_user_memories` 接入 agent_core context 拼装 + 命中回写 + 可观测日志 + 回归测试

结论：v0.3.2 "Context & Memory Minimal" 可以收口。

## v0.3.2 DoD 逐项确认

1. ✅ 显式记忆可命中：`记住 我喜欢短摘要` 后，下一轮问答可体现偏好
2. ✅ 用户隔离有效：A 用户记忆不会注入到 B 用户（回归测试已覆盖）
3. ✅ 长度治理有效：context/memory 注入有预算（5 条 / 500 字符 / 单条 160 字符）
4. ✅ 退化正常：无记忆 / 无 user_id 时系统不报错，行为可回退到当前基线
5. ✅ 可观测：日志有 `memory_hit_count`、`memory_total_chars`、`memory_ids`；Trace 有 `memory_hit_count` / `memory_total_chars`

## 当前主线（v0.3.3）

### 目标

基于已落地的 Memory 消费链路，继续推进系统工程层的补齐。

### 方向

1. 继续收口系统级日志与错误语义
2. 再考虑异步化、`tracing` 与错误分层
3. 然后用轻量评测驱动 runtime 稳定性增强
4. 最后再推进更完整的调度 / 多用户 / 多任务演进

### 不优先做

- 不先上 embedding / 向量库
- 不先做复杂 memory taxonomy
- 不先做多用户/多任务架构重构
- 不先做 `tokio` 全量迁移或 `sqlx` async 化
- 不回头重写 ReAct / Planning 主框架

## 当前明确不优先做

- 不先上 embedding / 向量库
- 不先做复杂 memory taxonomy
- 不先做多用户/多任务架构重构
- 不先做 `tokio` 全量迁移或 `sqlx` async 化
- 不回头重写 ReAct / Planning 主框架
