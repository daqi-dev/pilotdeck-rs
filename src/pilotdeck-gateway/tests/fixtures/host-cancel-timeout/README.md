# Host post-cancel exit bound TS oracle

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。`oracle.json` 保存 `src/agent/modules/transport/stdioAgentLoopSidecarConnection.ts` 的 SHA-256 与真实 TS 模块里 `DEFAULT_KILL_TIMEOUT_MS` 的运行时值。

取消后 TS 的 execute 等待没有计时器（`agentLoopSidecarClient.ts:931-958` 只发送 cancel，`:543-556` 继续 `await iterator.next()`）；TS 唯一的有界宽限是关闭子进程时的 `DEFAULT_KILL_TIMEOUT_MS = 5_000`（`stdioAgentLoopSidecarConnection.ts:10`）。Rust 侧 `host::POST_CANCEL_EXIT_TIMEOUT` 直接取这个值，而不是更短的 `close_timeout`，以免丢弃 TS 仍会接收的迟到合法终态。

生成器用 tsx/esbuild 在内存中给基线文件追加一条 `export { DEFAULT_KILL_TIMEOUT_MS };`（只改可见性，不复制实现），import 后记录真实运行时值；同时断言基线 HEAD 等于固定 commit、tracked 工作树干净、工作树文件哈希等于 `git show` blob 哈希。写入临时文件后 rename，连续跑两次字节一致。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/host-cancel-timeout/generate.mts [outdir]
cd src
cargo test -p pilotdeck-gateway --lib host::tests::post_cancel_exit_timeout_matches_ts_close_grace
```

`host_cancel_timeout_fixture_regenerates_identically_from_ts` 是 `#[ignore]` 的重生成测试；普通测试只读 `oracle.json` 并把 `POST_CANCEL_EXIT_TIMEOUT` 与该 TS 值比较。金样不覆盖取消后的时序行为——TS 没有对应计时器可逐字节对照，那部分由 Rust 对抗 peer（`scripts/a6-host/fixture.py` 的 `host-cancel-*` 模式）加暂停时钟测试与负控钉住。

`peer.py` 是独立的对抗 peer，不是 TS 金样。它通过文件 gate 控制断连、replacement hello/resume、合法 final 与关闭：EOF 后写 `closing`，等待 `close-release` 才退出；全进程受 30 s alarm 约束。`host::tests::cancel_exit` 同时读取测试专用的循环退出／终态已验证信号，在时钟仍暂停于取消后的 5.001 s 时确认退出和开始关闭，再单独放行关闭；既覆盖终态前取消，也覆盖终态已收到但应用未完成时取消。已应用游标与回调销毁计数单独断言，不把“已收到 final”当成“应用完成”。

负载下的同步点：先等待宿主发布实际 `cancel_deadline`，断言等于取消时刻加 5 s，再推进暂停时钟；静默 resume 等待宿主校验完成信号。重连开始处有默认关闭、仅测试启用的 gate：测试以 WNOWAIT 确认旧 peer 真正退出后放行，避免 stdout EOF 早于 OS exit 时，旧连接清理的 5 ms Tokio 轮询被暂停时钟卡住。这个 gate 不推进虚拟时间、不跳过实际关闭和握手；4.9 s 未退出、5.001 s 已退出、游标和销毁断言保持原样。

重连 peer 主动在旧进程仍活着时关闭 stdout，并等待 `original-exit-release`；测试先见到重连开始，再放行旧进程退出并通过 WNOWAIT 确认。因此 EOF 早于 OS exit 的交错每次都会发生，移除重连 gate 的负控必定卡在旧连接清理，而非靠负载碰运气。
