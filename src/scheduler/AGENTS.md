# AGENTS.md

@scope:src/scheduler:v1

## 模块定位

定时调度层，触发每日汇总与周期性后台任务。

## 预期职责

1. 根据配置时间触发日常整理（每日 / 每周报告生成），是报告快照的唯一主动生成触发点。
2. 到点判断由 `DailyReportSchedule` / `WeeklyReportSchedule::should_run_now` 单点提供，spawn loop 与 chat_adapter 定时推送共用同一份实现。
3. `report_to_user_id` 仅决定推送目标；缺省时调度仍照常生成快照，仅推送跳过。
4. 提供一次性手动触发入口。
5. 对调度失败做重试或补偿标记。

## 不做事项

1. 不负责文章抓取细节。
2. 不直接承担微信网关逻辑。
3. 不保存复杂业务状态。
