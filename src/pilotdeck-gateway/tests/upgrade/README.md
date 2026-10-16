# #56 真实 HTTP 升级边界原件

调用真实固定基线 CLI 的默认 gateway 装配与 GatewayServer HTTP parser，不修改 resources。只覆盖升级读边界；不覆盖 #65 调度顺序、完整 A4 或真人验收。

```sh
node src/pilotdeck-gateway/tests/upgrade/record.mjs \
  /absolute/path/to/PilotDeck \
  /absolute/path/to/exact-Cargo-artifact/pilotdeck-gateway \
  /absolute/path/to/new/evidence-output
```

输出目录必须不存在。基线需已有 dist 与依赖；调用者先从独立 target 的 Cargo JSON compiler-artifact 绑定当前产品源码与二进制，不能靠共享产物路径代替来源证明。脚本核产品目录无未提交修改，记录 HEAD 与输入二进制/基线源码及 dist 的前后 SHA；脚本或 cfg(test) 提交导致的 HEAD 变化须由产品源码 hash 关联。

只启动自有 loopback 监听，隔离 HOME/PILOT_HOME/TMPDIR/数据库/工作目录，无模型调用或真实用户目录访问。就绪轮询有界，协议时序以真实 101 屏障、socket data 和实际异常为依据。子进程、stdio 与日志分别有界收尾；SIGINT/SIGTERM 进入同一个 finally 清理。

共 18 个样本，分三组保存：

- `results` 14 条：原 12 格（直连 TS / Rust→TS，各空/完整 masked ping/该 ping 三字节前缀 × 后来 hello 一段/两段写入），再加两侧 POST `/ws` 合法升级。透明 relay 扣住真实 TS 的 101，客户端完成后来 hello 写入后才放行。直连 TS 的实际 upgrade head 须逐字节等于初始 tail，Rust 上游 TS head 须为空；hello_ok、describe_server、FENCE pong 各一次，初始 ping 不得产生 pong。
- `routingResults` 2 条：两侧 GET `/ws` 带 Upgrade header、缺 Connection Upgrade token，记录完整普通 HTTP wire、状态、body、EOF，比较实际 HTTP 行为。
- `suffixResults` 2 条：每侧新建独立 baseline 进程、HOME 和数据库；只随 HTTP headers 发送 ping 的前三字节，收到真实 101 后只发送剩余 suffix。保存实际 TS head/data、客户端原始 wire、frames、基线 uncaughtExceptionMonitor 观察及主动清理前 socket/进程状态。observer 只记录异常，不接管基线的异常策略；不能把主动清理后的 close 冒充远端 EOF。

`check_suffix.mjs` 在录制器给出 PASS 前逐项比较清理前的 EOF/destroyed/closed、socket errors、基线 exitCode/signalCode；PID、堆栈不作相等条件。缺字段或单侧提前结束均 FAIL。`node --test src/pilotdeck-gateway/tests/upgrade/check_suffix.test.mjs` 有匹配正控与七个变异负控；checker 本身纳入运行前后输入 SHA。

`inputs.json` 保存 Node 版本/可执行文件、录制器、观察器、watcher、关键 TS/Rust 源码及协议 payload 原件的 SHA/hex。`report.json` 同时绑定各隔离目录中实际生成配置的前后 SHA、二进制与基线的前后 SHA、输入索引/观察记录/每个 child log 的 SHA。配置留在对应 `runtime/**/home/pilotdeck.yaml`，不依赖另一轮 A2 的配置。

**证据分工**：一次 TCP write 不等于一次 read。此 HTTP 入口实测不能独自证明 Rust 初始 read 所有权；后者由同 PR 的 `http_proxy_tests.rs` scripted AsyncRead、无读门阳性对照和生产 gate/pair helper 确定性测试证明。上游失败且初始 head 残留的两格也是候选自身生产路径断言：invalid WS bytes → HTTP 400 终止，完整普通 HTTP head → 既有 HTTP 处理；直连 TS 没有代理上游失败，不伪造 TS 故障对照。

验收原件须 `status=PASS`、14/2/2 个唯一 case、所有自有 child 已停、cleanupErrors 为空、全部前后 SHA 相同。已有原型/失败记录继续保留主仓 `_tmp/20261016-debt-cleanup/upgrade/ts-http-oracle/`；旧配置的追加 SHA 索引为相邻 `legacy-config-bindings.json`，不改写旧 report。本录制器不改变冻结尺子或判分语义，不登记新豁免。
