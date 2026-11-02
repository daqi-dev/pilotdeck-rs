# #1327 channel core 负控

`negative_controls.py` 使用 M6 `negative_suite.matrix_main`，共享件只读。25 个变异绑定完整 `channel::tests::<test>` 与 nonce 断言：原 14 项，加 option-field-order、error-after-loop、replace-snapshot-gap、close-skips-stop-prefix、late-stop-error-swallowed，以及 lifecycle/loader 各自的 empty-cases、missing-case、wrong-declaration。

每个选择项依次正控、应用变异、no-run 编译、具名断言与本次 nonce 回执、恢复正控。共享执行器输出 `.patch`、命令/输出和逐项结果，全部变异后汇总；编译错误/错误 panic/缺回执均 INVALID，不会当 KILLED。共享执行器统一 `--offline`、pending 登记和进程/源码保护。禁止 quiet 测试。

```sh
export CARGO_TARGET_DIR=/absolute/path/to/owned/cargo-target
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
python3 src/pilotdeck-gateway/tests/fixtures/channel-core/negative_controls.py --check
python3 src/pilotdeck-gateway/tests/fixtures/channel-core/negative_controls.py --compile-only /new/compile-output
python3 src/pilotdeck-gateway/tests/fixtures/channel-core/negative_controls.py /new/negative-output
```

`--check-anchors` 是 `--check` 的兼容别名；输出目录后加变异名可选单个样本。调用方必须传入 CARGO_TARGET_DIR。macOS 大测试库若有 compact-unwind 大小警告，沿已验证配置加 `CARGO_PROFILE_TEST_OPT_LEVEL=1 CARGO_INCREMENTAL=0`，不禁用 unwind。

源码或测试与 HEAD 不同则守卫拒绝，修复 worker 不绕过；需协调者提交补丁后运行全量。fix-r1 的样本原始拒绝记录和未完成列表见本轮 RESULT。
