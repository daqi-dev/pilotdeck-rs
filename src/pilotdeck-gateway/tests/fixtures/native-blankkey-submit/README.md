# #2692 submit 侧空键折叠坍缩负控

S1 探针（`ts-deepkey-probe.out`）证明 TS 的 `sessionKey:null` 与 `sessionKey:""`
回合能越过完整 submit 链存活到存储/读取路径；fix-r1 探针
（`ts-prodkey-probe.out`）进一步把两个键域分开：复合 run 键拼
`String(sessionKey)`（null → `"null"`、缺失 → `"undefined"`、`template_key_checked`
把不可拼的深数组当 Err 回答），router 形 Map（admission / sessions / fence /
coordinator / busy 门预检）按原始值本身索引（`router_key`：字符串即自身，其余类别
打 `\0\0raw:…` 标签，null 与 `"null"` 是两个会话、一条记录拼法）。记录体经
`RunAcceptInput.session_key_raw` 保持原始值；读取面把请求自己的 `sessionKey` 回显进
snapshot（miss 也回显）。

## 变异名册（15 行，逐行绑死测试）

- `submit-owner-uses-template-key` —— owner 各 Map 回退模板拼法，null 回合对
  router 域查找不可见（admit 截止期先死）；
- `router-key-folds-null` —— Null 臂回退字符串拼法，null 与 `"null"` 重新折叠成
  一个 router 键；
- `template-key-checked-folds` —— 复合键助手把拼法 Err 折回 `""`（修复前的
  unchecked 折叠；fix-r5 起函数体是 `property_key` 的显式转调，锚点随迁）；
- `acceptance-error-swallowed` —— accept-catch 形状被丢掉：ToString 抛错的键被
  静默接受，无 result_unknown 事件；
- `owner-conjunction-only` —— busy 门只剩 #1872 owner 合取，请求键查不到自己的
  在飞回合（Grok#2 控制格）；
- `model-set-gate-string-only` / `permission-gate-string-only` —— 两个 busy 门回退
  as_string 字符串键，null 请求永远见不到自己的回合（F 格）；
- `registry-body-spelling` / `submit-raw-spelling` —— 记录体丢掉原始 JSON null
  （round-1 行，重锚后幸存）；
- `registry-skip-reverted` —— 折叠带跳过被整个去掉，深键记录越过带也跳过，load
  不再像 TS 那样失败（fix-r5 起带判据按 `array_nesting` 数组嵌套层数计，锚点
  随迁）；
- `registry-skip-silent`（fix-r5，评审 boundary#3 P0）—— 生产信号面（累计计数器
  增量 + 每读 stderr 行）被整体去掉，带内跳过回到只有 `#[cfg(test)]` 访问器可见
  的静默形状；
- `registry-band-leaf-depth`（fix-r6，评审 sibling#2）—— 带判据变异回旧的最深
  叶/节点深口径（container_depth），空内叶形状恰少计 1，带外第一层空内叶记录
  （2561）被跳过而不是 load 失败；旧口径代码下该变异是恒等（SURVIVED），杀灭即
  证明 `array_nesting` 口径咬得住；
- `echo-drops-raw-key` —— 读取面回显固定 null 而不是请求自己的值；
- `echo-capped-serializer`（fix-r6，评审 sibling#1）—— 带上限 provider-body
  序列化器的深度界抬高越过回显深度，两口径分工坍缩，深键回显测试的「有上限
  序列化器拒绝同一值」腿先死；
- `session-id-uses-router-key`（fix-r3 条1）—— SessionContext 交接把路径/持久身份域
  折叠回 router 键，非字符串 sessionKey 回合死在生产工厂的 `create_storage`
  （`std::fs` 拒绝 raw 值标签里的 NUL 字节，到不了模型解析），null.jsonl 不落盘。

