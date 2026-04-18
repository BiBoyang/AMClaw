@scope:src/bin:v1

## 作用

本目录存放可独立运行的 CLI 二进制工具，不承载主服务逻辑。

## 当前内容

- `context_eval.rs`：离线评测 session summary 策略（semantic vs truncate）的可执行文件。
- `trace_eval.rs`：离线评测 Agent trace 的可执行文件，产出多维度统计报告。

## 约束

1. 禁止在 bin 目录中引入网络依赖或外部服务调用，除非明确标注。
2. 评测类工具必须纯离线运行，不得使用真实敏感数据。
3. 新增 binary 需在 README 中补充简要使用说明。
