# #1136 大 stdin 与退出后 listener 检查

`oracle.json` 来自 Node v22.22.0 的真实 `runSdkCli` / SDK。输入 131081 bytes（128 KiB ASCII 加前后空白及中文），分别在启动后和 `new_session` IPC 钩子到达后写入。记录完整 prompt 的长度与 SHA-256、stdout/stderr、退出状态、连接数及关闭顺序。原始 stdin/stdout/stderr/wire 随生成目录保留；`generate.mjs` 使用 recursive 目录创建和独占 wx 文件写入。

探针是进程内 WebSocket double，只允许 `ws://127.0.0.1:28999/ws`。macOS 上 Node spawn 使用非阻塞 stdin socket，基线 `readFile('/dev/stdin')` 会 EAGAIN；探针在导入原 TS 前设为阻塞，模拟 shell 管道，未修改 TS。该前提不可解释为未适配的 Node spawn 也通过。

Rust 用旧 `sdk-run` 的已采集回放实例化大 prompt，对照新 oracle 的输入摘要、输出和退出状态；不宣称新 TS 原始 wire 与旧回放逐字相同。六族（run、resume、format、settings、sessions-list、sessions-messages）的后台 writer 由子进程 owner 持有，取消先 kill/reap 再有界 join；首次真实 pipe write 经 channel 报告线程身份，收到进展后 kill/reap 永不读取的子进程并 join；断言写入线程不是 start 调用线程，BOUND 只防挂起，不以耗时判杀。另有取消 registry、join 标记和退出后登记测试检查所有权。listener 由调用方持有到子进程退出，检查未被回放接收的连接。macOS 26.6.1 的最小探针已验证：子进程 connect 后 SO_LINGER=0 close 再退出，父进程仍能 accept，recv 为 ECONNRESET（5/5）。对应 RST 负控必须在实际宿主执行；其他内核不能仅凭这份 macOS 证据宣称通过。

从仓库根执行（先跑 cargo-slots，设置本树 CARGO_TARGET_DIR 与两个 debug profile）：

```sh
node src/pilotdeck-cli/tests/fixtures/sdk-run-stdin/generate.mjs <全新目录>
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-cli --test sdk_run_exit run_stdin_oracle_regenerates_identically -- --exact --ignored --show-output
python3 src/pilotdeck-gateway/tests/fixtures/sdk-cli-lifecycle/negative_controls.py --check
python3 src/pilotdeck-gateway/tests/fixtures/sdk-cli-lifecycle/negative_controls.py --compile-only <全新目录>
python3 src/pilotdeck-gateway/tests/fixtures/sdk-cli-lifecycle/negative_controls.py <全新目录>
```

负控 39 条：六族各有同步 writer 回退、漏 registry drain、漏 join、wire-end 后额外 FIN 连接、wire-end 后 RST 连接，run/resume/format 退出后登记回收，以及大输入阈值、输入摘要、delayed 数据截断、fixture 集合、早晚时序集合、退出后登记 writer 的回收及重生成漂移。wire-end 变异在已完成 close 后延迟 100 ms 再连接（超过旧 20 ms 窗口），仍在进程退出前发生；RST 变体先在未连接 socket 上设置 SO_LINGER=0，再 connect 并立即 drop；因此设置失败不会留下正常 FIN 连接来冒充 RST 判杀。不是调度压力测试。每项先正控、变异 no-run、nonce 绑定判杀、恢复正控；全部完成后原子汇总，任一 INVALID/SURVIVED 非零。新文件和改动源码不等于 HEAD 时拒跑，提交后由协调者宿主执行。
