# Live Recording 所有权矩阵

固定 cc7c4e6：真实 AgentSession / TurnRunner / recorder / SessionRuntime / JsonlPersistence / AgentHandle / AgentFactoryProvider；仅 AgentLoop 为 stub。生成器哈希递归运行依赖，输出 exclusive create。每格用 subscriber Promise gate 挂住 start/accept/complete，真实 factory dispose 和 return 清理均有界。

Rust 12 格在生产 LiveSessionAdapter + blocking TranscriptStore.append 上验证。cancel 对应丢掉 next 借用后重新 next；JS 则取消等待者后等待同一个 Promise。drop 对应 TS 排队 return 的资源不变量，不是析构 API 等价。dispose 在 Rust 组合 LiveSession.dispose 的取消/拒绝入口与 ProjectRuntimeRegistry.dispose 的实际资源回收；不声称已测完整 factory 自动接线。Rust 消费 active/writes/event/replacementStarted；其他金样字段保留 TS 原始观察，Rust 自身独立断言 attempt、pending、租约和 dispose。

金样 complete 的 afterWriteActive=1，而 Rust 终态交付即释放活动槽，属既有行为，见 `specs/gateway-parity/diffs/A7.2-LIVE-TERMINAL-SLOT-001.md`（未批准）。失败注入三例仅验证新 transition 接线不会把 runner 的失败回执当成功；不是失败事件转发或完整失败门禁验收。

重生成：设置 PDRS_BASELINE，运行 generate.mts <新目录>；ignored Rust `live_recording_regenerates_identically` 必须字节一致。

#996 起本夹具的负控是编译期开关（`crate::mutants::on`，仅 `--features
pdrs-mutants` 的库测试二进制可激活），由 `schemata.py` 声明并经共享执行器
`_shared/mutant_switch.py` 预编译后逐行执行（正控 → 变异 → 还原）；旧
`negative.py` 走 legacy `run_cases`/`classify` 判杀（具名 FAILED 行 + 绑定
测试自身断言消息——旧基线的 reason 与无 receipt 目录为证），与共享执行器
默认判杀一致，无需 receipt 协议。旧文本替换脚本已删除，迁移对账见各轮
RESULT。所有 I/O gate 显式释放；Node 5s、Rust next/资源回收15s、
blocking gate10s 超时只判测试失败，绝不当作产品解锁条件。

```sh
export CARGO_TARGET_DIR=/absolute/target PDRS_BASELINE=/absolute/PilotDeck
python3 -I src/pilotdeck-gateway/tests/fixtures/live-recording/schemata.py \
  _tmp/<date>-issue-996/negative-live-recording   # 22 行，全部必须 KILLED
# 只读锚点预检：--check
```

二十二行 = 旧 `negative.py` 的 16 静态行 + 6 生成行（payload-fields 六行照
生成展开写入表）：准入两连（disposed/active 各自忽略拒判）、start 失败不
传播、共享 recorder lease 在 cleanup 报错、integrity 失败终态报 success
（accept/complete 两行同一变位点，各自开关名）、真实/worker 槽位与租约
释放序、waiter 拿走 pending transition、pending payload 与三段 payload
字段的取消/重提交对、终态释放 runner、accepted 门与三处 dispatch 交换。

