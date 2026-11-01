# #1261 原生 run() 入口调用点负控

`src/pilotdeck-gateway/src/native_process.rs` 的 `run()` 在捕获继承环境后调用
`validate_environment(&inherited)?`。#1231 的三个 `production-*-authority-accepted`
负控只变异函数体 / 键名；`production_entry_snapshot_capabilities_are_disabled`
自己直接调用 `validate_environment` 镜像组合，都不经过 `run()` 的调用点。删除或后移
该调用后，生产进程会先消费再（或不再）拒绝继承的 `PILOTDECK_STORAGE_ROOT` /
`PILOTDECK_SNAPSHOT_ROOT` / `PILOTDECK_MANAGED_CONFIG`，此前的测试与负控均观察不到。

绑定测试是集成测试 `native_entry_process_rejects_inherited_authority`
（`src/pilotdeck-gateway/tests/native_process.rs`）：用 `CARGO_BIN_EXE_pilotdeck-gateway`
起真实 `--native` 子进程（完整合法 CLI、`env_clear()` 后注入被测键），断言子进程非零
退出且 stderr 含 `native isolated mode does not accept inherited <key>`。子进程由
5 秒截止期 + `kill_on_drop` 界定，且调用点的两种破坏各有专属观察：删除调用时子进程
会带着被接受的权限常驻，只有在 stderr 里观察到子进程自己的 `native listening`
广播后截止期才计杀（主机慢本身不是证据）；后移调用时装配层的项目注册 / 运行清单
会在拒绝前写入 `home`，测试断言 `home` 除自身写入的 config 外无新增条目。

判杀集合（MARKERS）只含上述三个键断言消息：launch / stderr 管道 / exit 等环境性
失败一律落 INVALID。测试自身的失败点均带 `#1261 native entry ...` 专属消息（共享
`Scratch` 构造内部的 expect 除外，它们不在判杀集合内，只落 INVALID）。

cargo 目标为 `--test native_process`（非 `--lib`）：`env!("CARGO_BIN_EXE_...")` 只在集成
测试编译期可用，且 `--test native_process` 会连带重建 gateway 二进制，变异体才被编进
真实子进程。已登记差异 `specs/gateway-parity/diffs/A8-NATIVE-SNAPSHOT-CAPABILITY-001.md`
记录了 TS 接受纯空白 storage 而 native 入口拒绝的隔离边界；第三个键
`PILOTDECK_MANAGED_CONFIG` 是 A8 snapshot 两键范围之外的 Rust 侧 authority。本负控只
锚定 Rust 侧调用点，不声称两者一致。

## 运行

`python3 -I negative_controls.py --check` 检查锚点与标记绑定（不依赖 cargo）。实际
负控命令为 `python3 -I negative_controls.py NEW_OUTPUT [mutation ...]`，必须由调用方
设置 `CARGO_TARGET_DIR`，使用共享 HEAD 守卫、进程所有权和最终汇总：每项先正控，再
`--no-run` 确认变异体可编译，再判杀，恢复后复跑；过期锚点与无法编译都是 INVALID，
任一非 KILLED 即非零退出。未提交源码不能运行（`mutating_sources` 拒跑），不得使用
副本仓库绕过；由协调者在提交后的宿主树上运行。
