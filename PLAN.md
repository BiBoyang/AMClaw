# AMClaw 当前状态

更新于 2026-04-11。

这份文件不再记录理想化路线图，而是记录仓库当前真实状态，方便后续判断“做到哪了、下一步该做什么”。

关于新的 `core/runtime` 与 `skill` 分层路线，不放在本文件展开，统一记录在 `CORE-SKILL-ROADMAP.md`。

## 项目定位

AMClaw 是个人 IMAgent 项目，以微信 Bot 为入口，围绕“收消息 -> 入任务 -> 抓取内容 -> 归档 -> 后续扩展”逐步演进。当前仍以单人使用为主，但设计上保留多用户、多任务、多阶段处理的空间。

## 已完成

### 微信 Bot 基础能力

- [x] iLink 扫码登录
- [x] 消息长轮询收取
- [x] 消息去重与入站消息落库
- [x] 基础聊天流与简单命令处理
- [x] 二维码过期自动刷新，避免登录阶段卡死
- [x] 待提交聊天会话最小持久化与恢复

### Agent / LLM

- [x] 多 Provider 支持（DeepSeek / Moonshot / OpenAI）
- [x] 基础 Agent 能力可用
- [x] 普通聊天消息可以进入 LLM 回复链路
- [x] Memory v1：显式用户记忆写入与上下文读取
- [x] Memory v2：最小主题 / 偏好自动提炼
- [x] Plan-aware ReAct：`plan` / `progress_note` / `step_status`
- [x] 最小完成条件：`expected_observation` / `done_rule`
- [x] 最小失败语义：`retry_step` / `replan` / `abort`
- [x] 最小重规划范围：`current_step` / `remaining_plan` / `full`
- [x] 最小 watchdog：`repeated_action` / `low_value_observation` / `trajectory_drift`

### 任务系统

- [x] SQLite 持久化任务表、文章表、消息表
- [x] 链接入库
- [x] 任务状态查询
- [x] 最近任务查询
- [x] 待补录任务查询
- [x] 手动补正文并归档
- [x] 重试任务
- [x] `retry` 现在会直接返回最终状态，而不是只返回中间态 `pending`

### 链接识别

- [x] `http://` / `https://` 链接识别
- [x] 裸域名链接识别，例如 `mp.weixin.qq.com`
- [x] 混在文本中的链接提取
- [x] 基础去重与 URL 规范化

### 浏览器抓取链路

- [x] `mp.weixin.qq.com` 优先走浏览器抓取
- [x] Playwright worker 接入
- [x] 原始 HTML 落盘
- [x] 全页截图落盘
- [x] 提取公众号标题与正文并归档 Markdown
- [x] 失败任务进入 `awaiting_manual_input`
- [x] 浏览器抓取来源 `content_source=browser_capture` 持久化
- [x] 页面类型 `page_kind` 持久化并暴露给状态查询

### 浏览器抓取稳定性

- [x] worker 超时 watchdog
- [x] 更细失败分类：
  - `browser_launch_failed`
  - `browser_context_failed`
  - `browser_navigation_timeout`
  - `browser_navigation_failed`
  - `browser_content_failed`
  - `browser_screenshot_failed`
  - `browser_worker_failed`
  - `browser_worker_timeout`
- [x] 失败日志回传到任务状态
- [x] 截图前主动触发公众号长文懒加载图片
- [x] 滚动整页并等待图片渲染后再截图
- [x] 已在真实 Bot 中完成一次端到端验证

## 已验证

以下不是“代码写了”，而是已经实际验证过：

- [x] 真实扫码登录
- [x] 真实消息收发
- [x] 真实链接入任务
- [x] 真实公众号链接浏览器抓取成功
- [x] 真实截图、HTML、归档文件成功落盘
- [x] 真实 `retry` 能重新处理任务并成功归档
- [x] 截图尾部图片渲染问题已修复并人工确认可接受

