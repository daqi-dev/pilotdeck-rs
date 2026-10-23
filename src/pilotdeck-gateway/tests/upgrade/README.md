# #56 真实 HTTP 升级边界原件

调用真实固定基线 CLI 的默认 gateway 装配与 GatewayServer HTTP parser，不修改 resources。只覆盖升级读边界；不覆盖 #65 调度顺序、完整 A4 或真人验收。

输出目录必须不存在。基线需已有 dist 与依赖；构建与录制都使用调用者已经隔离、持有的 target，保留同一次构建的原始 Cargo metadata / JSON compiler-artifact 及 dep-info。先在干净 checkout 准备构建证据，再录制（Node v22；开编译前先运行 `scripts/cargo-slots.sh`）：

```sh
# CARGO_HOME / CARGO_TARGET_DIR 由启动器设置；target 必须在本 checkout 内。
node src/pilotdeck-gateway/tests/upgrade/source_identity.mjs \
  "$PWD" /absolute/path/to/new-build-evidence.json
PDRS_SOURCE_IDENTITY_BUILD=/absolute/path/to/new-build-evidence.json \
  node src/pilotdeck-gateway/tests/upgrade/record.mjs \
  /absolute/path/to/PilotDeck \
  "$CARGO_TARGET_DIR/debug/pilotdeck-gateway" \
  /absolute/path/to/new/evidence-output
```

`source_identity.mjs` 的 `producerInputs(root, buildEvidence)` v2 合同从 `cargo metadata --offline --locked --format-version 1` 的 resolve 图派生本地 path 包闭包，包含经 registry 节点到本地 patch 的依赖；不再维护手写包名单。实际编译的源码、模块和 include 资产来自同次 `cargo build --offline --locked --message-format=json` 的 rustc dep-info，不扫描 Rust 文本。注释、宏别名、三种宏括号、嵌套 include、`#[path]` 和生产源码引用 tests 资产均按 rustc 实际输入覆盖。

每个本地 compiler-artifact 必须 `fresh=false`，dep-info 必须唯一对应该 artifact 的输出（库按带 hash 的输出路径；Cargo 顶层 binary / build-script 别名按产物字节身份）、包含其 target 源文件、时间落在该次构建区间且输入不比 dep-info 新。缺失、复用、错配、解析失败、树外路径、symlink 均 fail-closed，绝不从目录中随意挑一份旧 `.d`。已用过的 target 若出现缓存命中，会报 `DEP_INFO_STALE`；调用者在持有的隔离 target 中重建受影响的本地包后重新准备证据，不自动清理 target，也不另建完整 gateway target。Cargo/rustc 内部目录布局若变化也会拒绝，需更新显式绑定逻辑。

录制器将原始构建记录复制为输出目录的 `cargo-build.json`，纳入 inputs 前后 SHA 与 report 的 artifact SHA。编译后的证据立即保存 dep-info、产物、编译输入 SHA256；录制前后重新核验。build.rs 自身由其编译 dep-info 纳入；OUT_DIR 生成文件只允许位于本 checkout 的 target 内且绑定 Cargo 的本地 `build-script-executed.out_dir`，单独哈希并在 cleanup 后复核，不要求把生成文件提交 Git。build.rs 运行时读文件、外部工具、环境变量以及 registry 依赖的来源仍由调用者保存；dep-info **不是**生成器运行时文件访问跟踪器。生成的编译输入字节受本合同覆盖，不能由本合同宣称生成器所有输入也已覆盖。

合同还保留本次编译目标的源码目录、metadata 闭包内 manifest、recorder/watcher、工具链声明、build/config 存在性及 Git HEAD / staged / unstaged / scoped untracked（含 ignored）检查，拒绝 skip-worktree / assume-unchanged。仅 state/docs 改动不挡。`report.sourceIdentity.version=2` 与 binary 前后 SHA 分开；录制 binary 必须等于证据里的 Cargo executable，cleanup 后漂移保存 FAIL 与诊断。证据文件是调用者保管的构建记录，**不是防伪构建证明**。旧 v1 或缺字段的 report 保留原件，不追认或作废旧运行。

