# #1351 WS 升级分支 16 KiB 解析预算边界金样

`generate.mts` 在固定基线（`cc7c4e653b15`，`_shared/baseline.mts` 绑定）上加载真实 `startGatewayServer`（127.0.0.1 loopback、无静态根、token `fixture-token`），用原始 socket 逐格记录 Node v22.22.0 的 TrackHeader span 预算在**升级与非升级两条车道**上的边界行为：

| cell | 车道 | tracked | 响应 | closed |
|---|---|---|---|---|
| upgrade-16383 | `/ws` 升级 | 16383 | 101 Switching Protocols | false |
| upgrade-16384 | `/ws` 升级 | 16384 | 罐头 431 + close | true |
| upgrade-16385 | `/ws` 升级 | 16385 | 罐头 431 + close | true |
| upgrade-20480 | `/ws` 升级 | 20480 | 罐头 431 + close（20 KiB 远超格） | true |
| upgrade-17020 | `/ws` 升级 | 17020 | 罐头 431 + close（桥接 #1292 `b4-U1.out`） | true |
| plain-16383 | `/health` | 16383 | 200 chunked `{"ok":true}` | false |
| plain-16384 | `/health` | 16384 | 罐头 431 + close | true |
| plain-16385 | `/health` | 16385 | 罐头 431 + close | true |
| plain-20480 | `/health` | 20480 | 罐头 431 + close（20 KiB 远超格） | true |

16385 / 20480 格为 2026-11-06 轮独立原始 socket 探针（`_tmp/20261106-issue-1351/probe.out`）先行实测后并入金样：两车道在含 16384 的边界上逐字节同口径，越界格均为同一罐头 431 + close。

口径：升级头 = 3+13+17+16+41+3+n = 93+n，非升级头 = 23+n；`trackedSpan` 从请求字节自算并逐格断言，算术不靠注释信任。捕获语义与 `native-http-basic` 一致：250ms 静默窗、EOF/复位即关闭、date 记录时掩码（latin1 记账）。`x-p` 头携带 'a' 填充；nonce 为 RFC6455 §1.3 公开样例（`tests/common/native_ws_sample.rs` 同值）。

收尾差异：TS `TextWebSocketConnection` 对裸客户端 FIN 不 resume（服务端 socket 悬挂、`server.close()` 不返回），因此 101 格捕获结束后由探针补一次真 ws 客户端的 masked close 握手等 FIN——记录字节在静默窗结束时冻结，握手不影响金样；这是装置收尾语义，不是被测行为。

Rust 侧复放：`native_ws::tests::upgrade_budget_boundary_matches_ts` 对 native WS 监听重放全部格，归一化（date 掩码、头名小写、丢弃 hyper 隐式 `content-length: 0`——仅 `parser_admission` 登记的 431 格，其余格逐字比（#2153）、chunk 尺寸小写——已登记传输差异）后逐格比对，另核 closed 标志。再生成守卫 `regenerates_budget_fixture_identically`（`#[ignore]`）以 PATH 上 v22.22.0 的 node 逐字节再生比对。

oracle 的 `sourceSha256` 由 `baselineSourceHashes` 展开为入口文件的 import 闭包哈希；本 fixture 平台无关（无静态根、无文件系统相关格），macOS/Linux 记录与再生均一致。
