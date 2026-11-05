# #1576 / #1555 Node 侧原始输出（R7 引用）

Node v22.22.0（macOS arm64，本机 raw socket 直打 `node:http` `createServer` 无选项）
在 2026-11-06 采集，用来更正 `specs/gateway-parity/diffs/A7.4-HTTPSTORE-431-408-SHAPE-001-R6.md`
的三处越界。每个 `.out` 都由同名前缀的 `.mjs` 直接跑出来，复算方式：

    node node-obs-fold-probe.mjs > /tmp/x.out && diff /tmp/x.out node-obs-fold-probe.out

`Date` 头与连接建立耗时随运行变化，`.out` 里已按 `bytes=` + 前 40 字节比较，逐字节稳定。

- `cr-ows-probe.mjs` / `.out` — 值内 CR 的两格（`X: \r  \n`、`X:\r \r\n`）都是 **47 B 400**；
  两个对照格 `X: ok` 与 `X:  ok`（冒号后前导空格）都是 **96 B 200**。用来把 400 归因到 CR
  本身而不是值前导空格。fix-r3 更正：`X: <sp>ok` 这一格此前 payload 与 `X: ok` 逐字节相同
  （标签里的 `<sp>` 没真写进字节），现在是真的多一个空格。
- `cr-ows-length-probe.mjs` / `.out` — CR 族第三形状的**长度跨档实测**（fix-r3 补，回答「短形状 400 是不是外推到长形状」的质疑）：`X: \r` + 90,000 空格 + `  \n`（与 Rust 测试 `cr_in_value_past_the_raw_bound_keeps_the_registered_431` 同一字节形状）、以及值长 16,300 / 16,360 / 16,380 / 16,400 空格四档（跨 Node 的 16 KiB `maxHeaderSize`），**全部 47 B 400**；同文件 90,000 空格前导 OWS 对照格仍是 96 B 200。即 llhttp 在值内 CR 处就停，长到 90 KB 也不会先撞 `HPE_HEADER_OVERFLOW` 的 431。
- `node-obs-fold-probe.mjs` / `.out` — 折叠续行（`X: a\r\n<SP>b`）短 / 90,000 空格 / 16,400 B
  三格**全部 47 B 400**；同文件 `leading-ows-90000`（冒号后前导 OWS）对照格是 200 / 141 B，
  说明拒绝发生在折叠本身而非填充量。R7 §范围 1 用这一格更正 R6 的「Node 该形状 200」。
- `node-reqline-probe.mjs` / `.out` — 请求行 90,000 空格（target 前 / 后）、短双空格、plain
  四格**全部 200 / 141 B**。R7 §范围 3 用这一格登记「请求行分隔符超 raw 上界时 Rust 431 /
  Node 200」。
- `rss-16x-serialized.txt` — `fields-max` 与 `fields-max-16x` 的 receipt 转录（fix-r1 原始
  输出，当时 receipt 还没有 `flavor=` 字段）与 `tokio-macros 2.6.1` 的 `new_current_thread`
  依据；比值 14.98×，**不是** 16× 下界。fix-r3 起 receipt 打印运行时读出的
  `flavor=CurrentThread` 并有 `assert_eq!`。
- `tokio-test-current-thread.txt` — `tokio-macros` 2.6.1 `src/entry.rs` 的默认 flavor 代码行。

`rss-16x-serialized.txt` / `tokio-test-current-thread.txt` 是转录（含仓外 `_tmp/` 原始路径），
其余四对 `.mjs`/`.out` 是可直接重跑的探针与输出。
