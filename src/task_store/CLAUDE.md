# CLAUDE.md

@scope:src/task_store:v1

## 模块定位

持久化层，管理文章、任务、消息去重、入站消息、会话状态、用户记忆、embedding 缓存与出站待补发队列。

## 当前职责

1. 初始化 SQLite 数据库与基础表结构，并通过 ensure_column_exists 做幂等列迁移（兼容旧库）。
2. 持久化消息去重记录，保障跨重启幂等。
3. 持久化入站文本消息原文，供调试与后续流程使用。
4. 管理文章、任务的入库、状态查询、最近任务查询与重试操作。
5. 持久化任务来源、页面类型、截图路径、归档路径与人工补录状态。
6. 支持待人工补录任务列表查询。
7. 提供任务 lease/worker 机制：claim_task 原子领取、list_claimable_tasks 列出可领取任务、reset_expired_leases 启动时重置过期 lease。
8. 持久化 context_token、合并会话文本（user_sessions）与 7-slot 结构化会话状态（user_session_states，v2），并提供 TTL 过期清理。
9. 持久化用户记忆并承担写侧治理：govern_memory_write（validate → dedup → promote/skip → persist）、feedback 计数写回、显式确认有用与抑制。
10. 持久化 embedding 向量缓存（单条/批量读写），供 retriever 的 CachedEmbeddingProvider 使用。
11. 持久化出站待补发消息段（outbound_pending_chunks），支持插入、按序查询与删除。
12. 提供 URL 规范化与内网/本地地址拦截（SSRF 防护）。

## 后续职责

1. 提供更完整的文章与任务状态读写接口。
2. 保障同一 URL 或消息不重复处理。
3. 记录失败原因与重试次数。

## 不做事项

1. 不做网络抓取。
2. 不做微信发送。
3. 不做业务流程编排。
4. 不在本层决定消息应进入聊天流还是链接流。
5. 不读写 daily_reports 表：该表已废弃（全仓无读写代码），schema 保留仅为兼容旧库。
