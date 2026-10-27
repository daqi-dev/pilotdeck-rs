# #813 turn-submit 真实宿主日志（fix-r13）

宿主在 `claude/issue-813-negctl-scope` 上跑 `turn-submit/negative.py` 的原始
libtest 日志，用于回归「同 case 不同步骤不得互相判杀」：

- `snapshot-pre-skipped.log`：被点名测试
  `runtime::turn::submit::tests::real_ts_submit_scenarios` 自身 `FAILED`，
  断言消息为
  `#813 snapshot-success: step 1 "accept": submit scenario must match`。
- `snapshot-success-post-added.log`：同一被点名测试、同一 case
  `snapshot-success`，但目标断言是 step 3：
  `#813 snapshot-success: step 3 "end": submit scenario must match`。

旧 marker `snapshot-success: step` 是场景前缀，在两份日志的 panic payload 里都
出现，于是 step 1 的前置失败也能把 step 3 的变异记成 KILLED。fix-r13 把每个
变异绑定到完整断言消息后，两份日志互相不判杀。

`test_negative_panic.py::TurnSubmitStepEvidence` 与
`turn-submit/negative.py::anchor_self_test` 都重放这两份日志。

日志只把本机绝对路径替换为 `<repo>`，并剔除本沙箱 `cargo-slots.sh` 的
`在跑 … / 上限 …` 提示行；panic payload 与 `exit=101` 原样保留。
