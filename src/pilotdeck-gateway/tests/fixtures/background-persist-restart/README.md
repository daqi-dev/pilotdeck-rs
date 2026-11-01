# #1233 dispose 冲刷与重启恢复金样

基线为 PilotDeck `cc7c4e6`，Node v22.22.0 实测三遍（`persist-restart-run1/2/3.out`；
fix-r1 追加的四格损坏采样一遍）。十三个 case 覆盖：终态任务重启恢复（含输出）、
崩溃 running 任务以 unknown 恢复、spill 写失败后重启、dispose 冲刷拒绝（`flush`
返回 `#1233 flush refused`）、dispose 冲刷同步 throw、重启长度不匹配（`has 7
bytes; expected 6`）、重启输出缺失、settle 正常、settle 写失败（completionEvents
空、diagnostics `#1233 settle write refused`）、以及 state.json 损坏四格（坏
JSON、version 2、重复 taskId、非法 status，各只留一个缺陷、其余字段全合法）。
Rust 侧对应 `tests/background_persist.rs`，`background_persist_restart_regenerates_identically`
守卫比较整个 oracle 字节。settle 写失败仍通知、hasDurableOutput 只认实际落盘字节、
start 二次持久化、损坏拒绝两处文案差异均为**已登记未批准**差异，见
`specs/gateway-parity/diffs/A6.5-BACKGROUND-PERSIST-001-R2.md` 与
`-R3.md`。

## 负控

须先提交本单文件，源文件和执行器均由 HEAD 守卫验证。调用方必须提供自己的
`CARGO_TARGET_DIR`；不接受替代 baseline 或私建仓库。

```sh
export CARGO_TARGET_DIR=<协调者专用构建目录>
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
python3 -I src/pilotdeck-gateway/tests/fixtures/background-persist-restart/negative_controls.py --check
python3 -I src/pilotdeck-gateway/tests/fixtures/background-persist-restart/negative_controls.py --compile-only <新证据目录>
python3 -I src/pilotdeck-gateway/tests/fixtures/background-persist-restart/negative_controls.py <另一新证据目录>
```

17 个变异分别覆盖：冲刷错误被吞掉、失败后仍进 Disposed、拒绝未 memoize、resources
dispose 隐藏冲刷拒绝、损坏输出变软恢复、不可恢复任务被跳过、恢复退回信任 persisted
标志、append 不再持久化、终态任务一律改写 unknown、output.rs 中
`has_durable_output` 直接返回 true，以及 fix-r1 新增的七条：store 丢弃规则取反、
append/settle 序号失效（两条）、损坏四缺陷逐一软化。行内第六字段是 cargo 目标
（`background_persist` 或 `lib`）。每项先编译，再以准确测试名和 `#1233` 消息判杀，
再恢复并跑正控；锚点过期、编译失败、错误 panic、超时或恢复失败均为 INVALID。始终汇总
所有选中项，任一非 KILLED 即非零退出。`--compile-only` 只证明编译，不能代替判杀。

负控只能在待测源码提交后由受保护 runner 执行（HEAD 守卫拒绝未提交树）。上一轮
（提交 `345f56878`）的宿主判杀证据评审时未回收，记「待协调者核」；本轮 fix-r1 的
七条新变异已在本工作树逐条实测（编译通过 + 点名测试按专属消息 KILLED），完整
runner 判杀与汇总同样交协调者提交后执行。原始证据与验收缺口见本树
`_tmp/20261026-issue-1233/`。
