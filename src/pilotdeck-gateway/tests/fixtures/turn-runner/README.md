# #741 TurnRunner 探针与持久化端口

`oracle.json` 由固定基线 `cc7c4e6`、Node 22.22.0 的真实 TS 生成。`matrix` 保存真实 TurnRunner 事件与 JSONL；`recorderCases` 是 Rust 已实现的独立耐久完成闭环；`sidecar` 是真实 stdio AgentLoop + 确定性模型端口的观测字段。完整原始 wire（含实际注入后的帧）保存在每次生成目录的 `sidecar-*-raw.json`，未拿 Rust 输出生成期望。

**本切片不代表 #741 完成**：Rust 已有独立 `runtime/turn/runner.rs` 持久化端口，LiveSessionAdapter 已接原始输入与共享 Recorder 注入，尚未持有 Recording / 接 AgentLoop 宿主生命周期，也没有放松 StreamIntegrity。TS native loop 的前置 abort 有终态，真实 sidecar 前置 abort 没有；TS 接受合法重排至 sequence 0 的 final，issue 要求 Rust 拒绝。这些实测事实留作接线依据，不把目标行为倒写成 TS 金样。

生成器直接导入真实 TurnRunner、AgentSessionEventRecorder、JSONL writer、AgentLoop；故障只注入 persistence append 观察者、确定性 UUID 源及 loop / wire 边界。循环模型与工具不访问网络；子进程 HOME 位于新生成目录。每个 sidecar 场景上限 20 秒，close 超过 1 秒升级终止；drop 等待真实 transport 完成回收。所有输出均独占创建，重复目录不得重用。

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/turn-runner/generate.mts /tmp/new-turn-runner-fixture
scripts/cargo-slots.sh
export CARGO_TARGET_DIR=/Users/da/ws/pilotdeck-rs/_build/wt-741c/_build/cargo-target
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
export CARGO_TARGET_DIR=/Users/da/ws/pilotdeck-rs/_build/wt-741c/_build/cargo-target
PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck \
  cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway \
  --lib runtime::turn::runner::tests -- --include-ignored
