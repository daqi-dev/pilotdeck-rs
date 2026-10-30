# #1131 A4 监听地址

`generate.mts` 用 Node v22.22.0 实际启动固定基线 `cc7c4e6` 的 CLI `server --port 0`，随后在已释放的实际端口上再次固定启动。使用独立 HOME / PILOT_HOME / cwd、仅本地假 provider 配置、关闭 telemetry / cron；不继承密钥。两次均从 stdout 取实际地址，请求 `/health` 得到 200 和 `{ok:true}`。原始 stdout/stderr 连同请求端口与实际端口打印到生成器 stdout。

`oracle.json` 只将动态端口替换成 `<PORT>`，保留两行地址的文本、顺序和源码闭包哈希。Rust 用例执行真实 `pilotdeck-gateway` 二进制，从 stdout 读回地址，要求非零、可 TCP 连接、固定端口保持不变、两行文本及 LF 字节匹配金样。该范围只核地址发布；A4 转发代理的 HTTP/WS 业务能力由原有用例负责。

```sh
export CARGO_TARGET_DIR=/absolute/path/to/this-tree/_build/cargo-target
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
export PDRS_BASELINE=/absolute/path/to/PilotDeck
scripts/cargo-slots.sh
cargo test --offline --locked --manifest-path src/Cargo.toml -p pilotdeck-gateway --test proxy_listen_address
scripts/cargo-slots.sh
cargo test --offline --locked --manifest-path src/Cargo.toml -p pilotdeck-gateway --test proxy_listen_address regenerates_identically -- --exact --ignored
```

重生成到未存在的输出文件（不覆盖 oracle）：

```sh
node --disable-warning=ExperimentalWarning --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/proxy-listen-address/generate.mts /new/output/directory
```

负控必须在提交后、源码等于 HEAD 的本树运行；不另设构建目录：

```sh
python3 -B src/pilotdeck-gateway/tests/fixtures/proxy-listen-address/negative_controls.py --check
python3 -B src/pilotdeck-gateway/tests/fixtures/proxy-listen-address/negative_controls.py /new/evidence/directory
```

七个变异分别退回请求地址、破坏 HTTP 前缀、破坏 WS 路径、将 LF 改为 CRLF、破坏固定端口地址、发布旧端口后换绑其他监听器、篡改 oracle 的基线 commit（该变异在共享 `common/baseline.rs` 溯源 gate 的 panic 上判杀）。每项先正控，再应用变异并 `cargo test --no-run`，随后运行精确绑定测试、恢复并重跑；使用共享 `negative_suite` 判杀，全部逐项汇总，任一 SURVIVED / INVALID 非零退出。锚点失配直接报错；源码守卫拒绝和编译失败均不能计 KILLED。守卫范围含产品源码、本测试、oracle、共享执行器与溯源 gate（`common/baseline.rs`），任一偏离 HEAD 即整轮拒跑。

启动等待每行最多 15 秒，TCP 连接最多 3 秒；Rust 子进程在测试成功或 panic 时 kill + reap。TS 启动最多 15 秒、health 最多 3 秒，并在 finally 中 kill + reap。本单不修改产品会话或取消生命周期。