绑定测试：`config::tests::session_model_set_null_key_turn_from_the_submit_chain_answers_the_ts_cells`
（键域 + 四格 + 记录体）、`config::tests::deep_array_session_key_submit_is_rejected_without_landing_a_record`
（accept-catch）、`config::tests::session_model_set_null_request_never_answers_for_a_busy_owner_session`
（Grok#2）、`js_value::tests::template_key_spells_the_template_literal_form`（拼法表）、
`storage::run_registry_tests::null_session_key_body_keeps_json_null_while_the_key_spells_null`
（记录体）、`storage::run_registry_tests::deep_session_key_record_reopens_without_bricking_the_other_records`
（折叠带）、`storage::run_registry_tests::unrekeyable_skips_advances_in_production_on_every_read`
（生产信号面，fix-r5）、`runtime::turn::read::tests::read_session_messages_reads_the_router_domain_and_echoes_the_raw_key`
（回显）、`runtime::turn::read::tests::deep_session_key_echo_survives_the_response_serializer`
（深键回显两口径分工，fix-r6 评审 sibling#1）、`native_process::methods::config::tests::real_services_null_session_key_turn_dies_at_model_resolution`
（SessionContext 双域键，fix-r3 条1）。

扫描面（fix-r5，评审 P3#5）：SOURCE_TEXTS 除五个被变异源码（submit / config /
run_registry / js_value / read）外，纳入断言所在的两个测试模块
（`run_registry_tests.rs`、`read_tests.rs`）——先前只住在测试文件里的断言不在
字面锚覆盖内，纳入后按「断言逐字出现于被扫源 → 必须有 pin」规则补齐（14 个
字面锚；fix-r6 增 read_tests 的两口径 pin）。`--check` 除锚点/名册外还核每行
绑定测试名仍在声明它的那个文件里（`negative_suite.bind_module_file` +
`declares_test`，fix-r6 评审 boundary#5）。这条门禁的红侧由同目录
`test_bound_test_gate.py` 钉住（#2692 fix-r9，评审 sibling#3：正控跑真 `--check`
核 `bound tests declared` 计数、红侧模拟绑定测试改名要求 rc 2 拒跑），该测试经
`tests/negative_anchor_selfcheck.rs` 进 cargo CI。字面锚守卫
（`check_literal_anchors`）自己的红侧由同目录 `test_literal_anchors.py` 钉住
（#2692 fix-r10，评审 sibling#1：正控净表通过，负控各破坏一个不变式——pin
不在源 / pin 出 SOURCE_TEXTS / pin 无行绑定 / 断言无 pin / 行出 SOURCE_TEXTS /
子集仍按全表绑定——要求 rc 2 与对应拒绝语），同样经
`tests/negative_anchor_selfcheck.rs` 进 cargo CI；该 .rs 清单另有发现式 meta
测试：每个 `fixtures/*/test_bound_test_gate.py` 与 `fixtures/*/
test_literal_anchors.py` 红侧 twin 必须在清单里，忘登记即红（fix-r10，评审
simplify#5）。

## 启动契约（#2391 迁移）

`negative_selection` 契约：`registered()` 名册等值核验（漂移 = 启动拒绝）、
`refuse()` 形拒跑（rc 2、stderr `INVALID:`）、`missing_target()` +
`refuse_unusable_target()` 在占用证据目录之前、`claim_dir` 原子占用（已存在目录
rc 2）、`verdict_exit` 共享裁决（永不自算）。`--check` 的 argv 契约按 #2495 fix-r2
机器核验：多余参数 / `--compile-only` 混入都拒绝。

每行变异依次正控 → 应用 → `cargo test --no-run` 编译核 → 负控 → 恢复 + 字节核对
→ 恢复后正控；`--compile-only` 只跑到编译核（COMPILED 即过）。变异通过
`mutating_sources` 持锁（要求参与源码等于 HEAD 且 index 干净），cargo 经
`run_owned` 与 `wait_for_slot`，必须使用调用方 CARGO_TARGET_DIR。未提交的改动
会被 HEAD 守卫拒绝，此时交协调者提交后执行，不绕过守卫。

```sh
export CARGO_TARGET_DIR=/abs/owned/target
python3 -I src/pilotdeck-gateway/tests/fixtures/native-blankkey-submit/negative_controls.py --check
# 提交后且工作树源码等于 HEAD：
python3 -I src/pilotdeck-gateway/tests/fixtures/native-blankkey-submit/negative_controls.py /abs/fresh/evidence
# 或只做编译核：
python3 -I src/pilotdeck-gateway/tests/fixtures/native-blankkey-submit/negative_controls.py --compile-only /abs/fresh/evidence
```