python3 src/pilotdeck-gateway/tests/fixtures/turn-runner/runner_negative.py /tmp/new-runner-negative
```

runner 负控默认25项（两个 terminal 分支分别变异，每个锚点 count == 1），可在输出目录后传具体变异名。与旧 recorder 负控一样使用共享保护器，不能在变异期间另起同树 cargo 或改源码。`test_sources` 仅在 cfg(test) 存在，让 recorder 与直接 writer 调用共用同一确定性来源。

待接续：LiveTurnStream 持有 Recording、active slot 与不可取消 I/O 的共同所有权、AgentLoop-only integrity、真实 TS sidecar 的 Rust 宿主验收、旧 Python peer 与兄弟测试同步、已有可见 failure status 回调去重、hooks/steer/artifact/session 元数据收尾。取消/early-final 实测差异见 specs/gateway-parity/diffs/A7.2-TURN-{ABORT,EARLYFINAL}-001.md（未批准）。本切片不关闭 #741/#589。

## fix-r1 消费账本

`matrix` 的以下 14 行由 `runtime/turn/runner_tests.rs` 的非 ignore 用例消费；断言仅覆盖 runner 所有的事件子序列、错误与 JSONL 字节（两条 input 还比较 loopMessages），lease 另行断言。`loopCalls/modelCalls/returns/completes/closes/writeAttempts` 仍是 TS 观测记录，完整宿主计数由 pr3 同名场景接手。所有 `live_*` 名称均为 pr3 待实现的接手用例，不代表本包已有该测试。

| 节 | 行 | 当前消费 / 接手用例 |
|---|---|---|
| matrix | `success` | pr2 已消费：`runner_success` |
| matrix | `start-fails` | pr2 已消费：`runner_start_write_failure` |
| matrix | `accepted-fails` | pr2 已消费：`runner_accepted_write_failure` |
| matrix | `abort-before-execute` | 仅记录，由 pr3 消费：`live_runner_abort_before_execute` |
| matrix | `abort-after-accepted` | 仅记录，由 pr3 消费：`live_runner_abort_after_accepted` |
| matrix | `loop-fails-twice` | pr2 已消费：`runner_repeated_loop_failure` |
| matrix | `loop-throws` | pr2 已消费：`runner_loop_exception` |
| matrix | `early-completed` | 仅记录，由 pr3 消费：`live_runner_early_completed` |
| matrix | `consumer-drop` | pr2 已消费：`runner_consumer_drop` |
| matrix | `complete-once` | pr2 已消费：`runner_completion_retry` |
| matrix | `complete-always` | pr2 已消费：`runner_completion_persistent_failure` |
| matrix | `input-text` | pr2 已消费：`runner_input_text_boundaries` |
| matrix | `input-blocks` | pr2 已消费：`runner_input_blocks_boundaries` |
| matrix | `accepted-duplicate` | pr2 已消费：`runner_accepted_validation_error` |
| matrix | `loop-aborted-error` | pr2 已消费：`runner_aborted_exception` |
| matrix | `status-fails` | pr2 已消费：`runner_best_effort_status` |
| matrix | `loop-null-hint` | pr2 已消费：`runner_nullish_hint` |
| sidecar | `success` | 仅记录，由 pr3 消费：`live_sidecar_success` |
| sidecar | `gap` | 仅记录，由 pr3 消费：`live_sidecar_gap` |
| sidecar | `duplicate` | 仅记录，由 pr3 消费：`live_sidecar_duplicate` |
| sidecar | `early-final` | 仅记录，由 pr3 消费：`live_sidecar_early_final` |
| sidecar | `drop` | 仅记录，由 pr3 消费：`live_sidecar_drop` |
| sidecar | `abort-before` | 仅记录，由 pr3 消费：`live_sidecar_abort_before` |
| sidecar | `abort-after` | 仅记录，由 pr3 消费：`live_sidecar_abort_after` |

以上 pr3 行在 oracle 内也逐行标注「仅记录，由 pr3 消费」。sidecar 的 gap/duplicate/early-final 与 abort 仍须遵守 StreamIntegrity 及已登记差异的审批状态；本轮只登记接手测试，不更改接受行为。

`sourceSha256` 从真实导入入口（包括 `cli/pilotdeck-agent-loop-sidecar.ts` 与 `permission/index.ts`）递归计算运行时 import/export/literal import() 的相对依赖闭包，当前为 455 个源码；TypeScript 转译先消除 type-only 引用，排序后逐文件对固定 commit 校验。唯一非字面量动态导入是 CLI 的外部 factory，探针子进程仅传 PATH/HOME、未设置 factory 环境变量，故走已纳入闭包的默认 factory。node 内建与 npm 包不属于 sourceSha256，使用固定基线的本地安装。

`recorderCases` 新增 null/undefined/number × fresh/started/completed 九格真实 TS 金样。null/undefined 在去重前抛 TypeError；数字在完成标记为 undefined 时 no-op，已有完成标记时抛 `Durable turn undefined has not started.`；这些调用都不追加 JSONL，started 格随后成功完成证明可重试。Rust 保留显式 session/turn 参数的既有对象调用合同，仅对齐本轮非对象边界。

两套负控先定位函数作用域，再替换不带前导缩进、不跨行的短语句锚点，每个锚点严格 `count == 1`；recorder 10 项、runner 20 项，均要求运行时断言失败且恢复后正控通过。

## pr3 独立端口（未完成 LiveSession 接线）

`TurnInput::from_agent_input` 消费原始 text/blocks，分开接收宿主 history/synthetic，按真实 TS `acceptedInputMetadata` 投影选项。新增八行 `input-metadata-*` / `input-text-original` 金样由 `recording_metadata_*` / `recording_original_text` 消费；既有17行 matrix、21行 recorder、7行 sidecar 和 sourceSha256 原值保持。metadata 在 writer 归一化前也对拍，不能用 writer 丢弃空对象掩盖投影差异。

`Recording` 将 transition future 留在自身，`settle()` 被取消后可继续同一次迁移；在 pending 或完成后拒绝 `begin()`。丢弃驱动时，已进入同步 I/O 的 worker 继续持有传入的共享 owner 和 RuntimeLease。调用方须把真实 active-slot guard 传给 `Recording::new`；目前 LiveSession 还没有调用该端口，不能把本地 guard 析构实验称为端到端 slot 验收。永久卡住的同步 I/O 不承诺强杀或提前释放 owner。

`runner_tests` 的现有 oracle 重放经 Recording 驱动全部五种 action；六个独立 gate 用例覆盖 start/accept/complete 中取消 waiter 后继续以及直接 drop。gate entered/Disposed 上限5秒，阻塞 I/O gate 上限10秒；用未放行 gate 和单次 poll 覆盖前驱持续 pending、第二个 waiter 被取消、禁止覆盖 pending。另测未 poll drop、失败后的 closed 状态、非法 AgentInput 与 modelSelection prototype。

`recording_negative.py <新证据目录>` 使用同一三个公共 helper，对这些新分支逐项变异，要求运行时 RED 与恢复 PASS。原 runner/recorder 负控仍适用且锚点未改。重生成继续使用已存在的 ignored `runner_regenerates_identically`，须显式运行；生成器仍 recursive mkdir + `flag:'wx'`。

尚未完成的完整接线范围仍为上面“待接续”及本包 RESULT 的最终清单；所有 live_* 用例仍是接手要求，不因新增端口而改成已验收。

## #741 送审前 fix-r2

新增 `invalid-result-{turn,session,number,array,null}-{idle,step}` 十格真实 TurnRunner 探针；step 格在 loop 内调用真实 recorder.recordModelRequest 后返回异常 result，原输入保存在 rawResult。turn/null 四格由同名 runner 用例逐字节对拍；session/number/array 六格按 A7.2-TURN-INVALID-RESULT-001 验证 Rust 错误终态与关闭，不冒充 TS 一致。每格都有独立 Rust 用例，invalid-result-early-return 负控必须同时击中全部十格。

`loop-{missing,undefined,null}-message` 三格记录 TS JSON 结果及 statusTextOwn/statusTextUndefined。TS live 对象有 text:undefined，但 wire JSON 省略；Rust agent_status 按 wire 形状省略 text，保留 null。failureMessageUndefined 标记让 Rust 回放恢复真实 Undefined 输入，turn_failed.error 保留原始输入；detail 的 undefined 字段同样省略。

三套负控共用 `_negative_common.py` 的 scoped/unscoped mutate 与有界 cargo run，仍各自使用 negative_process 的 mutating_sources；所有变异要求运行时断言失败，恢复正控后才记录通过。当前 runner 负控 25 项，recorder 10 项，recording 31 项。使用本树 target：`CARGO_TARGET_DIR=/Users/da/ws/pilotdeck-rs/_build/wt-741c/_build/cargo-target`。既有 ignored `runner_regenerates_identically` 覆盖扩展后的完整 fixture。

## pr4 独立修复（完整接线留 pr5）

新增32个 malformed result 金样格（每种 idle/step）和3个 loop_failed 后 complete 再失败格。TS catch 确实重复写第二条 status，Rust 保留该行为；Undefined 和非字符串 turnId 文案逐字节对拍，缺身份/非法 session 的关闭策略见 A7.2-TURN-INVALID-RESULT-001-R1。字段级 Undefined sessionId、prototype/getter、任意组合不宣称已逐格验证。旧金样原值保留。

真实 sidecar 新增 dispose-before 行（连接/模型调用为0）；live 的预取消 factory 用例消费 abort-before/dispose-before 的连接数，使用两种真实 adapter 构造器核 factory 零调用。另测 pending factory 的 abort/dispose/drop，以及两次取消 next 借用后继续同一前驱。这里只对齐“不得创建连接”，不声称 Rust synthesized aborted terminal 与 TS 无终态一致。旧 Python peer 协议未改，StreamIntegrity 未放松。

四套负控都要求调用方显式提供 CARGO_TARGET_DIR，不能覆盖为别树 target。runner 31项；新 live_negative.py 3项（eager factory、factory 优先、忽略 pending cancel）。它们使用 mutating_sources/run_owned/wait_for_slot，未提交的新源码遇到 baseline 拒绝须交协调者提交后运行，不建立副本仓库或第二构建目录。既有 ignored runner_regenerates_identically 覆盖新金样并需显式运行。

## pr5a 输入与共享 Recorder（按 500 行规则仅交付第 1 条）

`LiveSessionAdapter::new/with_interaction` 显式接收会话的 `Arc<Recorder>` 与当前 history 读取函数，factory 接收同一个 Arc，组装 model/tool durable ports 时必须复用它。submit 接原始 text/blocks；执行配置与 syntheticMessages 来自 options，history→accepted→synthetic 的组装由 TurnInput 共用。`input-synthetic-null` 是真实 TS 新行，原有行与 sourceSha256 保持，已有 ignored `runner_regenerates_identically` 覆盖重生成。

`input_negative.py` 将12个源码变异逐项绑定测试名和断言，使用公共 mutating_sources/run_owned/wait_for_slot，要求调用方设置 CARGO_TARGET_DIR。旧 live factory 负控参数锚点及 recording text-value 作用域已同步。第 2 条 Recording/真实 active slot 共持与 I/O pending 矩阵仍未交付，不能用输入测试或 runner 测试 guard 代替。


## pr5d AgentLoop 流完整性

真实 sidecar 首帧是 model_request_started；StreamIntegrity 不再要求宿主 start/accept。success/max_turns 需要 loop 前缀，error 需要 loop 前缀及 failure；收到/交付计数不等时非 aborted final 失败。HostClient 保留 sequence 严格校验和 wire 非final completed 抑制，绕过这一层泄漏到 session 的 completed 仍失败。

`sidecar` 的 success/completed-warning 行由 `live_stream_tests.rs` 真实 Node 进程测试逐项比较 `hostEvents`。此投影仅覆盖宿主 lifecycle + 全部 loop，排除 pr5e 的 session 外壳/setup/hooks；原始完整事件仍在生成目录。gap/duplicate 比较错误终态与收尾尾序列，不声称已交付前缀完全一致；其摘要排除与错误终止竞态的 instructions_loaded，完整 raw 不过滤。early-final 保留 TS success 原值，Rust拒绝，见 A7.2-TURN-EARLYFINAL-001-R1。

真实测试（ignored，显式执行）使用 `PDRS_BASELINE` 与 Node 22.22.0；过滤 `runtime::turn::live::stream_tests::real_ts_sidecar` 并加 `--include-ignored --test-threads=1`。`stream_integrity_regenerates_identically` 覆盖整个 turn-runner 金样。`sidecar.mts` 的直接执行模式是 Rust用的透明 stdio代理，只变异传输帧，真实 AgentLoop与确定性Rust model port不被替换。错误探针、正常流和completed后的warning都保留独占wire记录。
