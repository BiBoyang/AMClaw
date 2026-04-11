# AMClaw 当前状态

更新于 2026-04-12。

这份文件记录的是仓库当前真实状态，不是理想化路线图。  
它的作用是回答三件事：

1. 当前已经做到哪了
2. 现在主要在推进哪一层
3. 下一步最值得继续做什么

关于更长期的 `core/runtime` 与 `skill` 分层路线，不在本文件展开，统一记录在 `CORE-SKILL-ROADMAP.md`。

## 项目定位

AMClaw 是个人 IMAgent 项目，以微信 Bot 为入口，围绕“收消息 -> 入任务 -> 抓取内容 -> 归档 -> 后续扩展”逐步演进。当前仍以单人使用为主，但设计上保留多用户、多任务、多阶段处理的空间。

## 一句话判断

- 架构层：已经从“单一 loop”演进到最小 runtime helper 分层，但还不是完整执行内核
- 业务层：公众号链路已经可用，通用网页已有最小 HTTP 抽取 / 分类，摘要还没有形成稳定闭环
- 工程层：日志与 Trace 基础已经具备，但错误语义、异步化与调度能力还在继续补齐

## 三层视角

当前把仓库状态拆成三层来看会更清楚：

1. **架构 / Runtime 演进**
   - Agent 的执行内核怎么工作
   - `ReAct` 如何进化成更像执行系统的 runtime
   - planner / executor / watchdog / controller 如何分层
2. **业务能力演进**
   - 微信 Bot、任务流、抓取、归档、摘要、日报这些用户可见能力
3. **系统工程演进**
   - 日志、错误、异步化、tracing、调度、多用户、多任务这些工程能力

下面的状态都按这三层来整理。

## 一、架构 / Runtime 演进

### 已完成

- [x] 多 Provider 支持（DeepSeek / Moonshot / OpenAI）
- [x] 基础 Agent 能力可用
- [x] 普通聊天消息可以进入 LLM 回复链路
- [x] Memory v1：显式用户记忆写入与上下文读取
- [x] Memory v2：最小主题 / 偏好自动提炼
- [x] Plan-aware ReAct：`plan` / `progress_note` / `step_status` / `current_step_index`
- [x] 最小完成条件：`expected_observation` / `done_rule` / `expected_fields` / `minimum_novelty`
- [x] 最小失败语义：`retry_step` / `replan` / `ask_user` / `abort`
- [x] 最小重规划范围：`current_step` / `remaining_plan` / `full`
- [x] 最小 watchdog：`repeated_action` / `low_value_observation` / `stalled_trajectory` / `trajectory_drift`
- [x] 最小 `planner / executor / watchdog` helper 分层
- [x] 最小 `state/controller` budget：`max_steps` / `replan_budget`
- [x] Agent Trace 已包含运行上下文、决策、observation、failure、tool call、LLM call 与每日索引

### 进行中

- [ ] `state/controller` 仍是最小版本
  - 当前已有 `max_steps` / `replan_budget`
  - 但还没有更完整的 budget / policy / strategy control
- [ ] Runtime 虽已出现最小分层，但还不是完整执行内核
  - 当前更像“分层 helper”
  - 还不是严格拆开的 planner / executor / watchdog / controller 子系统

### 下一步最值得做

1. 继续把 `state/controller` 从 budget 扩到更完整的策略控制
2. 继续把 runtime 分层从 helper 演进成更明确的执行结构
3. 再评估是否值得引入更细的 budget，例如 step budget / failure budget / tool budget

## 二、业务能力演进

### 微信 Bot

#### 已完成

- [x] iLink 扫码登录
- [x] 消息长轮询收取
- [x] 消息去重与入站消息落库
- [x] 基础聊天流与简单命令处理
- [x] 二维码过期自动刷新，避免登录阶段卡死
- [x] 待提交聊天会话最小持久化与恢复

### 任务系统

#### 已完成

- [x] SQLite 持久化任务表、文章表、消息表
- [x] 链接入库
- [x] 任务状态查询
- [x] 最近任务查询
- [x] 待补录任务查询
- [x] 手动补正文并归档
- [x] 重试任务
- [x] `retry` 现在会直接返回最终状态，而不是只返回中间态 `pending`

### 链接识别

#### 已完成

- [x] `http://` / `https://` 链接识别
- [x] 裸域名链接识别，例如 `mp.weixin.qq.com`
- [x] 混在文本中的链接提取
- [x] 基础去重与 URL 规范化

### 浏览器抓取链路

#### 已完成

