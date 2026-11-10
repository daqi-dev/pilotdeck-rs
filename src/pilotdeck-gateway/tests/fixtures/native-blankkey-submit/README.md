# #2692 submit 侧空键拼法负控

S1 探针（`_tmp/20261110-issue-2692/probes/ts-deepkey-probe.mjs`）证明 TS 的
`sessionKey:null` 与 `sessionKey:""` 回合能越过完整 submit 链存活到存储/读取
路径：null 记录体落盘为 JSON null、复合键拼 `"null"`、router Map 键分离、同
runId 交叉拼法各成第二条记录。修复让 Rust submit 链保留折叠前拼法（null →
`"null"`、缺失 → `"undefined"`），记录体经 `RunAcceptInput.session_key_raw` 保
持原始拼写；`session_model_set` 与 `set_permission_mode` 的 busy 门按拼法查
admission。

绑定测试（均经真实 `TurnSubmitter` 诞生回合，`HangingRefreshServices` 把回合停
在 pump 的 config refresh——TS 探针里永不 yield 的 `submit` 生成器的移植）：

- `native_process::methods::config::tests::session_model_set_null_key_turn_from_the_submit_chain_answers_the_ts_cells`
  —— null 键回合 + 四格方法答案（E/F + permission 孪生）+ 记录体 JSON null；
- `native_process::methods::config::tests::session_model_set_blank_key_turn_from_the_submit_chain_answers_the_ts_cells`
  —— `""` 键回合 + B/D 格 + 交叉拼法第二记录/第二在飞回合；
- `storage::run_registry_tests::null_session_key_body_keeps_json_null_while_the_key_spells_null`
  —— 记录体/键/磁盘往返；
- `js_value::tests::template_key_spells_the_template_literal_form` ——
  `template_key` 拼法表。

七个负控依次编译、跑绑定测试、恢复正控并汇总，任何非 KILLED 非零退出：
submit 回退折叠（两格：null 格与交叉拼法格）、记录体回退拼法串、raw 字段喂拼
法、两个 busy 门回退 as_string、`template_key` 本身回退折叠。锚点必须唯一；
变异通过 `mutating_sources` 持锁（要求源码等于 HEAD），cargo 通过 `run_owned`
和 `wait_for_slot`，必须使用调用方 CARGO_TARGET_DIR。未提交的新金样会被 HEAD
守卫拒绝，此时交协调者提交后执行，不绕过守卫。

```sh
export CARGO_TARGET_DIR=/abs/owned/target
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
python3 -I src/pilotdeck-gateway/tests/fixtures/native-blankkey-submit/negative_controls.py --check-anchors
# 提交后且工作树源码等于 HEAD：
python3 -I src/pilotdeck-gateway/tests/fixtures/native-blankkey-submit/negative_controls.py /abs/fresh/evidence
```
