# S18-1 Review Package: CI Shellcheck Strategy（2026-05-08）

- Step 编号：`S18-1`
- 范围：`.github/workflows/trace-eval-compare.yml`（仅审计与策略选型）

## 1) 现状审计

当前 workflow 在 `Lint shell scripts` 步骤直接执行 `make lint-scripts`，但没有以下保障：

1. 未显式安装 `shellcheck`（依赖 runner 预装状态）。
2. 未输出 `shellcheck --version`（版本不可观测）。
3. `ubuntu-latest` 切换镜像时，可能出现缺失或版本漂移导致的非业务失败。

## 2) 安装策略二选一评估

### A. 显式安装（apt/action）

- 优点：
  - 依赖来源清晰，避免“恰好预装”带来的不确定性。
  - 缺失场景可被安装步骤兜底。
  - 配合版本输出，排障更直接。
- 成本：
  - 增加少量 CI 时间（安装步骤）。

### B. 仅校验预装并在缺失时报错

- 优点：
  - CI 速度更快（无安装步骤）。
  - workflow 改动最小。
- 风险：
  - 仍依赖 runner 镜像变化，版本漂移不可控。
  - 缺失时直接失败，恢复路径需要再改 workflow。

## 3) 结论（本次选型）

选 **A：显式安装 + 版本可观测**。  
理由：在稳定性、可维护性、排障效率三项上更均衡，符合 S18 “确定性加固”目标。

## 4) 后续落地（S18-2）

1. 在 workflow 增加 `Install shellcheck` 步骤。
2. 在 `make lint-scripts` 之前输出 `command -v shellcheck` 与 `shellcheck --version`。
3. 保持 lint 入口仍为 `make lint-scripts`，不分叉执行路径。
