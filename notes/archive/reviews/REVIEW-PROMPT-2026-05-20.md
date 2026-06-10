你是一位资深 Rust 代码审查员。请审查 AMClaw 项目在 2026-05-20 的以下改动。

## 项目背景

AMClaw 是一个 Rust 微信 Bot / Agent 学习项目（~32K 行，14 个模块，394 个测试）。功能包括微信扫码登录、消息收发、公众号链接浏览器抓取、正文 Markdown 归档、LLM 驱动的 ReAct agent。

## 审查的 commit 范围

```
f32590d..2222ce1
```

仓库地址：`https://github.com/BiBoyang/AMClaw`

## 改动概览（6 个 commit）

### 1. `f32590d` — 修复 WeChat 公众号代码块 Markdown 转换

公众号文章使用 `<pre><code>` 逐行 + `<span>` 语法着色的 HTML 结构。旧转换器把代码拆散为逐词换行的纯文本。新增 `replace_pre_blocks` 函数，在 normalize 前用占位符隔离代码块，最终输出 ``` 围栏代码块。

**涉及文件**：`src/pipeline/mod.rs`, `src/task_store/schema.rs`
**改动量**：+195/-11 行

### 2. `e4172dd` — pipeline/mod.rs 拆分

将 2261 行的单文件拆为 4 个文件：
- `pipeline/html_extract.rs` (289 行)：HTML 解析、标题提取、页面分类
- `pipeline/markdown.rs` (253 行)：Markdown 转换、代码块处理、实体解码
- `pipeline/logging.rs` (26 行)：日志包装
- `pipeline/mod.rs` (1715 行)：Pipeline 编排、URL 检测、WeChat 错误处理、测试

**涉及文件**：3 个新文件 + `pipeline/mod.rs` 删减
**改动量**：+756/-572 行
**约束**：纯内部重构，对外接口不变，394 测试全通过

### 3. `00cded1` — 日志包装去重 + dead code

- 在 `src/logging.rs` 新增 `define_module_loggers!` 声明宏
- 替换 4 个模块（agent_core, task_store, pipeline, task_executor）中重复的 log_*_info/warn/error 包装
- 删除 `mode_policy` 中无人调用的 `log_policy_info` / `log_policy_warn`
- 删除 `agent_core` 中无引用的 `RunContext` 类型别名

**涉及文件**：`src/logging.rs`, 4 个 `logging.rs`, `mode_policy/mod.rs`, `agent_core/mod.rs`
**改动量**：+315/-71 行

### 4. `af0d9e5` — 错误处理韧性加固

- `TaskExecutor` 新增 `Drop` impl：先 drop sender 通知 worker，再 join 线程（不再 detach）
- `TaskStore` 新增 `reset_expired_leases()`：worker 启动时清理过期 lease
- `task_executor` 日志包装替换为宏

**涉及文件**：`src/task_executor/mod.rs`, `src/task_store/tasks.rs`
**改动量**：+51/-16 行

### 5. `9e7a414` — lint 配置 + 安全加固

- `Cargo.toml`：添加 `[lints.rust]` (unsafe_code = deny) 和 `[lints.clippy]` (dbg_macro = warn)
- `pipeline/mod.rs`：`fetch_html_once` 发 HTTP 请求前增加 DNS 二次校验（防 TOCTOU）
- `task_store/schema.rs`：`ensure_column_exists` 添加安全注释

**涉及文件**：`Cargo.toml`, `pipeline/mod.rs`, `task_store/schema.rs`
**改动量**：+25/-6 行

### 6. `2222ce1` — 文档更新

更新 `notes/review-2026-05-20.md` 记录全部批次的完成状态。

## 审查要点

请按以下维度审查：

### A. 正确性
1. `replace_pre_blocks` 的占位符逻辑是否存在边界情况（空 `<pre>`、嵌套标签、非标准大小写）？
2. `define_module_loggers!` 宏的三个 arm 是否正确覆盖所有调用方？visibility (`pub` vs `pub(crate)`) 是否正确？
3. `TaskExecutor::Drop` 的 `sender.take()` + `worker.take().join()` 顺序是否保证 worker 先收到 shutdown 信号再被 join？
4. `reset_expired_leases` 的 SQL 是否正确？是否会误重置别的 worker 正在处理的任务？

### B. 安全性
1. DNS TOCTOU 校验放在 `fetch_html_once` 中，是否每次 HTTP 请求都会检查（包括重试）？
2. pipeline 拆分为子模块后，`ExtractedArchiveBody` 类型的可见性 (`pub(crate)`) 是否合适？
3. 是否有任何新的 `.unwrap()` / `.expect()` 调用以不当方式引入？

### C. 代码质量
1. 新宏 `define_module_loggers!` 使用 `#[macro_export]` + `crate::` 调用，跨模块使用时路径是否正确？
2. `html_extract.rs` 和 `markdown.rs` 的函数可见性是否恰当（哪些该 `pub(crate)` 哪些该私有）？
3. 测试模块的 import 路径（从 `super::` 改为 `super::html_extract::` 等）是否都正确？
4. 是否有任何新增的 `#[allow(dead_code)]` 或 clippy 抑制？

### D. 测试
1. 4 个新增测试是否覆盖了关键路径：
   - `strip_html_tags_removes_all_tags`
   - `decode_entities_converts_common`
   - `pre_block_converts_to_code_fence`
   - `pre_block_wechat_code_snippets`
   - `html_fragment_to_markdown_preserves_code_block`
2. 是否有遗漏的测试场景（如空 `<pre>`、多层嵌套标签、HTML 实体边界）？

## 审查方式

```bash
git clone https://github.com/BiBoyang/AMClaw.git
cd AMClaw
git log --oneline f32590d..2222ce1   # 查看 commit 列表
git diff f32590d..2222ce1            # 查看全部 diff
cargo check                           # 验证编译
cargo test --lib                      # 验证测试
cargo clippy --all-targets --all-features  # 验证 lint
```

请对每个审查点给出 "通过 / 需改进 / 有问题" 的结论，并对需改进或有问题的项目给出具体建议。
