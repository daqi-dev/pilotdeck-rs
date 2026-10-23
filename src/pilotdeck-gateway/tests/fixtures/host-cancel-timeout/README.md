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
