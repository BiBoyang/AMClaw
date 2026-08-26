# AGENTS.md

@scope:src/mode_policy:v1

## 模块定位

模式策略层，统一判定 restricted/unrestricted 下的 URL 抓取权限。

## 预期职责

1. URL 抓取的运行时权限门禁检查。
2. 对被拒绝行为返回明确原因。
3. 为审计日志提供策略决策结果。

## 不做事项

1. 不实现具体工具逻辑。
2. 不解析业务命令文本。
3. 不直接进行网络请求。
4. 不做工具动作门禁（工具白名单由 `agent_core` 的 `map_llm_plan` 解析与 `tool_registry` 路径边界承担；原 `check_tool_action` 拒绝列表对真实 action 名永不命中，已删除）。
