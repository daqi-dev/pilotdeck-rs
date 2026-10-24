# A5.2 并发 / 重启夹具

入口为 `a52_fixtures.rs`（Rust 默认回归）和 `a52_baseline.rs`
（需要真实 TS 环境、显式执行的集成验收）。范围按
`docs/plan/005_phase-a-session-domain.md` §A5.2；复用
`scripts/a5-19/` 的预注册、独立 observer 和负控，不改冻结尺子。

## 运行

在 `src/` 下，先核 `../scripts/cargo-slots.sh`。每棵树独立设置
`CARGO_TARGET_DIR="$PWD/target"`，然后运行：

```sh
cargo test -p pilotdeck-gateway --test a52_fixtures
PDRS_BASELINE=/absolute/path/to/pinned/PilotDeck \
  A52_EVIDENCE_DIR=/absolute/path/to/new/evidence-directory \
  cargo test -p pilotdeck-gateway --test a52_baseline -- --ignored --nocapture
```

第二条要求已安装 Node 22、固定基线 `cc7c4e6` 和已构建的 TS sidecar；
不下载依赖或预编译二进制。证据目录必须尚不存在，父目录需存在。
启动器在 Node 启动前隔离 HOME / PILOT_HOME，cold 只终止并回收自己
启动的 PID；WS 使用 `127.0.0.1:48789`，端口占用即失败，不扫描或杀占用者。

## 独立判据

| 夹具 | 刺激与同步 | 独立断言 |
|---|---|---|
| 双连接 | 同一生产 `TurnGateway` 上两个已 hello 的连接；第一条进入 stream gate 后第二条请求 | 第二条 `session_busy`；只有一次 session.submit；第一条成功事件和磁盘记录 |
| 多会话 | 两条 stream 都进入 gate，仅释放第一条 | 第一条结束时第二条仍 running、无 completion；释放后各自仅有自己的 runId |
| 旧 close / 新 reconnect | 显式 `reconnect_interaction` 成功，核新 binding 后旧连接 closed | 保留基线的 stale close 仍可 abort 当前 run 语义；生产 close 中间边界由 TS 8 例单独实测 |
| registry 写失败 | 只在独占 scratch 内把父目录换成普通文件，触发真实 ENOTDIR | 原始磁盘字节不变；fresh reader 无事件/result；恢复后能重试 |
| snapshot 写失败 | snapshot root 是普通文件 | 失败不是 committed，工作区原文不变；修复根路径后同一 recorder 重试确有 manifest 和 `_COMMITTED` |
| 冷重启 | 子进程写 accepted / running / 显式 result_unknown 后退出，父进程重新 open | PID 身份、state/revision/seq/result 和原始 events；不把 drop/reopen 当进程重启 |

成功路径固定业务事件 `turn_started → input_accepted → turn_completed`。
实时业务帧、WS final 复本、registry seq/revision/result 以及 raw snapshot
分别检查。原生 observer 另有 4 个指定规则负控：丢实时帧、漏 registry event、
破坏 seq、覆盖 result；必须命中对应规则，普通异常不算检出。
双连接竞争与 stale close 前均断言 abort 为零，避免错误的提前 abort 掩盖交错。
每个 gate 的进入通知先于释放；15 秒仅是失败看门狗。
冷启轮询只等子进程退出，20 秒到期 kill/wait 后失败，不用于挑选通过的交错。

## #19 六格实测

`a52_baseline` 串行调用现有六套装置并检查完整集合、次数和进程回收；六格、并发、生产 close、sidecar 另核组内稳定性，cold / WS 逐次检查 oracle 与身份：

| 套件 | 范围 | 预注册次数 |
|---|---|---|
| `run.py` | 36 个 writer/次序子例 × model/tool；有/无 registry × 快/慢/失败；unknown-first / known-final-first / after-complete | 72 × 5 = 360 |
| `run-b.py` | 双连接、多会话、close、store 故障 | 34 × 5 = 170 |
| `run-production-b.py` | 真实生产 close 次序 | 8 × 5 = 40 |
| `run-b.py --sidecar` | 真实 sidecar：有/无 registry × model/tool | 4 × 5 = 20 |
| `run-external-b.py --suite cold` | running / 显式 unknown 的跨 PID 重启 | 2 × 5 = 10 |
| `run-external-b.py --suite ws` | 高位 loopback 上双连接 / 多会话 | 15 × 5 = 75 |

六格的帧、durable status 与 FileRunRegistry events/result 观察面及独立
破坏对照复用 `observation_rules.mjs`；状态写入与 registry persist 的
barrier / 失败分别注入。生产 registry 的迟到 error 可以覆盖 result，
这些是 TS 实测现象，不是对 Rust timeout 修正的批准。真实 sidecar 四格
仅交叉确认快 writer，不冒称覆盖所有慢写与失败组合。

## 明确保留的边界

- FileRunRegistry 的冷启动状态是 `accepted/running → interrupted`，并不新增
  `result_unknown` event/result。预先写入的 unknown 保持原值。不能把请求中的
  “pending/running → unknown”当成实际存在的 registry enum 或自动状态转换。
- 真 Web bridge 重连仍由独立 D5 门承担，原有 `NOT_RUN` 原样保留；本套 WS
  测的是 gateway server 和独立 client driver。
- 默认 cargo test 不自动启动 TS 与监听端口；忽略项必须用上面的命令显式执行，
  日志和每个 suite 的 raw manifest 才是实测证据。`cold_restart_child` 是子进程
  辅助入口，无环境变量时不计作一条独立夹具。

旧 `scripts/a5-19/summarize-b.py` 绑定历史单格 sidecar 采集（5 次），不能接收
本入口四格 20 次的 manifest。本入口直接检查原始 manifest，不调用该历史聚合器；
现代汇总的兼容修复 [#623](https://github.com/daqi-dev/pilotdeck-rs/issues/623)
须在 A5.4 聚合验收前完成。
