# AGENTS.md

@scope:src/task_executor:v1

## 模块定位

任务执行器，负责将 pipeline 任务从 poll_loop 主线程解耦到独立 worker 线程。

## 当前职责

1. 提供 TaskExecutor，用 mpsc channel 接收 task_id。
2. 在独立 worker 线程中执行抓取、归档与状态更新。
3. 使 poll_loop 不被慢任务阻塞，保障聊天收发实时性。

## 不做事项

1. 不管理消息去重或会话状态。
2. 不做微信协议对接。
3. 不决定任务何时被查询或重试。
