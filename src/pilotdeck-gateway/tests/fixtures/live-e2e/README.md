# live-e2e 端到端装置（#969，#748a）

真实 TS client → Python stdio MITM → 真实 TS sidecar CLI → loopback mock provider。
`read` 工具实际读取本格沙箱文件。基线固定 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，
Node 必须是 `v22.22.0`。本装置不代表 Rust 产品行为已经通过；#748b 复用这些基准。

## 运行

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
python3 -B src/pilotdeck-gateway/tests/fixtures/live-e2e/selftest.py <新证据目录>
python3 -B src/pilotdeck-gateway/tests/fixtures/live-e2e/attack_suite.py
python3 -B src/pilotdeck-gateway/tests/fixtures/live-e2e/test_harness.py
python3 -B src/pilotdeck-gateway/tests/fixtures/live-e2e/memory_controls.py
```

输出目录拒绝复用；`--cases text-drop …` 限制矩阵，三个 MITM 生命周期检查始终执行。
任一比较、运行或协议错误输出 `verified:false` 和专属 `#969` 消息，整轮非零。
所有进程使用本格 HOME，不读取/审计真实 `~/.pilotdeck`；拒绝端口 18789。

## 金样与原件

唯一行为期望是 `expected/<scenario>-<injection>.json` 的 12 格。
`generate.mts` 经 `capture.py` 真实运行 TS 生成，不读期望、不调用 verifier。
输出目录 `recursive:true`，JSON 文件 `flag:'wx'`；固定 Node 版本与基线提交、记录 TS 源哈希。
`oracle.json` 仅保留来源与每个 expected 文件的 SHA-256，不复制金样内容。

每格保留完整 `driver-result.json`、`frames.json`、`mitm-summary.json`、`mock.log`。
selftest 将这些原件原样组合，与期望逐层严格比较：类型、键集合、数组长度/顺序、所有叶值。
不先计数、投影或过滤；不从实际结果构造任何期望。期望里的 `<CASE>` / `<PORT>`
仅由调度器预先分配的路径/端口展开，实际结果不做字符串替换或端口掩码。
原件中伪造占位符和换成另一个合法端口均失败。

`frames.json` 是 MITM 在观察时直接记录的两个独立有序数组 `h2s` / `s2h`，
每方向有自己的连续 `index`，完整保存每帧内容、注入 action 与真实 delivered 值。
双工协议只承诺各方向的线序；不把调度器选择的跨方向交错冻结成业务判据。
`frames.jsonl` 仍保留按落盘时间的诊断流水，不是期望输入；被比较的原件为 `frames.json`。
`determinism.mjs` 在两个 TS 进程导入基线前固定 Date、Math.random、crypto.randomUUID；
真实 timers 仍运行，所有超时由单调时钟/真实定时器控制。

```sh
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/live-e2e/generate.mts <新生成目录>
# 按项目要求先 cargo-slots，并设置独立 CARGO_TARGET_DIR / 两个 DEBUG env。
cd src
cargo test -p pilotdeck-gateway --test live_e2e
cargo test -p pilotdeck-gateway --test live_e2e live_e2e_regenerates_identically -- --ignored --exact --nocapture
```

12 个 Rust 用例分别检查各文件与独立重生成清单的字节绑定；ignored 守卫重新实跑全部格，
比较新清单字节。篡改 expected 会被各格测试拒绝；同步改清单也会被真实重生成守卫拒绝。

## 状态 × 注入矩阵

| 状态 | passthrough | 第 2 event drop | 第 1 event duplicate | 第 2 event early-final |
|---|---|---|---|---|
| text | 9 事件，success/completed，1轮/2消息/1次模型 | expected 1, got 2；无终态 | expected 1, got 0；无终态/0次模型 | 2事件，success/mitm_premature_final，0消息 |
| tool | 20事件，success/completed，2轮/4消息/2次模型，read 真执行 | 同 text-drop | 同 text-duplicate | 同 text-early-final |
| cancel | 首个 model_event 后取消；3事件，aborted/aborted_streaming，mock aborted | 序号异常先于取消窗口 | 序号异常先于取消窗口 | 合成终态先于取消窗口 |

mock 用明确释放信号控制注入窗口：drop 发送 2 个 model event 后等待，early-final 发送 1 个后等待。
若关闭注入后客户端真的收到目标事件，driver 才释放 gate；正常错误/终态路径关闭连接释放 gate。
cancel 首事件后的 gate 由独立窗口记录与 abort 分支协调：取消时断流；未取消时释放，
mock 在最后事件后有界等待 1 秒再正常结束。gate 最迟 10 秒结束，pipe 空闲期限 30 秒，
driver watchdog 60 秒，外层进程 90 秒；各层故障不作为取消负控判杀。

取消负控必须证明：`cancelWindowEnteredAtEvents=2`、error=null、正常 success/completed 终态、
一次模型调用/完整 3 个事件、abort 未触发、model 未中断、mock 唯一请求 completed/sent=3；
且报告中命中取消专属断言。窗口外连接故障、缺证、watchdog、无关 panic 均 INVALID。
`LIVE_E2E_TEST_NO_ABORT=1` 是仅供负控的显式运行时变异；金样生成器拒绝该环境变量。

## 生命周期与负控

MITM 单独检查：宿主关 stdout 后仍记录；stdin EOF 后回收忽略 TERM 的子进程；
组长退出但后代持管道时按组升级 KILL。三个检查的读取与等待共用 15 秒 deadline。
`io_deadline.py` 使用非阻塞 fd + readiness + 单调 deadline；半行、无输出、背压都不能卡死。
`test_harness.py` 用真实管道覆盖这些情况、所有直接检查的 finally，以及窗口外故障拒绝。

`attack_suite.py` 使用真实金样的独立副本攻击 verifier，覆盖混类型、缺键/额外键、顺序/形状、
同步截短/追加数组、toolCalls 字符串、duplicate False、空/截短帧、空实际、篡改期望、
原件占位符/错误端口。变异负控绑定专属攻击名与结构化 caught=false，不能靠其他错误判杀。

```sh
# 要求 CARGO_TARGET_DIR，即便这组只改 Python/TS 装置；不创建第二份构建目录。
python3 -B src/pilotdeck-gateway/tests/fixtures/live-e2e/negative_controls.py <新目录> [变异名...]
```

源码变异必须等于 HEAD，经 `mutating_sources` 保存/恢复，每项先编译，再执行专属检查；
每格先跑正控，全部执行后汇总 KILLED / SURVIVED / INVALID。没有替代 baseline 或脏树绕过。
提交前可以只跑无源码写入的 `driver-abort-never-fires` 样本及 `memory_controls.py`；
后者编译所有 Python 变异，并实际杀死类型/键/长度/报告、管道 deadline、取消 error 守卫变异。
完整源码变异由协调者在提交后宿主执行。日志与最后汇总应一起保留，不把拒跑登记为通过。
