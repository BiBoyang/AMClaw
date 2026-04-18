# Context 技术文章发布作战手册（2026-04-18）

> 目标：把 AMClaw 上下文工程沉淀为可发布技术文章（长文 + 短文）

---

## 0. 输入材料（已就绪）

- 主稿：`notes/context-memory/CONTEXT-TECH-ARTICLE-DRAFT-2026-04-18.md`
- 技术历程：`notes/context-memory/CONTEXT-TECH-SELECTION-JOURNEY.md`
- C0/C1/C2 方案：`notes/context-memory/SESSION-STATE-C0-C1-C2-PLAN-2026-04-17.md`
- C3/C4 方案：`notes/context-memory/CONTEXT-PACK-C3-PLAN-2026-04-18.md`
- Trace 闭环报告：`notes/agent-eval/reports/TRACE-EVAL-REPORT.md`
- 当日 session：`sessions/SESSION-2026-04-18.md`

---

## 1. 发布产物定义（必须有）

1. 长文（技术社区版）
   - 约 3k~5k 字
   - 完整讲清：问题 -> 选型 -> 接线 -> 边界 -> 下一步
2. 短文（公众号/朋友圈版）
   - 约 800~1200 字
   - 强调结论与方法，不展开代码细节
3. 配图（至少 2 张）
   - 图 1：SessionState 接线图（chat -> store -> agent）
   - 图 2：ContextPack 组装图（sources -> pack -> render -> trace）

---

## 2. 长文改稿清单（按顺序）

1. 开头 3 段重写成“问题驱动”
   - 不先讲实现，先讲痛点：为何“能跑”仍不够
2. 合并第 3~5 节重复表述
   - 保留每节一句核心结论 + 一段接线摘要
3. 强化第 6~7 节“怎么接进去”
   - 每节补 3 行：改动文件、入口函数、可观测字段
4. 第 8 节新增“明确不做”
   - embedding/taxonomy/全量 async 为什么后置
5. 第 9 节补“收益与代价”
   - 收益：可解释、可回归
   - 代价：复杂度与参数治理成本
6. 第 10 节改为可执行路线
   - C5 变成 2 周里程碑，给 DoD

---

## 3. 质量闸门（发布前必须过）

1. 敏感信息检查
   - 不出现本机目录、用户标识、token、密钥
2. 术语一致
   - `SessionState` / `ContextPack` / `drop reason` 前后一致
3. 事实一致
   - 测试数、阶段名、版本号和仓库状态一致
4. 可读性检查
   - 每节首段可独立阅读，结论句清晰

---

## 4. 发布节奏（建议）

- D1 上午：长文改稿 + 补图草图
- D1 下午：短文抽取 + 自查
- D2 上午：最终校对 + 发布
- D2 下午：把发布链接回填到 `sessions/SESSION-2026-04-18.md`

---

## 5. 完成标准（DoD）

- 长文与短文都完成并可对外发布
- 至少 2 张结构图可读
- 已完成敏感信息与术语一致性检查
- Session 日志记录“发布时间 / 发布渠道 / 链接”