回归直接运行 `node --test src/pilotdeck-gateway/tests/upgrade/*.test.mjs`；真实小型 workspace 在 `/tmp/pdrs-source-identity-*`，每个夹具有自有 target，子进程有期限，结束由 test cleanup 删除。用例包括独立计算资产 SHA 的 G8、固定 HEAD 的哈希漂移、生成文件漂移，以及缺失/陈旧/错配/格式损坏/树外 dep-info 和 Git 守卫；无需手写 Rust 宏解析器或 TS 金样。本合同验证 Rust 编译来源，未定义新的 TS 产品行为。

格式依据：[Cargo JSON messages](https://doc.rust-lang.org/cargo/reference/external-tools.html#json-messages)、[Cargo dep-info](https://doc.rust-lang.org/cargo/reference/build-cache.html#dep-info-files)；文件名处理按 [rustc `escape_dep_filename`](https://doc.rust-lang.org/nightly/nightly-rustc/src/rustc_interface/passes.rs.html) 及真实编译夹具核验（rustc 仅转义空格，`#` / `$` 保留为文件名字符）。

只启动自有 loopback 监听，隔离 HOME/PILOT_HOME/TMPDIR/数据库/工作目录，无模型调用或真实用户目录访问。就绪轮询有界，协议时序以真实 101 屏障、socket data 和实际异常为依据。子进程、stdio 与日志分别有界收尾；SIGINT/SIGTERM 进入同一个 finally 清理。

共 18 个样本，分三组保存：

- `results` 14 条：原 12 格（直连 TS / Rust→TS，各空/完整 masked ping/该 ping 三字节前缀 × 后来 hello 一段/两段写入），再加两侧 POST `/ws` 合法升级。透明 relay 扣住真实 TS 的 101，客户端完成后来 hello 写入后才放行。直连 TS 的实际 upgrade head 须逐字节等于初始 tail，Rust 上游 TS head 须为空；hello_ok、describe_server、FENCE pong 各一次，初始 ping 不得产生 pong。
- `routingResults` 2 条：两侧 GET `/ws` 带 Upgrade header、缺 Connection Upgrade token，记录完整普通 HTTP wire、状态、body、EOF，比较实际 HTTP 行为。
- `suffixResults` 2 条：每侧新建独立 baseline 进程、HOME 和数据库；只随 HTTP headers 发送 ping 的前三字节，收到真实 101 后只发送剩余 suffix。保存实际 TS head/data、客户端原始 wire、frames、基线 uncaughtExceptionMonitor 观察及主动清理前 socket/进程状态。observer 只记录异常，不接管基线的异常策略；不能把主动清理后的 close 冒充远端 EOF。

`check_suffix.mjs` 在录制器给出 PASS 前逐项比较清理前的 EOF/destroyed/closed、socket errors、基线 exitCode/signalCode；PID、堆栈不作相等条件。缺字段或单侧提前结束均 FAIL。`node --test src/pilotdeck-gateway/tests/upgrade/check_suffix.test.mjs` 有匹配正控与七个变异负控；checker 本身纳入运行前后输入 SHA。

`inputs.json` 保存 Node 版本/可执行文件、录制器、观察器、watcher、关键 TS/Rust 源码及协议 payload 原件的 SHA/hex。`report.json` 同时绑定各隔离目录中实际生成配置的前后 SHA、二进制与基线的前后 SHA、输入索引/观察记录/每个 child log 的 SHA。配置留在对应 `runtime/**/home/pilotdeck.yaml`，不依赖另一轮 A2 的配置。

**证据分工**：一次 TCP write 不等于一次 read。此 HTTP 入口实测不能独自证明 Rust 初始 read 所有权；后者由同 PR 的 `http_proxy_tests.rs` scripted AsyncRead、无读门阳性对照和生产 gate/pair helper 确定性测试证明。上游失败且初始 head 残留的两格也是候选自身生产路径断言：invalid WS bytes → HTTP 400 终止，完整普通 HTTP head → 既有 HTTP 处理；直连 TS 没有代理上游失败，不伪造 TS 故障对照。

验收原件须 `status=PASS`、14/2/2 个唯一 case、所有自有 child 已停、cleanupErrors 为空、全部前后 SHA 相同。已有原型/失败记录继续保留主仓 `_tmp/20261016-debt-cleanup/upgrade/ts-http-oracle/`；旧配置的追加 SHA 索引为相邻 `legacy-config-bindings.json`，不改写旧 report。本录制器不改变冻结尺子或判分语义，不登记新豁免。
