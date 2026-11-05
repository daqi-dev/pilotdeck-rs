# #1576 / #1555 Node 侧原始输出（R7 引用）

Node v22.22.0（macOS arm64，raw socket 直打 `node:http` `createServer` 无选项）
在 2026-11-06 采集，用来更正 `specs/gateway-parity/diffs/A7.4-HTTPSTORE-431-408-SHAPE-001-R6.md`
的三处越界。每个 `.out` 都由同名前缀的 `.mjs` 直接跑出来，复算方式：

    node node-obs-fold-probe.mjs > /tmp/x.out && diff /tmp/x.out node-obs-fold-probe.out

`.out` 打印的是**稳定投影**（响应字节数 + 状态行；410 类拒绝另附完整 hex），不含
`Date`，因此同一 Node 版本重跑逐字节一致。被拒连接的 `why` 字段在 `EOF` 与
`error:ECONNRESET` 之间可能抖动（连接拆除时序），判读看 `bytes` 与 `head`。
四个 CR / 折叠 / 请求行探针都**不装** `clientError` 处理器，47 B 400 是 Node 默认
形状；需要对照组时以不装处理器的 `node-obs-fold-probe` 背书。

- `cr-ows-probe.mjs` / `.out` — 值内 CR 的两格（`X: \r  \n`、`X:\r \r\n`）都是 **47 B 400**；
  两个对照格 `X: ok` 与 `X:  ok`（冒号后前导空格）都是 **96 B 200**。用来把 400 归因到 CR
  本身而不是值前导空格。
- `cr-ows-length-probe.mjs` / `.out` — CR 族第三形状的长度实测（fix-r3 补，fix-r4 加真 431
  对照）：`X: \r` + 90,000 空格 + `  \n`（与 Rust 测试
  `cr_in_value_past_the_raw_bound_keeps_the_registered_431` 同一字节形状）与值长
  16,300 / 16,360 / 16,380 / 16,400 空格四档**全部 47 B 400**；`ows-90000-control`
  （90,000 空格前导 OWS）96 B 200；`long-value-16380` / `long-value-20000`
  （非空白长值）**67 B 431**。后两行是「这台 server 不是只会 400」的反证：Node 的
  16 KiB `maxHeaderSize` 溢出确实以 67 B 431 出现，而 CR 在值第 33 B 时 llhttp 在 CR
  处即停、根本走不到溢出记账，所以 CR 形状任意长度都是 400；纯空白值则完全不计入
  上限（90,000 空格仍 200）。四档 `cr-pad-*` 的 CR 都固定在第 33 B，属冗余确认，
  真正跨过 16 KiB 的是 `cr-90000` 一行。
- `node-obs-fold-probe.mjs` / `.out` — 折叠续行（`X: a\r\n<SP>b`）短 / 90,000 空格 / 16,400 B
  三格**全部 47 B 400**；同文件 `leading-ows-90000`（冒号后前导 OWS）对照格是 200 / 141 B，
  说明拒绝发生在折叠本身而非填充量。R7 §范围 1 用这一格更正 R6 的「Node 该形状 200」。
  该探针不装 `clientError` 处理器，是「47 B 400 是 Node 默认形状」的独立凭据。
- `node-reqline-probe.mjs` / `.out` — 请求行 90,000 空格（target 前 / 后）、短双空格、plain
  四格**全部 200 / 141 B**。R7 §范围 3 用这一格登记「请求行分隔符超 raw 上界时 Rust 431 /
  Node 200」。
- `rss-16x-serialized.txt` — `fields-max` 与 `fields-max-16x` 的 receipt 转录（fix-r1 原始
  输出，当时 receipt 还没有 `flavor=` 字段）与 `tokio-macros 2.6.1` 的 `new_current_thread`
  依据；比值 14.98×，**不是** 16× 下界。fix-r3 起 receipt 打印运行时读出的
  `flavor=CurrentThread` 并有 `assert_eq!`。
- `tokio-test-current-thread.txt` — `tokio-macros` 2.6.1 `src/entry.rs` 的默认 flavor 代码行。

`rss-16x-serialized.txt` / `tokio-test-current-thread.txt` 是转录（含仓外 `_tmp/` 原始路径），
其余**四对** `.mjs`/`.out` 是可直接重跑的探针与输出；连同转录共十份文件，不含本 README。