## 进行中

这些部分不是空白，但还没有收敛成“稳定完成态”：

- [ ] 项目状态文档整理
  - 主文档已基本同步到当前状态
  - 模块级文档后续还可以继续细化

- [ ] 日志体系升级
  - `chat_adapter` / `pipeline` / `task_store` 已补第一版结构化日志
  - `chat_adapter` 的登录、轮询、消息、任务消费日志已基本收口
  - `main` / `config` / `agent_core` 的启动与内部旧输出已补第一版结构化事件
  - 已有最小日志契约测试，防止 payload 字段漂移
  - 目前仍有仓库其他模块的旧 `println!` / `eprintln!` 未完全收口
  - 已有 Agent Trace、Markdown Trace 与每日 `index.jsonl`
  - Agent Trace 已接到真实聊天链路，并补充 `source_type`、`trigger_type`、`user_id`、`message_ids`
  - 每天可读的 `index.md` 已补齐
  - 下一步是继续收口剩余旧输出，并统一日志约定

- [ ] 内容处理深度
  - 目前公众号正文提取可用
  - 通用网页提取、分类、摘要还没有形成稳定闭环

- [ ] 短链跳公众号的策略决策
  - 当前已修正 HTTP 重定向与最终 URL 判定，避免公众号错误页识别失真
  - 现阶段明确选择“先观察、暂不自动升级 browser capture”
  - 后续是否自动升级到 browser capture，取决于真实样本中的命中频率、正文质量损失与抓取稳定性

## 未开始或明显未完成

### 基础设施

- [ ] tokio async runtime 渐进迁移
- [ ] `sqlx` async 化
- [ ] 统一 `thiserror` 错误分层
- [ ] 全链路 `tracing` / `tracing-subscriber`

### 内容处理

- [ ] 通用网页抽取策略完善
- [ ] 更完整的内容分类体系
- [ ] 摘要生成
- [ ] 更明确的 pipeline 阶段状态拆分

### 汇总与调度

- [x] 每日定时任务（本地 Markdown 日报）
- [x] 日报 Markdown 输出
- [ ] 微信摘要回传
- [ ] 汇总失败补偿策略

### 多用户 / 多任务演进

- [ ] 更完整的会话持久化
- [ ] 更明确的并发任务调度模型
- [ ] 多用户隔离能力

## 当前最值得继续做的事

如果继续开发，优先级建议如下：

1. 把 `current_step_index` 与 richer `expected_observation` 做稳
2. 继续增强 `StepFailureKind / FailureAction`
3. 继续增强 trajectory / drift 检测
4. 完善通用网页抽取、分类、摘要链路
5. 再考虑 planner / executor / watchdog 分层

## 最近已落地的关键改动

- 浏览器 worker 失败分类与日志补强
- worker stdin 关闭问题修复，解决假性超时
- 公众号截图前懒加载图片触发与等待
- 非公众号链接允许 HTTP 跟随重定向，公众号链接继续保留重定向保护
- browser worker 返回产物路径校验，避免主进程信任异常响应
- 二维码过期自动刷新
- 裸域名链接自动识别
- `retry` 直接返回最终状态
- Agent Trace 全量 JSON 落盘
- Agent Trace Markdown 摘要化
- Agent Trace 每日 `index.jsonl`
- Agent Trace 每日 `index.md`
- 真实聊天链路 Trace 上下文透传（`source_type` / `trigger_type` / `user_id` / `message_ids`）
- `chat_adapter` / `pipeline` / `task_store` 第一版结构化日志
- `main` / `config` / `agent_core` 第一版结构化事件
- 最小日志契约测试（chat / pipeline / task_store）

## 当前仓库外的说明

- `PLAN.md` 现在是状态文档，不是承诺式路线图
- 如果后续进入新阶段，建议继续按“已完成 / 进行中 / 未开始”维护，而不是回到纯规划文档
