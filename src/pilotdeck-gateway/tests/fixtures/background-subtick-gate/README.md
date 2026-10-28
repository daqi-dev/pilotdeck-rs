# #957 subTick 完成门闩

原 `background-tasks` 的 sdk-projections step 30/32 分别等待 20 ms，ignore-abort 任务体在 50 ms 后自动完成；任务回放被调度推迟时，step 32 就读到 completed，违背已录金样的 running。本补丁不改变产品与原 oracle，不放宽逐步 JSON 字节比较。

`background_tasks.rs::SubagentGate` 延用 #956 CreateGate 的「到达门闩 → 比较快照 → 显式释放」协议；进程内任务用已有 oneshot，原生 shell 继续用原 CreateGate。subTick 等实际进入事件；fail / 已释放任务另等 ended_at 登记，再做原严格比较。ignore-abort 只在 SDK stop 提交取消后显式释放。轮询中的 10 ms 只控制检查频率，8 秒仅为失败看门狗，经过时间不会释放门闩。

Node v22.22.0 探针调用固定 TS 的真实 BackgroundSubagentRuntime 与 ProjectRuntimeRegistry.stopBackgroundTaskForSdk（只替换项目 resolve 接缝），用 Promise 进入/释放握手记录四种行为：

| 行为 | 释放/停止前 | 事件后 |
|---|---|---|
| release | running、无 ended；前驱不完成时一直 pending | release → completed；SDK stop false |
| fail | failed、boom、有 ended | SDK stop false，仍 failed |
| block | running、无 ended | SDK stop → cancelled，true |
| ignore-abort | running；取消后 cancelled，但 stop/body 仍 pending | release → stop true，状态仍 cancelled，无 error |

金样不含墙钟、UUID；生成器要求固定 Node/TS 版本、源码 SHA 校验、递归创建输出目录、`flag: 'wx'` 独占写文件。重生成守卫逐字节比较金样。

```sh
export CARGO_TARGET_DIR=/absolute/wt-957/_build/cargo-target
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
export PDRS_BASELINE=/absolute/fixed/PilotDeck
scripts/cargo-slots.sh
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway --test background_tasks
scripts/cargo-slots.sh
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway --test background_tasks subagent_gate_regenerates_identically -- --exact --ignored
```

## 负控

提交候选后运行（输出目录必须不存在）：

```sh
PYTHONDONTWRITEBYTECODE=1 python3 src/pilotdeck-gateway/tests/fixtures/background-subtick-gate/negative_controls.py /absolute/evidence/negative
```

也可用 `--compile-only OUT` 只验证每个变异可编译，或在 OUT 后传变异名做样本。脚本调用共享 `negative_process` 的 HEAD 守卫、进程归属与槽位等待，使用 `negative_suite` 的具名测试判分；不接受未设的 CARGO_TARGET_DIR，不建立第二套 target 或影子仓库。每项均先正控、应用变异、no-run 编译、具名测试、恢复后正控；全部跑完才汇总，过期锚点、编译失败、错位 panic 与恢复失败均 INVALID，任一非 KILLED 非零退出。

| 变异 | 绑定测试 / 失败点 |
|---|---|
| remove-body-gate | holds_until_release：首次 poll 必须 Pending；删除等待必为 Ready |
| finish-on-cancel | holds_until_release：取消后 body 仍须 Pending |
| skip-entry-handshake | lifecycle：必须捕获真实运行信号 |
| wrong-failure | lifecycle：真实 failed/boom 投影逐字比较 |
| skip-cancel-release | sdk_projections_replay：SDK stop 的有界完成 |
| skip-explicit-release | holds_until_release：显式释放后的有界完成 |
| missing-matrix-behavior | lifecycle：四种行为的精确清单 |

新源码未在 HEAD 时脚本会拒绝变异；协调者提交后再跑，拒绝不算 KILLED。原 #956 六项负控的共享消息/锚点同步添加 #957，行为和绑定保持不变。
