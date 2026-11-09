---
id: D2-NATIVE-MULTIPROJECT-SESSION-ISOLATION-001-R1
original: D2-NATIVE-MULTIPROJECT-SESSION-ISOLATION-001
---

## 类别

补充可复算判据，明确publisher隔离的实际边界；不改原条目，不增加判分豁免。

## 基线依据

沿用原条目固定TS真实A/B同key复用的原件及SHA；现场原件保全见主仓 `_tmp/20261108-cli-completion/multi-project-unregistered/` 和原条目所引capture-switch。不把隔离网络中的provider失败称成功。

## Rust 行为

原条目的“绑定publisher”准确指 `RunRef.project_key.is_some()`，包括携带project key的通用调用者；native通过host绑定保证始终为Some。旧publisher的record/emit/retain均验证registration实例，阻止相同runId复用后迟到事件污染后继流。project=None的通用API保持原合同，未由本次修复覆盖；不能泛称所有通用publisher均不变。

## 范围

会话归属、选定项目的活动/保留replay与持久run读取、Some(project) publisher代际隔离。词法身份折叠`.`/`..`，不将符号链接或大小写等价关系推定为同一项目。stage副作用与CLI安装由各自测试覆盖。

## 机器判据

以下命令仅在mbp2源码绑定镜像执行，CARGO_TARGET_DIR必须指向该镜像的 `_build/cargo-target`。测试名均来自产品测试，不能把0 tests当通过：

```sh
cargo test -p pilotdeck-gateway --lib native_project_binding_rejects_pending_and_warm_cross_project_reuse --locked
cargo test -p pilotdeck-gateway --lib native_project_request_scope_pins_absent_owner_until_handler_finishes --locked
cargo test -p pilotdeck-gateway --lib native_owner_close_keeps_retained_replay_scoped_to_original_project --locked
cargo test -p pilotdeck-gateway --lib native_project_binding_rejects_unbound_creation_before_and_during_scope --locked
cargo test -p pilotdeck-gateway --lib late_old_publisher_cannot_corrupt_reused_run_or_detach_successor --locked
cargo test -p pilotdeck-gateway --lib scoped_submit_omitted_and_aliased_project_keep_canonical_active_replay --locked
cargo test -p pilotdeck-gateway --lib native_process::methods::runs::tests:: --locked
```

会话测试在 `src/pilotdeck-gateway/src/runtime/session/owner_tests.rs`；publisher测试在 `runtime/turn/publish_tests.rs`；submit在 `runtime/turn/submit_tests.rs`。实际负控命令：

```sh
python3 src/pilotdeck-gateway/tests/fixtures/session-owner/negative.py "$EVIDENCE/owner"
python3 src/pilotdeck-gateway/tests/fixtures/turn-publish/negative.py "$EVIDENCE/publish"
```

前者包含 `foreign-retained-snapshot-exposed`、`unbound-owner-creation-accepted`；后者包含 `stale-publisher-emit`、`stale-publisher-record`、`stale-publisher-retain`。每条须基线真实通过、变异编译成功且指定测试真实断言失败、恢复原字节；静态anchor成功、编译失败、超时都不算KILLED。实际执行与SHA绑定在主仓 `_tmp/20261108-cli-completion/negative-generation-r1/`，本文写入时仍在执行，未冒称通过。真实同host A/B/重启验证另用 `multi-project-e2e` 候选，最终二进制实跑前不计通过。

## 批准记录

AI代登记，待人复核；未批准，非已接受差异，无冻结判分豁免。范围澄清对应独审 review-isolation-repair 的DeepSeek sibling判据P1和GLM concurrency边界P2；实效仍待最终实跑与人签。
