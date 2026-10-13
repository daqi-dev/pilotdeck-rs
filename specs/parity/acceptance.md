# 验收口径草稿（待批）

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

## 原始证据与复算

原始目录：`/Users/da/ws/pilotdeck-rs/_tmp/20261014-ts5/r1` … `r5`；完成标记为同目录 `progress.txt` 的 DONE。五遍沿用相同运行输出目录，模型 prompt 路径一致，本批没有路径对齐。单份 trace、summary、上游代码与本仓代码的 sha256 记在本 worktree `_tmp/self-compare.json`。

复算命令：`python3 scripts/parity/self_compare.py`。结果为 `_tmp/self-compare.json` 和 `_tmp/self-compare.md`；后者列出全部场景在三种口径下的状态。对照与边界见 [exp004](../../docs/experiments/exp004-ruler-recheck/readme.md) 和 [normalization](normalization.md)。独立评审仍待协调者填写；四项闸门未齐之前不宣称尺子已验收。
