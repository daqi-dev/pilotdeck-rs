# exp005 · 阶段 C 第一个可信分数：Rust sidecar 对 TS sidecar

> 2026-10-14 03:55。尺子：main 上冻结的 `scripts/parity/`（`specs/parity/FROZEN.md`）。Rust 二进制 sha256 `8bc4889c56cf6e90…`（PR #12 编出，复制到 `_tmp/rust-sidecar-8bc4889c` 钉住）。原始数据：`_tmp/ruler-snap/_tmp/20261014-parity/{ts,rust}-r{1..5}/`（不入库）。

## 假设

搬入的旧 Rust sidecar（旧执行在 `b243fa0` 上按 G03 v1 得过 51–53/53）在 `cc7c4e6` 上、按冻结口径，仍接近全过。

## 做法

1. **两侧用同一个输出路径跑**（`round.py --label cur`，跑完改名），TS 与 Rust 交替各 5 遍——这是第二次尝试；第一次两侧路径不同，system prompt 里嵌的工作目录差 34 字符，token 预算数字全不同，得出 4/53 的**假象**（`state/morning-report-20261014.md` 方法缺口 ⑳）。
2. 健全性：新一批 TS 两两 10 对。
3. 判分：第 i 遍 Rust 对第 i 遍 TS，5 对全 PASS 才算一致（`specs/parity/acceptance.md`）；另算 25 对矩阵。

## 结论

| 项 | 结果 |
|---|---|
| 健全性：新一批 TS 自比 10 对全一致 | **53/53** |
| **阶段 C 分数**（5 对全 PASS） | **50 / 53** |
| 25 对矩阵全 PASS | 50 / 53（与配对口径一致） |
| Rust 身份 | 10 遍全部 `identity_ok`（被启动命令 = 指定二进制、sha256 匹配） |

### 未过的 3 个（5 遍全 FAIL，稳定差异，不是噪声）

| 场景 | 差异 | 初步分诊 |
|---|---|---|
| `deadline` | Rust 侧多一条宿主写的 `durable.status`：`gateway_submit_failed` /「AgentLoop terminal outcome is result_unknown and host reconciliation found no final result.」（触发 E10、E11） | **Rust 产品**：Rust 在 deadline 时上报的终态与 TS 不同，引发宿主对账失败。旧执行见过同一条（当时时有时无，判为宿主竞态）；这次稳定出现 |
| `deadline_during_tool` | 同上 | 同上 |
| `sidecar_budget_limit` | 终态 `text`：TS `max_budget_reached`，Rust `Reached Gateway-owned maxBudgetUsd ($0.500000) after spending $1.000000.` | **Rust 产品**：Rust 给的是人读消息，TS 给的是原因码 |

## 改变了哪个取舍

- 阶段 C 的修复范围收窄到 **3 个场景、2 类问题**（deadline 终态、budget 终态文本），开 Issue #4 处理。
- 「两侧同路径」写进了冻结的 `normalization.md`，此后所有对拍照此跑。

## 结论的边界

- 只有一组 5 + 5 遍；7 个在 exp001 里「时有时无」的场景，这一批全部稳定通过，但样本仍少。
- 只测了 agent 循环这一层（上游装置的 53 个场景，mock 模型与工具）。
