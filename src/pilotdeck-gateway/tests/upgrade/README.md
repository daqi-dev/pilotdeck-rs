# #56 真实 HTTP 升级边界原件

调用真实固定基线 CLI 的默认 gateway 装配与 GatewayServer HTTP parser，不修改 resources。只覆盖升级读边界；不覆盖 #65 调度顺序、完整 A4 或真人验收。

```sh
node src/pilotdeck-gateway/tests/upgrade/record.mjs \
  /absolute/path/to/PilotDeck \
  /absolute/path/to/exact-Cargo-artifact/pilotdeck-gateway \
  /absolute/path/to/new/evidence-output
```

输出目录必须不存在。基线需已有 dist 与依赖；调用者先从独立 target 的 Cargo JSON compiler-artifact 绑定当前产品源码与二进制，不能靠共享产物路径代替来源证明。脚本记录输入二进制/基线源码与 dist 的前后 SHA；测试脚本提交后的 HEAD 变化不代表产品源码变化，调用者仍须给产品 hash 关联。只启动自有 loopback 监听、隔离 HOME/PILOT_HOME/TMPDIR/数据库/工作目录，无模型调用或真实用户目录访问；就绪轮询有界，协议时序以真实 101 屏障和响应为依据。

12 格：直连 TS / Rust→TS 各 3 种初始 head（空、完整 masked ping、该 ping 三字节前缀）× 后来 hello 一段/两段写入。透明 relay 扣住真实 TS 的 101，客户端完成后来 hello 写入后才放行。真实 HTTP server 的只读观察器记录 upgrade head 和后续 socket data；直连 TS 初始 head 必须逐字节等于该格的初始 tail。完整初始 ping 不得出现 pong；后来 hello_ok、101 后 describe_server、FENCE pong 各一次。Rust 到 TS 不得夹带原始初始 head。

**证据分工**：一次 TCP write 不等于一次 read。此 HTTP 入口实测不能独自证明 Rust 初始 read 所有权；后者由同 PR 的 `http_proxy_tests.rs` scripted AsyncRead、无读门阳性对照和生产 gate/pair helper 确定性测试证明。半帧“只发后来 suffix”的异常路径仍由对应单元矩阵证明，本录制器不冒称已实测它。继承环境、分块、不完整样本或额外帧不能靠删字段后视为等价。

原件 `report.json` / `ts-observed.jsonl` / 各子进程日志留调用输出目录；报告必须 PASS、精确 12 个唯一 case、所有自有 child 已停、前后 SHA 相同。首次本轮原型失败及后续成功均留主仓 `_tmp/20261016-debt-cleanup/upgrade/ts-http-oracle/`。本录制器不改变冻结尺子或判分语义，不登记新豁免。