- [x] `mp.weixin.qq.com` 优先走浏览器抓取
- [x] Playwright worker 接入
- [x] 原始 HTML 落盘
- [x] 全页截图落盘
- [x] 提取公众号标题与正文并归档 Markdown
- [x] 失败任务进入 `awaiting_manual_input`
- [x] 浏览器抓取来源 `content_source=browser_capture` 持久化
- [x] 页面类型 `page_kind` 持久化并暴露给状态查询

#### 稳定性已完成

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

### 汇总与调度

#### 已完成

- [x] 每日定时任务（本地 Markdown 日报）
- [x] 日报 Markdown 输出

#### 未完成

- [ ] 微信摘要回传
- [ ] 汇总失败补偿策略

### 内容处理深度

#### 当前现状

- [x] 公众号正文提取已经可用
- [x] 通用网页 HTTP 归档已开始尝试最小正文抽取，并记录 `source=http` 与 `page_kind`
- [ ] 通用网页高质量提取、分类、摘要还没有形成稳定闭环

#### 未开始或明显未完成

- [ ] 通用网页抽取策略继续完善
- [ ] 更完整的内容分类体系与分类落库
- [ ] 摘要生成
- [ ] 更明确的 pipeline 阶段状态拆分

### 策略问题

#### 进行中

- [ ] 短链跳公众号的策略决策
  - 当前已修正 HTTP 重定向与最终 URL 判定，避免公众号错误页识别失真
  - 现阶段明确选择“先观察、暂不自动升级 browser capture”
  - 后续是否自动升级到 browser capture，取决于真实样本中的命中频率、正文质量损失与抓取稳定性

### 业务层下一步最值得做

1. 完善通用网页抽取、分类、摘要链路
2. 把内容处理从“公众号可用”推进到“通用网页最小闭环”
3. 再决定短链是否需要自动升级到 browser capture

## 三、系统工程演进

### 已完成

- [x] `chat_adapter` / `pipeline` / `task_store` 已补第一版结构化日志
- [x] `main` / `config` / `agent_core` 已补第一版结构化事件
- [x] 已有最小日志契约测试，防止 payload 字段漂移
- [x] Agent Trace 已有 JSON / Markdown / 每日 `index.jsonl` / 每日 `index.md`

### 进行中

- [ ] 项目状态文档整理
  - 主文档已基本同步到当前状态
  - 模块级文档后续还可以继续细化
- [ ] 日志体系升级
  - `chat_adapter` 的登录、轮询、消息、任务消费日志已基本收口
  - 目前仍有仓库其他模块的旧 `println!` / `eprintln!` 未完全收口
  - 下一步是继续收口剩余旧输出，并统一日志约定

### 未开始或明显未完成

- [ ] `tokio` async runtime 渐进迁移
- [ ] `sqlx` async 化
- [ ] 统一 `thiserror` 错误分层
- [ ] 全链路 `tracing` / `tracing-subscriber`
- [ ] 更完整的会话持久化
- [ ] 更明确的并发任务调度模型
- [ ] 多用户隔离能力

### 系统工程下一步最值得做

1. 继续收口系统级日志与错误语义
2. 再考虑异步化、`tracing` 与错误分层
3. 最后推进更完整的调度 / 多用户 / 多任务演进

## 已验证

以下不是“代码写了”，而是已经实际验证过：

- [x] 真实扫码登录
- [x] 真实消息收发
- [x] 真实链接入任务
- [x] 真实公众号链接浏览器抓取成功
- [x] 真实截图、HTML、归档文件成功落盘
- [x] 真实 `retry` 能重新处理任务并成功归档
- [x] 截图尾部图片渲染问题已修复并人工确认可接受

## 当前最值得继续做的事

如果继续开发，优先级建议如下：

1. 先做业务层的通用网页抽取、分类、摘要最小闭环
2. 再继续收口系统级日志与错误语义
3. 然后回到 runtime，继续把 `state/controller` 从 budget 扩到更完整策略控制
4. 最后再推进更完整的调度 / 多用户 / 多任务演进

## 最近已落地的关键改动

- 普通 HTTP 网页归档已开始尝试最小正文抽取，并向任务状态透出真实 `page_kind` / `content_source`
- Agent runtime 已从“单一 loop”继续演进到最小 `planner / executor / watchdog / controller` helper 结构
- `current_step_index`、richer `expected_observation`、`stalled_trajectory` 与 `replan_budget` 已落地
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
- 后续建议继续用“三层视角”维护：
  - 架构 / Runtime 演进
  - 业务能力演进
  - 系统工程演进
