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
`kill_on_drop` + 60 秒静默上限界定，stderr 按行读、见到目标行立即返回（#1401 评审 P2：
原先 `read_to_end` 撞 5 秒截止期，常驻子进程每次白等满 5 秒，慢主机又会被读成「没有
广播」）。调用点的两种破坏各有专属观察：删除调用时子进程会带着被接受的权限常驻，
stderr 出现子进程自己的 `native listening` 行即判杀（主机慢本身不是证据）；后移调用时
装配层的项目注册 / 运行清单会在拒绝前写入 `home`，测试断言 `home` 除自身写入的 config
外无新增条目。

判杀集合（MARKERS）只含上述三个键断言消息：launch / stderr 管道 / exit-wait 等
环境性失败一律落 INVALID。子进程早退分两支（#1401 项 1）：stderr **既无 rejection
也无 `native listening` 广告**（先于入口门退出，dyld/OOM/装配失败）时，测试用
**不带判杀标记**的 panic 报「子进程提前退出，无法判定」，只落 INVALID——环境性
早退不得记成 KILLED；stderr **已有 `native listening` 广告**再退出时，广播本身已
证明入口门接受了继承权限，测试用带键标记的断言判杀（KILLED）。测试自身的失败点均
带 `#1261 native entry ...` 专属消息（测试自建 scratch 目录，不再依赖共享
`test_scratch::Scratch`，#1401 项 5）。

#1401 项 6 为 `run()` 里 `options.environment(inherited)` 的调用点另绑一个测试
`native_entry_scrubs_inherited_process_environment`：`environment()` 会移除
`PILOTDECK_CONFIG_PATH`（`config::global_config_path` 里该键赢了 `<home>/pilotdeck.yaml`），
测试注入指向无效 YAML 的该键——正控下键被移除、子进程读测试写好的合法 home config
并常驻（见 `native listening` 行即正控结束）；变异 `let env = inherited;`（跳过 scrub）下
键保留、子进程读注入文件快速失败，stderr 必现 `Failed to parse default config YAML.`
（实测：错误转换丢弃携带 path 的 diagnostics，消息文本是唯一可观察证据）。两分支的
失败方向均为 INVALID（早退无该消息 / 静默无广播都只报无标记 panic），不引入
「慢主机 → KILLED」路径。

cargo 目标为 `--test native_process`（非 `--lib`）：`env!("CARGO_BIN_EXE_...")` 只在集成
测试编译期可用，且 `--test native_process` 会连带重建 gateway 二进制，变异体才被编进
真实子进程。已登记差异 `specs/gateway-parity/diffs/A8-NATIVE-SNAPSHOT-CAPABILITY-001.md`
记录了 TS 接受纯空白 storage 而 native 入口拒绝的隔离边界；第三个键
`PILOTDECK_MANAGED_CONFIG` 是 A8 snapshot 两键范围之外的 Rust 侧 authority。本负控只
锚定 Rust 侧调用点，不声称两者一致。

## 运行

`python3 -I negative_controls.py --check` 检查锚点与标记绑定（不依赖 cargo）：除 marker
模板、三个键名与 scrub 标记外，还按**测试函数体切段**绑定观察——rejects 测试里的
`leftover.is_empty()` 与 `read_stderr_until(stderr, "native listening"`，scrub 测试里的
`Failed to parse default config YAML.` 与同一条广播读取（#1401 项 4 / 评审 P2：整文件
needle 分不清哪个测试在观察，两个测试都含 `native listening` 后删掉 rejects 那处仍会
通过）。实际负控命令为 `python3 -I negative_controls.py NEW_OUTPUT [mutation ...]`，必须由
调用方设置 `CARGO_TARGET_DIR`，证据目录走共享 `new_output_directory`（`#953` 非空拒用、
`.claim` O_EXCL），使用共享 HEAD 守卫、进程所有权和最终汇总：每项先正控，再
`--no-run` 确认变异体可编译，再判杀，恢复后复跑；过期锚点与无法编译都是 INVALID，
任一非 KILLED 即非零退出。未提交源码不能运行（`mutating_sources` 拒跑），不得使用
副本仓库绕过；由协调者在提交后的宿主树上运行。

`python3 -I early_exit_invalid.py NEW_OUTPUT`（同样必须设置 `CARGO_TARGET_DIR`，
`--check` 为静态检查）证明早退分支的两半：它把 `CARGO_TARGET_DIR` 里已构建的
`debug/pilotdeck-gateway` 原子换成 stub（临时名走 `tempfile.mkstemp`，#1401 项 2），
跑同一个绑定测试并用共享 `negative_suite.classify` 判定——无输出 stub（立即 `exit 86`）
必须 INVALID 且命名测试的 panic 载荷含早退分支专属消息；`native listening` stub（先向
stderr 打印 `native listening 127.0.0.1:1` 再 `exit 86`）必须 KILLED 且载荷含绑定
marker。两个变体的原始输出分别落 `early-exit-neg-eof.log` /
`early-exit-neg-listening.log`，互不覆盖。随后按 SHA-256 校验恢复原二进制并复跑正控。
证据目录走共享 `new_output_directory`（`#953` 非空拒用、`.claim` O_EXCL，#1401 项 3）。
它不变异任何源码，也不经过 `mutating_sources`，所以未提交的树（含本 fixture 所在
测试文件的改动）上也能运行。任一环节不符即非零退出。
