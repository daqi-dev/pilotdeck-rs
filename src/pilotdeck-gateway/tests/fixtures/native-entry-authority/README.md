# #1261 原生 run() 入口调用点负控

`src/pilotdeck-gateway/src/native_process.rs` 的 `run()` 在捕获继承环境后调用
`validate_environment(&inherited)?`。#1231 的三个 `production-*-authority-accepted`
负控只变异函数体 / 键名；`production_entry_snapshot_capabilities_are_disabled`
自己直接调用 `validate_environment` 镜像组合，都不经过 `run()` 的调用点。删除该调用后，
生产进程会接受继承的 `PILOTDECK_STORAGE_ROOT` / `PILOTDECK_SNAPSHOT_ROOT` /
`PILOTDECK_MANAGED_CONFIG`，此前的测试与负控均观察不到。

绑定测试是集成测试 `native_entry_process_rejects_inherited_authority`
（`src/pilotdeck-gateway/tests/native_process.rs`）：用 `CARGO_BIN_EXE_pilotdeck-gateway`
起真实 `--native` 子进程（完整合法 CLI、`env_clear()` 后注入被测键），断言子进程非零退出且
stderr 含 `native isolated mode does not accept inherited <key>`。子进程由 5 秒截止期 +
`kill_on_drop` 界定：被接受的继承权限若成功启动并常驻，测试在截止期断言处失败，不会挂死。
测试路径上所有失败点均带 `#1261 native entry ...` 专属消息，变异体绑定该消息集合。

cargo 目标为 `--test native_process`（非 `--lib`）：`env!("CARGO_BIN_EXE_...")` 只在集成
测试编译期可用，且 `--test native_process` 会连带重建 gateway 二进制，变异体才被编进
真实子进程。已登记差异 `specs/gateway-parity/diffs/A8-NATIVE-SNAPSHOT-CAPABILITY-001.md`
记录了 TS 接受纯空白 storage 而 native 入口拒绝的隔离边界；本负控只锚定 Rust 侧调用点，
不声称两者一致。

## 运行

`negative_controls.py --check` 仅检查锚点。实际负控命令为
`negative_controls.py NEW_OUTPUT [mutation ...]`，必须由调用方设置 `CARGO_TARGET_DIR`，
使用共享 HEAD 守卫、进程所有权和最终汇总：每项先正控，再 `--no-run` 确认变异体可编译，
再判杀，恢复后复跑；过期锚点与无法编译都是 INVALID，任一非 KILLED 即非零退出。未提交
源码不能运行（`mutating_sources` 拒绝），不得使用副本仓库绕过；由协调者在提交后的宿主
树上运行。
