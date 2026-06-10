# Plan Card: S18 + S19 + S20（2026-05-08）

## 0) 背景基线

- 仓库：`/Users/boyang/Desktop/AMClaw`
- 当前分支：`main`
- 基线提交：`348eaec`（`docs: S17 doc sync for script lint, CI gate and session index`）
- 已完成主线：
  - S11: `trace_eval --gate-json` 结构化输出
  - S12/S13: CI soft gate 改为 JSON 消费并补鲁棒分支
  - S14/S15: `trace_soft_gate.sh` 抽离 + 回归测试接线
  - S16: `shellcheck` + CI lint 接线
  - S17: 文档轻量收口（README/DEVELOPMENT/notes/sessions）

## 1) 总目标（本计划要达成什么）

在不改变核心业务功能的前提下，完成三条工程收口线：

1. S18：CI 确定性加固（脚本 lint 环境稳定、可观测）
2. S19：Gate 策略升级（soft/hard 可治理、可切换）
3. S20：状态文档时效性收口（减少实现与文档漂移）

## 2) Out of Scope（明确不做）

- 不改 `trace_eval` 指标口径与评分规则（`specs` 内定义保持不动）
- 不重构既有 Rust 主链路逻辑
- 不进行全仓文档重写或目录重组
- 不引入与本目标无关的新工具链（例如额外 formatter/linter 套件）

## 3) 执行顺序与依赖

- 推荐顺序：`S18 -> S19 -> S20`
- 依赖关系：
  - S19 依赖 S18 的 CI 基础确定性（至少 shellcheck 环境明确）后再做策略升级更稳妥。
  - S20 可部分并行，但建议在 S18/S19 结论稳定后统一同步，避免二次改文档。

## 4) S18 - CI 确定性加固（Shell 脚本 lint 环境）

### S18 目标

- 避免 `ubuntu-latest` 镜像变化导致 `shellcheck` 缺失或版本漂移引发假故障。
- CI 日志中可直接看到 `shellcheck` 来源与版本。

### S18-1 现状审计与策略选型

- 任务：
  - 审计当前 workflow 的脚本 lint 依赖（是否显式安装、是否版本固定）。
  - 形成“安装策略二选一”并择一落地：
    - A: 显式安装（apt/action）
    - B: 显式校验预装并在缺失时失败
- DoD：
  - 有清晰选型结论（含理由：稳定性/维护成本/可读性）。
  - 记录在提审包中（不是口头）。

### S18-2 Workflow 落地改造

- 任务：
  - 在 `.github/workflows/trace-eval-compare.yml` 增加/调整 shellcheck 准备步骤。
  - 在 lint 前输出 `shellcheck --version`（日志可观测）。
- DoD：
  - CI 中 shellcheck 来源明确，版本可见。
  - `Lint shell scripts` 仍使用 `make lint-scripts`，入口不分叉。

### S18-3 本地一致性与兜底（可选但建议）

- 任务（可选）：
  - 在 `Makefile` 或开发文档中增加“本地缺 shellcheck 的提示路径”。
- DoD：
  - 本地开发者可快速知道缺依赖时怎么补齐。

### S18-4 验证与提审

- 最低验证：
  - `make lint-scripts`
  - `bash scripts/tests/test_trace_soft_gate.sh`
  - `cargo check --bin trace_eval`
- DoD：
  - 本地验证通过。
  - workflow diff 与验证结果可复现。

## 5) S19 - Gate 策略升级（Soft -> Hard 可治理）

### S19 目标

- 把“是否阻断合并”从隐式约定升级为显式策略：
  - 条件清楚
  - 开关清楚
  - 回滚路径清楚

### S19-1 策略定义文档（先文档后实现）

- 任务：
  - 定义 Gate 模式：
    - `soft`：仅告警不阻断
    - `hard`：按规则阻断（建议至少 FAIL 阻断）
  - 明确升级触发条件（例：连续 N 次稳定、误报率阈值、基线样本完备度）。
  - 明确回退条件（例：误报升高、样本失真、基础设施异常）。
- DoD：
  - 形成独立策略说明（建议落在 `notes/agent-eval/specs/` 或 `notes/agent-eval/plans/`）。
  - 不修改指标定义文档中的统计口径。

