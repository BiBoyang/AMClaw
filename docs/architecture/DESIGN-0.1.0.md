# AMClaw 0.1.0 设计文档

## 1. 背景与目标

本项目 0.1.0 不是通用自动化平台，而是一个连接验证与场景闭环版本：

1. 通过微信消息入口接收文章链接。
2. 自动处理链接（快照、抽取、分类、归档）。
3. 将结果写入本地固定目录（目录位置可配置）。
4. 每日定时汇总当日收藏文章，生成简报。

本版本强调可控、可观测、可恢复，不追求通用电脑控制能力。

## 2. 版本范围

### 2.1 0.1.0 In Scope（必须实现）

1. 微信消息接入与去重。
2. URL 提取、规范化、入库。
3. 文章抓取快照与正文抽取。
4. LLM 简分类与简摘要。
5. 每日定时汇总报告。
6. 所有产物仅写入配置目录 `root_dir`。
7. 失败任务可重试，且有状态记录。

### 2.2 0.1.0 Out of Scope（暂不实现）

1. 任意 shell 执行。
2. 任意路径文件读写。
3. GUI 自动化点击与系统级控制。
4. 多用户、多节点分布式部署。
5. 高并发吞吐优化。

## 3. 模式策略（核心要求）

### 3.1 受限模式 `restricted`（默认）

仅允许以下工具能力：

1. `save_snapshot`
2. `extract_content`
3. `classify_article`
4. `write_report`
5. `list_files_under_root`

禁止以下能力：

1. 任意命令执行
2. 任意路径写入
3. GUI/系统自动化
4. 外部未声明工具

### 3.2 不受限模式 `unrestricted`（预留）

0.1.0 不启用不受限执行，但必须预留架构接口：

1. 预留模式枚举值与策略分支。
2. 预留高风险工具注册位（例如 `system_action`）。
3. 在受限模式下若触发高风险命令，返回明确拒绝与原因。

注意：0.1.0 的运行配置中若设置 `unrestricted`，系统应直接拒绝启动或降级到 `restricted`（二选一，待讨论确定）。

## 4. 总体架构

建议采用单进程、分层模块架构（保持 Demo 简洁）：

1. `chat_gateway`：聊天应用登录、轮询、发送回执。
2. `command_router`：解析微信文本，抽取 URL 与命令。
3. `task_store`：SQLite 持久化任务、文章、日志。
4. `scheduler`：每日定时触发整理任务。
5. `pipeline`：抓取 -> 快照 -> 抽取 -> 分类 -> 归档。
6. `agent_core`：LLM 调度与工具调用编排。
7. `tool_registry`：工具声明、元信息与风险标记。
8. `mode_policy`：运行模式权限判断（restricted/unrestricted）。
9. `reporter`：生成每日 Markdown 报告并回微信摘要。

## 5. 数据流

### 5.1 实时入库流

1. 微信消息进入 `chat_gateway`。
2. `command_router` 提取 URL。
3. 规范化 URL（去尾斜杠、去追踪参数等策略待定）。
4. 写入 `articles` 与 `tasks`，初始状态 `pending`。
5. 微信回执：`已接收` + `task_id`。

### 5.2 处理流（Pipeline）

1. Worker 领取 `pending` 任务。
2. 执行 `save_snapshot`（输出到 `root_dir/snapshots/...`）。
3. 执行 `extract_content`（输出结构化正文）。
4. 执行 `classify_article`（类别、关键词、简摘要）。
5. 写入归档文件与元数据，状态进入 `archived`。

### 5.3 每日汇总流

1. `scheduler` 在配置时间触发。
2. 聚合当日 `archived` 文章。
3. 生成 `daily-YYYY-MM-DD.md` 到 `root_dir/reports/`。
4. 微信发送简版摘要与报告路径。

## 6. 文件布局（产物目录）

假设 `root_dir=/Users/boyang/ArticleInbox`：

1. `root_dir/raw/YYYY-MM-DD/` 原始抓取响应与中间数据
2. `root_dir/snapshots/YYYY-MM-DD/` 网页快照（html/png/pdf 可选）
3. `root_dir/processed/YYYY-MM-DD/` 清洗正文与分类 JSON
4. `root_dir/reports/` 每日汇总 Markdown
5. `root_dir/logs/` 运行日志（可选）

所有写操作必须经过路径校验：

1. 将目标路径 `canonicalize`。
2. 校验其前缀为 `root_dir`。
3. 不通过则拒绝执行并记审计日志。

