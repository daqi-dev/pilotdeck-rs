# #1079 原生 WS 跨连接资源预算

固定 TS cc7c4e6 + Node v22.22.0，`generate.mts` 通过 esbuild 原样加载实际
GatewayServer、GatewayWsConnection、TextWebSocketConnection；未触发的依赖 fail-closed。
65 次真实 TCP 升级与一个未完成 HTTP 握手并存，全部端口由系统分配。
聚合缓冲探针对 GatewayServer 实际构造的 transport 发确定性 `data` 回调，
避免 TCP 分片影响内存观测；三条连接各收到未完成的 100 MiB 帧的 90 MiB 前缀。
它证明 TS 在 270 MiB 样本中仍接收，不声称有限实验能证明数学上的无限。
不以 RSS/GC 作为判断，也不修改基线。

| 格 | TS 金样 | Rust 回归（native_ws::caps_tests） | 关系 |
|---|---|---|---|
| C1/C2 | connections | connection_cap_covers_handshakes_and_recovers | 超限差异 |
| C3 | cancelled / replacement | connection_cap_covers_handshakes_and_recovers | 正常重用；Rust 错误回收单测 |
| B1 | aggregate | aggregate_overload_isolated_from_existing_peers | 超限差异；前驱永不消费 |
| B2 | consumed | consumed_frames_release_backing_after_tail | 消费后复用；backing 计账为 Rust 内部约束 |
| B3 | cancelled | protocol_error_releases_input / eof_releases_input_before_pending_business | 取消可观察；EOF/协议错误为 Rust 回收断言 |
| B4 | 不观测 GC/瞬时分配 | growth_charges_old_and_new_backing | Rust 内部约束，不适用 TS 字节对拍 |
| S1 | pendingStop / cancelledStop | shutdown_and_abort_release_resources | TS 无期限；Rust 沿用有界 drain 差异 |

预算计 decoder **backing 容量**，新旧扩容同时计账、非空 tail 保留整个 backing 的收费。
只有 decoded Frame 已丢弃且 buffer 为空才释放存储与收费；退出读取时先于业务 drain 释放。
每连接另有固定 8 KiB 栈上读取块；本预算不涵盖已解码 payload、业务请求或输出队列，
这些继续受既有单帧、pending request 和输出上限约束。

```sh
PDRS_BASELINE=/path/to/PilotDeck node --disable-warning=ExperimentalWarning \
  --import /path/to/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/native-ws-caps/generate.mts /fresh/output
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib \
  native_ws::caps_tests::regenerates_identically -- --exact --ignored --nocapture
python3 src/pilotdeck-gateway/tests/fixtures/native-ws-caps/negative_controls.py --check
python3 src/pilotdeck-gateway/tests/fixtures/native-ws-caps/negative_controls.py /fresh/evidence
```

负控只在 `test + pdrs-mutants` 编译中启用，生产分支恒 false。
执行器保持 `mutating_sources` 的源码=HEAD 守卫；未提交工作树会拒绝，交协调者提交后运行，
不使用替代仓库。各变异先 no-run 编译，再正控、负控、恢复正控；全部完成后原子发布汇总。
调用者必须显式传入 CARGO_TARGET_DIR，不使用第二构建目录。
