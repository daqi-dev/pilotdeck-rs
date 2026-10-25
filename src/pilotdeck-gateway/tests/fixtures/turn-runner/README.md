# #741 TurnRunner 探针与持久化端口

`oracle.json` 由固定基线 `cc7c4e6`、Node 22.22.0 的真实 TS 生成。`matrix` 保存真实 TurnRunner 事件与 JSONL；`recorderCases` 是 Rust 已实现的独立耐久完成闭环；`sidecar` 是真实 stdio AgentLoop + 确定性模型端口的观测字段。完整原始 wire（含实际注入后的帧）保存在每次生成目录的 `sidecar-*-raw.json`，未拿 Rust 输出生成期望。

**本切片不代表 #741 完成**：Rust 已有独立 `runtime/turn/runner.rs` 持久化端口，尚未接 LiveSessionAdapter / AgentLoop 宿主流，也没有放松 StreamIntegrity。TS native loop 的前置 abort 有终态，真实 sidecar 前置 abort 没有；TS 接受合法重排至 sequence 0 的 final，issue 要求 Rust 拒绝。这些实测事实留作接线依据，不把目标行为倒写成 TS 金样。

生成器直接导入真实 TurnRunner、AgentSessionEventRecorder、JSONL writer、AgentLoop；故障只注入 persistence append 观察者、确定性 UUID 源及 loop / wire 边界。循环模型与工具不访问网络；子进程 HOME 位于新生成目录。每个 sidecar 场景上限 20 秒，close 超过 1 秒升级终止；drop 等待真实 transport 完成回收。所有输出均独占创建，重复目录不得重用。

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/turn-runner/generate.mts /tmp/new-turn-runner-fixture
scripts/cargo-slots.sh
export CARGO_TARGET_DIR=/Users/da/ws/pilotdeck-rs/_build/wt-741b/_build/cargo-target
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway \
  --test recorder_completion -- --include-ignored
```

`recorder_completion.rs` 每格独立测试，逐字节比较 transcript；`turn_runner_regenerates_identically` 是显式 ignored 重生成守卫，实际运行会比较整个 oracle 字节并启动真实 TS sidecar。正常测试不要求 Node。

`negative_controls.py <新证据目录>` 覆盖 10 个源码变异，要求运行时断言失败，最后恢复正控。它使用共享 `mutating_sources/run_owned/wait_for_slot`，默认要求已提交、干净的产品源码。沙箱交付时使用证据目录里的隔离 Git tree 快照作为 `PDRS_NEGATIVE_BASELINE`；不创建 commit/ref，不改主仓 Git metadata。执行负控期间同树不得并发写入或运行 cargo。

## runner 独立端口（pr2 部分交付）

`start / accept / loop_failed / complete / fail` 消费并返回 runner，取消未结束的 transition 后不能重用原 runner。每次同步写入在 blocking worker 内执行，worker 保留真实 RuntimeLease。history 仅用于 loop；accepted event 仅原始 AgentInput；durable accepted 为原始输入加 synthetic，元数据由宿主预先按 TS acceptedInputMetadata 投影。字段、顺序、JSONL 原始字节由 runner_tests 对拍；它只比较 runner 负责的事件子序列，明确排除 hooks 的 user_prompt_submitted 与 loop 的 model_event，不冒充完整宿主序列验收。

新增六格真实 TS 探针：text/blocks + history/synthetic，accepted 验证失败、agent_aborted 异常、status 写失败、null userHint。保留原11格主矩阵、12格recorder及7格真实sidecar原值。accepted 验证异常的可见 Error 属性与 structuredClone 后的持久化形状不同，两者各自对拍。

```sh
scripts/cargo-slots.sh
export CARGO_TARGET_DIR=/Users/da/ws/pilotdeck-rs/_build/wt-741b/_build/cargo-target
PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck \
  cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway \
  --lib runtime::turn::runner::tests -- --include-ignored
python3 src/pilotdeck-gateway/tests/fixtures/turn-runner/runner_negative.py /tmp/new-runner-negative
```

runner 负控默认19项，可在输出目录后传具体变异名。与旧 recorder 负控一样使用共享保护器，不能在变异期间另起同树 cargo 或改源码。`test_sources` 仅在 cfg(test) 存在，让 recorder 与直接 writer 调用共用同一确定性来源。

待接续：LiveSessionAdapter 注入/接线、active slot 与不可取消 I/O 的共同所有权、AgentLoop-only integrity、真实 TS sidecar 的 Rust 宿主验收、旧 Python peer 与兄弟测试同步、已有可见 failure status 回调去重、hooks/steer/artifact/session 元数据收尾。取消/early-final 实测差异见 specs/gateway-parity/diffs/A7.2-TURN-{ABORT,EARLYFINAL}-001.md（未批准）。本切片不关闭 #741/#589。
