# #867 共享模型 HTTP 编码金样

`generate.mts` 在 Node **v22.22.0** 上调用真实 `fetch` 和真实 loopback HTTP server，不复制 fetch 或产品函数体。固定 `cc7c4e6` 的模型、EdgeClaw 源闭包由 `baseline_source_guard.mjs` 只读核验；本夹具的运行时判据是 Node 内置 fetch 的字节行为。输出目录递归创建，`oracle.json` 用 `wx` 独占写入。

- `cases`：gzip、x-gzip、deflate（zlib/raw）、br、identity、大小写、反序编码栈、多 gzip member、未知整串透传、坏流、空流、头/正文/尾截断、HEAD、204、205；同时记录保留的 wire headers。
- `headers`：Latin-1、宽字符、孤立代理项、trim、重复头、控制字符、CR/LF、非法/宽字符/代理项头名、整 record 的 ByteString 转换顺序。
- `lifecycle`：三种 codec 在服务端发送第一段并停住时得到明文；尾段随后释放或前驱永不完成时取消。
- `endings`：完整压缩成员到达仍不能代替 HTTP EOF，分别释放终止块、取消、断链。`interrupted` 则在成员中途断链。此处明确使用 chunked + keep-alive；Node 对 `Connection: close` 的额外容忍不充当正常 HTTP EOF 判据。
- `progress`：wire 每 400 ms 送一个 gzip 头字节，bodyTimeout=1000 ms，1600 ms 后送完；正常得到明文，但独立 900 ms reader watchdog 仍到期。Rust 用 paused clock 和受控 body frame 覆盖这两格；空 data frame 只传递 raw 活动，不冒充明文。

Node 全程有 10 秒 watchdog；所有响应与 agent/server 在 finally 关闭。Rust loopback 等待有 10 秒上限，所有 server task 均 join。`encoding_fixture_regenerates_identically` 是 ignored 字节重生成守卫。

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
export CARGO_TARGET_DIR=/Users/da/ws/pilotdeck-rs/_build/wt-867/_build/cargo-target
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
scripts/cargo-slots.sh
cd src
cargo test -p pilotdeck-gateway --lib encoding_tests
cargo test -p pilotdeck-gateway --lib encoding_fixture_regenerates_identically -- --ignored
```

`negative.py --check-anchors` 只读核锚点。正式变异须产品源码已经提交且与 HEAD 一致；`negative_process.mutating_sources` 持树锁、核对 HEAD 并还原，cargo 由 `run_owned` 管理，调用前 `wait_for_slot`。脚本必须继承调用方的 `CARGO_TARGET_DIR`。每项做正控 → 变异精确测试 → 还原正控，逐项记录 KILLED / SURVIVED / INVALID，最后统一非零判失败；编译失败、无关 panic、超时、锚点过期均不是 KILLED。点名测试每个本地失败点均有 `#867` 消息，杀死判据绑定命名测试的 panic 块。

全套 28 个变异需多次重编，交协调者宿主执行；本树未提交时应拒绝，不能创建副本仓库或绕过守卫。证据目录由调用者指定并独占创建：

```sh
python3 src/pilotdeck-gateway/tests/fixtures/model-http-encoding/negative.py _tmp/20261026-issue-867/negative-host
```
