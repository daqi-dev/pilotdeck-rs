# #741 TurnRunner 探针与 recorder 完成切片

`oracle.json` 由固定基线 `cc7c4e6`、Node 22.22.0 的真实 TS 生成。`matrix` 保存真实 TurnRunner 事件与 JSONL；`recorderCases` 是 Rust 已实现的独立耐久完成闭环；`sidecar` 是真实 stdio AgentLoop + 确定性模型端口的观测字段。完整原始 wire（含实际注入后的帧）保存在每次生成目录的 `sidecar-*-raw.json`，未拿 Rust 输出生成期望。

**本切片不代表 #741 完成**：Rust 尚未接 runner / accepted input / 宿主生命周期，也没有放松 StreamIntegrity。TS native loop 的前置 abort 有终态，真实 sidecar 前置 abort 没有；TS 接受合法重排至 sequence 0 的 final，issue 要求 Rust 拒绝。这些实测事实留作接线依据，不把目标行为倒写成 TS 金样。

生成器直接导入真实 TurnRunner、AgentSessionEventRecorder、JSONL writer、AgentLoop；故障只注入 persistence append 观察者和 loop / wire 边界。循环模型与工具不访问网络；子进程 HOME 位于新生成目录。每个 sidecar 场景上限 20 秒，close 超过 1 秒升级终止；drop 等待真实 transport 完成回收。所有输出均独占创建，重复目录不得重用。

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/turn-runner/generate.mts /tmp/new-turn-runner-fixture
scripts/cargo-slots.sh
export CARGO_TARGET_DIR=/Users/da/ws/pilotdeck-rs/_build/wt-741/_build/cargo-target
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway \
  --test recorder_completion -- --include-ignored
```

`recorder_completion.rs` 每格独立测试，逐字节比较 transcript；`turn_runner_regenerates_identically` 是显式 ignored 重生成守卫，实际运行会比较整个 oracle 字节并启动真实 TS sidecar。正常测试不要求 Node。

`negative_controls.py <新证据目录>` 覆盖 10 个源码变异，要求运行时断言失败，最后恢复正控。它使用共享 `mutating_sources/run_owned/wait_for_slot`，默认要求已提交、干净的产品源码。沙箱交付时使用证据目录里的隔离 Git tree 快照作为 `PDRS_NEGATIVE_BASELINE`；不创建 commit/ref，不改主仓 Git metadata。执行负控期间同树不得并发写入或运行 cargo。
