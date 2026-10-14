# 验收口径（2026-10-14 冻结 · AI 代批，待人复核）

依据冻结的 [README](README.md) 第三节，k = 5，定 S 只使用 TS sidecar 原料。基线为 PilotDeck@cc7c4e6；本 issue 没有运行 Rust。本文尚未冻结，也未批准任何排除。

## 默认口径与稳定集合

五遍两两比较共 10 对；一个场景在全部 10 对的 G03 v1 总判决均为 PASS 才进入 S。总判决包含通道比较、适用 E 规则与原版 oracle/生产证据/trace 健康检查。默认 |S| = **53**。

| 口径 | 配对数 | S 大小 |
|---|---:|---:|
| 默认：10 对两两全一致 | 10 | 53 |
| r1 为基准的四对 | 4 | 53 |
| 相邻四对 | 4 | 53 |

| S 的成员 |
|---|
| `allowed_read_files` |
| `auto_compact` |
| `can_prompt_false` |
| `cancel` |
| `cancel_during_tool` |
| `checkpoint_resume` |
| `deadline` |
| `deadline_during_tool` |
| `denied_read_files` |
| `image` |
| `large_tool_result` |
| `malformed_model_response` |
| `max_turns` |
| `model_non_retryable_error` |
| `model_retryable_error` |
| `multi_tool_mixed_error` |
| `multi_tool_mixed_permission` |
| `multi_tool_ordered` |
| `multimodal_image_and_text` |
| `multiple_tool` |
| `permission_allow` |
| `permission_ask_approve` |
| `permission_ask_deny` |
| `permission_denial` |
| `plan_mode_bypass_host_policy` |
| `plan_mode_host_policy` |
| `pure_text` |
| `sidecar_additional_working_directories` |
| `sidecar_budget_limit` |
| `sidecar_continuable_followup_cold` |
| `sidecar_continuable_followup_live` |
| `sidecar_durable_compaction` |
| `sidecar_elicitation` |
| `sidecar_elicitation_execution` |
| `sidecar_empty_system_prompt` |
| `sidecar_full_request_compaction_budget` |
| `sidecar_live_model_stream` |
| `sidecar_live_steer` |
| `sidecar_model_metadata` |
| `sidecar_one_shot_parent_abort_after_admission` |
| `sidecar_one_shot_parent_close_after_admission` |
| `sidecar_one_shot_subagent_failure` |
| `sidecar_one_shot_subagent_success` |
| `sidecar_parent_close_after_admission` |
| `sidecar_projected_request_compaction_budget` |
| `sidecar_sdk_tool_progress` |
| `sidecar_seed_read_state` |
| `single_tool` |
| `stream_interruption` |
| `tool_error` |
| `tool_non_retryable_error` |
| `tool_retryable_error` |
| `write_snapshot_resume` |

## S 以外的场景

每个条目同时追加到 [accepted-diffs](accepted-diffs.md)，类别「原版不稳定」、状态「待批」。排除须逐个批准；待批不能当成免检豁免。

| 场景 | 排除状态 |
|---|---|
| 无 | — |

## 使用条件

|S| ≥ 45 的下限已满足。后续 Rust 对 S 中场景须按冻结 README 连跑 k = 5 遍全一致，即 m = k = 5；本 issue 没有依据 Rust 的运行结果调整 k、m 或 S。正式排除批准后的分母为 |S|；待批草稿本身不改变正式分母。全轮比较器仍检查 53 个场景齐全，不以删掉 S 外文件代替单列报告。

BLOCKED 是环境失败，整轮作废，不归入原版不稳定；本批五轮 blocked 与 oracleFailures 均为空。上游 run.py 的非零退出是 native 对 sidecar 的严格差异，不是这次 sidecar 自比的判决。

| 配对 | 一致场景数 |
|---|---:|
| r1-r2 | 53 / 53 |
| r1-r3 | 53 / 53 |
| r1-r4 | 53 / 53 |
| r1-r5 | 53 / 53 |
| r2-r3 | 53 / 53 |
| r2-r4 | 53 / 53 |
| r2-r5 | 53 / 53 |
| r3-r4 | 53 / 53 |
| r3-r5 | 53 / 53 |
| r4-r5 | 53 / 53 |

## Rust 怎么判（AI 代拍，待人复核）