### S19-2 Workflow 策略开关实现

- 任务：
  - 在 CI 中实现可切换 gate mode（例如 env/step 级开关）。
  - 默认模式建议先保持 `soft`，由配置控制切换。
- DoD：
  - 同一份 workflow 可在 soft/hard 两模式运行，不需复制两套脚本。
  - 行为与策略文档一致。

### S19-3 回归测试与负例验证

- 任务：
  - 至少覆盖：PASS / WARN / FAIL / N/A 在 soft/hard 下的行为矩阵。
  - 验证 warning 文案与 exit code 是否符合预期。
- DoD：
  - 有可复现命令或脚本化验证记录。
  - 关键分支行为有证据（日志或测试输出）。

### S19-4 文档同步

- 任务：
  - 更新 README / DEVELOPMENT / notes 中与 gate 运行方式相关段落。
- DoD：
  - 用户能从 README 找到当前默认策略与切换方式。

## 6) S20 - 状态文档时效性收口（主线文档）

### S20 目标

- 让 `PLAN.md` / `NEXT-STEPS.md` 不再“时间明显过旧或阶段状态漂移”。
- 保持“轻量、事实化、可维护”，不做大重写。

### S20-1 漂移审计（只读）

- 任务：
  - 对 `PLAN.md`、`NEXT-STEPS.md` 做“日期/版本/已完成项/进行中项”漂移矩阵。
- DoD：
  - 有一份简表列出“需改/不需改”项与原因。

### S20-2 `PLAN.md` 最小更新

- 任务：
  - 更新文档头部“更新日期/当前阶段”。
  - 修正与当前实现冲突的条目（仅必要最小改动）。
- DoD：
  - 不新增未经实现的能力描述。
  - 文档可回答“当前真实状态是什么”。

### S20-3 `NEXT-STEPS.md` 最小更新

- 任务：
  - 清理已完成但仍列为“待做”的项。
  - 把后续优先级聚焦到少量可执行任务（避免过长愿望清单）。
- DoD：
  - 下一步清单短而可执行（建议 3-5 项）。

### S20-4 一致性校验

- 任务：
  - 交叉核对 `README.md`、`PLAN.md`、`NEXT-STEPS.md`、`sessions/` 是否互相矛盾。
- DoD：
  - 无明显“README 说 A，PLAN 说 B”的冲突。

## 7) 每步提审格式（协作模式复用）

每个 step 提交 Review Package 时建议包含：

- Step 编号：
- 改动文件列表：
- 关键 diff 摘要：
- 自测命令：
- 自测结果：
- 已知风险 / 待确认点：

Review Gate 结论仅两种：`Approved` / `Changes Requested`。

## 8) 风险与回滚点（总览）

### 风险

- CI 依赖来源变化导致偶发失败（S18 核心风险）
- gate 策略切换引发误阻断（S19 核心风险）
- 文档收口改动过大导致信息噪音（S20 核心风险）

### 回滚点

- 回滚点 1：仅回退 workflow 相关改动（不动脚本和 Rust 代码）
- 回滚点 2：仅回退 gate mode 切换逻辑，保留策略文档
- 回滚点 3：仅回退 `PLAN/NEXT-STEPS` 文本，保留其他工程改动

## 9) 建议验证命令（执行期间反复使用）

```bash
git status --short
git rev-parse --short HEAD && git rev-parse --short origin/main

make lint-scripts
bash scripts/tests/test_trace_soft_gate.sh
cargo check --bin trace_eval

rg -n "shellcheck|lint-scripts|gate|soft|hard|GATE_EXIT" \
  .github/workflows/trace-eval-compare.yml README.md DEVELOPMENT.md notes/agent-eval/README.md
```

## 10) 计划完成判定（Exit Criteria）

满足以下条件即视为 S18~S20 完成：

1. CI 脚本 lint 依赖稳定且可观测（S18）。
2. Gate 策略有明确模式、切换机制与回滚条件（S19）。
3. 主线状态文档与当前实现一致，无明显过期冲突（S20）。
4. 各 step 均通过 Review Gate（`Approved`）。
