# 决策卡 · #1199 纯文档改动跳过重 CI job（2026-10-31）

- **卡在哪**：只改 `state/` 的 state PR（如 #1197）也要跑两遍完整 CI（PR 侧 24 min + merge_group 侧 16 min），还占合并队列位；重 job（rust / a51-coverage / metadata-observer）对纯文档改动没有增量价值，但 #904/#918 的守卫测试按「必需 job 无条件执行、skipped 一律拒绝」钉死了 ci.yml。
- **人批**（2026-10-31）：「同意改 ci.yml，最好立即改」。这是合并闸门语义变更，由人批而非 AI 代批。
- **放宽边界（唯一放行路径）**：新增无条件 job `scope` 先跑 `scripts/ci_scope.py` 的自测（`scripts/test-ci-scope.py` 正负例），再按事件算改动文件：pull_request 用 `base.sha..head.sha`、merge_group 用 `base_sha..head_sha`、push 到 main 恒为 full。分类器**确定**改动只碰 `state/`、`.42cog/`、`docs/` 三个前缀时输出 `docs_only=true`；任一路径（含 rename / delete 两侧，用 `--no-renames`）不在白名单、diff 为空、git 出错、事件缺 SHA、分类器异常，一律输出 false（fail-closed，宁可全量）。
- **闸门新语义**：`check` 的 `needs` 加入 `scope`；scope 非 success 一律拒绝（含取消 / 跳过）；三个重 job 各自的拒绝条件放宽为「result != success 且**不是**（result == skipped 且 docs_only == true）」——即只有这一种 skipped 被接受，取消和其他跳过照旧拒绝（#904 的放宽）。三个重 job `needs: [scope]` + 精确 `if: needs.scope.outputs.docs_only != 'true'`，不加其他条件（#918「步骤 / job 无条件」在这三个 job 上放宽为「只经 scope 门控」）；守卫测试同步改为精确字符串断言 + 四类变异负控（去 docs_only 条件、白名单加 src / .github、scope 失败仍通过、重 job 加 continue-on-error）。`gateway-schema-baseline` 不变照常跑。
- **能否撤回**：能。删 `scope` job、还原 `check` 的三个旧条件与三个重 job 的 needs/if、守卫测试回退即可，无数据迁移。
- **动不动尺子 / 意向**：动。#904「check 只认 success、skipped 一律拒绝」与 #918「必需 job 无条件执行」被有意放宽，但只放开上面一条精确路径；「job 不许 continue-on-error」不放宽。
- **待观察**：① 探针 PR（只改 `state/`）实测 rust / a51-coverage / metadata-observer 被跳过且 `check` 在 PR 与 merge_group 两侧都通过；② GitHub 对 merge_group 上 skipped 必需检查（本仓必需检查只有 check / gateway-schema-baseline，均无条件跑，理论上不受影响）与 job outputs 在 skipped 前发布的语义，以探针 PR 实测为准。