## 7. 配置设计（草案）

```toml
[agent]
mode = "restricted"              # restricted | unrestricted
timezone = "Asia/Shanghai"

[storage]
root_dir = "/Users/boyang/ArticleInbox"

[scheduler]
enabled = true
daily_run_time = "22:30"

[llm]
provider = "openai"
model = "gpt-4.1-mini"
max_tokens = 800

[wechat]
channel_version = "1.0.0"
```

说明：

1. 0.1.0 推荐强制 `mode=restricted`。
2. `unrestricted` 仅作配置占位，不开放实际高风险能力。

## 8. 数据模型（SQLite 草案）

### 8.1 `articles`

1. `id` TEXT PRIMARY KEY
2. `normalized_url` TEXT UNIQUE NOT NULL
3. `original_url` TEXT NOT NULL
4. `title` TEXT
5. `source_domain` TEXT
6. `created_at` DATETIME NOT NULL
7. `updated_at` DATETIME NOT NULL

### 8.2 `tasks`

1. `id` TEXT PRIMARY KEY
2. `article_id` TEXT NOT NULL
3. `status` TEXT NOT NULL
4. `retry_count` INTEGER NOT NULL DEFAULT 0
5. `last_error` TEXT
6. `created_at` DATETIME NOT NULL
7. `updated_at` DATETIME NOT NULL

### 8.3 `message_dedup`

1. `message_id` TEXT PRIMARY KEY
2. `from_user_id` TEXT NOT NULL
3. `received_at` DATETIME NOT NULL

### 8.4 `daily_reports`

1. `date` TEXT PRIMARY KEY
2. `report_path` TEXT NOT NULL
3. `summary` TEXT
4. `created_at` DATETIME NOT NULL

## 9. 状态机（任务级）

状态建议：

1. `pending`
2. `snapshotted`
3. `extracted`
4. `classified`
5. `archived`
6. `failed`

规则：

1. 每步落库后才进入下一步。
2. 失败进入 `failed`，支持手动重试。
3. 同一 `normalized_url` 重复提交时，返回已存在任务信息（幂等）。

## 10. 命令协议（微信侧草案）

1. `收藏 <url>`：新增文章处理任务。
2. `今日整理`：立即触发当日汇总。
3. `状态 <task_id>`：查看任务状态。
4. `重试 <task_id>`：重试失败任务。
5. `帮助`：返回命令列表。

预留但不启用：

1. `执行 <action>`：受限模式下统一返回拒绝说明。

## 11. 安全与风控（0.1.0）

1. 路径白名单：仅允许写入 `root_dir`。
2. 工具白名单：仅注册低风险处理工具。
3. 模式门禁：执行前统一经过 `mode_policy`。
4. 敏感日志脱敏：隐藏 token、cookie、授权头。
5. 审计记录：记录每次工具调用请求与结果（成功/拒绝/失败）。

## 12. 可观测性

最小监控指标：

1. 入站消息数（每小时）
2. 新增任务数、完成数、失败数
3. 任务平均处理耗时与 P95
4. 每日汇总成功率
5. 去重命中率（重复消息/重复 URL）

日志分级建议：

1. `INFO`：状态推进与关键事件。
2. `WARN`：重试与降级。
3. `ERROR`：任务失败与不可恢复错误。
4. `DEBUG`：协议响应详情（默认关闭）。

## 13. 里程碑建议

### M1: 链路闭环（1-2 天）

1. 微信收链接 -> 入库 -> 回执。
2. 任务状态机打通（无 LLM 可先用假分类）。

### M2: 内容处理（2-4 天）

1. 网页快照与正文抽取。
2. LLM 分类与简摘要。
3. 归档文件输出到固定目录。

### M3: 定时汇总（1-2 天）

1. 每日定时任务。
2. 日报 Markdown 输出与微信摘要回传。

## 14. 关键未决问题（待讨论）

1. `unrestricted` 在 0.1.0 是拒绝启动，还是启动但禁用高风险工具。
2. 快照格式优先级（html / png / pdf）。
3. URL 规范化规则范围（是否去除全部 tracking 参数）。
4. LLM 分类体系（固定标签还是动态标签）。
5. 每日汇总时间与失败补偿策略（漏跑是否补跑）。

---

当前文档定位：0.1.0 架构草案。  
下一步可基于本文件冻结接口与表结构，再进入代码改造。