- **配对**：第 i 遍 Rust 对第 i 遍 TS（i = 1…5），同一场景 **5 对全 PASS** 才算一致；另报 5 × 5 = 25 对矩阵作参考，不作判决。
- **前提**：每一对的两侧都必须满足 `normalization.md`「两侧输出路径必须一字不差」，否则该对作废重跑。
- **分母**：53 − |原版不稳定|；本批 TS 校准 S = 53、无排除，故分母 = **53**。
- **身份**：Rust 那一侧每一遍都必须 `identity_ok`（被启动命令 = 指定二进制、sha256 匹配）。

## 批准记录

2026-10-14 夜，人授权 AI 代做本次重构的全部判断（`CLAUDE.md` §五「授权扩大」）。本文件与 `normalization.md` 由协调者 **AI 代批**：机器检查全过（controls、self_compare、review_r1 9/9）、|S| = 53 ≥ 45、PR #13 两家两轮评审收敛到无 P0/P1/P2（`docs/reviews/001_ruler-recheck.md`）。**待人复核**；推翻时改本文件并重算，trace 都在，不用重跑。

## 原始证据与复算

原始目录：`/Users/da/ws/pilotdeck-rs/_tmp/20261014-ts5/r1` … `r5`；完成标记为同目录 `progress.txt` 的 DONE。五遍沿用相同运行输出目录，模型 prompt 路径一致，本批没有路径对齐。单份 trace、summary、上游代码与本仓代码的 sha256 记在本 worktree `_tmp/self-compare.json`。

复算命令：`python3 scripts/parity/self_compare.py`。结果为 `_tmp/self-compare.json` 和 `_tmp/self-compare.md`；后者列出全部场景在三种口径下的状态。对照与边界见 [exp004](../../docs/experiments/exp004-ruler-recheck/readme.md) 和 [normalization](normalization.md)。独立评审仍待协调者填写；四项闸门未齐之前不宣称尺子已验收。

## 原版多态场景

Issue #35 的方案 B 已由人批准：仅对 [multimodal.md](multimodal.md) 登记的原版多态场景，改用「每一遍 Rust 都必须与 TS 参考池中至少一遍的冻结 G03 总判决为 PASS」，五遍全满足才算场景 PASS。参考池至少 20 遍，类别只由 TS 两两 G03 判决确定，每类至少两遍；单例类保守拒绝计分。具体登记状态仍为「待批」，由协调者评审后冻结。

离线计分入口为 [scripts/parity/score.py](../../scripts/parity/score.py)，参数、JSON 证据和拒绝条件见登记文档。普通场景仍执行本文件原有的第 i 对第 i 判法；同路径、身份、oracle、E 规则、健康检查与 BLOCKED 整轮作废条件继续有效。多态场景仍进入分子与分母；分母保持 53 − 已批准的原版不稳定场景数，目前无此类排除，故为 53。上述例外覆盖原文中登记场景的逐遍配对要求，不修改原文或 G03。

## 日期上下文一致前提（Issue #35 第 2 步）

参考池必须与被测 Rust 轮次在**同一 UTC 日采集**。一轮回归的两侧各五遍不得跨 UTC 午夜（UTC+8 本地 **08:00**）；跨日的轮次**整轮作废重跑**，不得以部分遍数或 prompt 日期归一化补救。TS 参考池本身也不得跨 UTC 日。此项与 `normalization.md`「两侧输出路径必须一字不差」一样，是计分前提。

[score.py](../../scripts/parity/score.py) 在场景比较前提取每份 trace 的 systemPrompt `now: YYYY-MM-DD` 日期；缺失时记录 null 和说明。普通场景同序号 TS/Rust 日期不同，或多态场景 Rust 遍与参考池日期不同，均判该遍及该场景 **REFUSED**，不能计作 PASS 或 FAIL。一侧日期为 null、另一侧有日期同样拒绝；双方均无日期时保留原 G03 检查，并如实注明无法从该 trace 推断采集日。多个 UTC 日期出现在一个 trace 或同一侧的五遍中，整轮拒绝并重跑；多日期参考池拒绝定标。

一轮只要存在任何 REFUSED，就输出 `valid_score=false`、`verdict=REFUSED`、`score=null`，退出码 2；分母规则不变，不以拒绝场景缩小分母。日期与拒绝理由保留在逐场景/逐遍 JSON 中。上述前提由采集者保证，缺日期不是跨日采集的豁免；满足前提后，原请求体、prompt、G03、oracle、身份、健康与 E 规则仍完整检查。具体证据与两个日期负例见 [exp009](../../docs/experiments/exp009-multimodal-ruler/readme.md) 与 [controls/multimodal.py](../../scripts/parity/controls/multimodal.py)。
