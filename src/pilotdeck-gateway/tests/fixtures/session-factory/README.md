# #745 SessionFactory 生命周期金样与负控

`oracle.json` 记录固定 TS commit / 源码闭包 SHA / Node v22.22.0、20 格生命周期矩阵、13 个完整投影探针和 3 个默认 storage hydrate 错误。原始 20 格探针来自协调者 `_tmp/20261028-issue-745-matrix/`；`probe.mts` 保留其操作和确定性门闩，基线/输出路径为参数；fix-r1 增加两个 writer 交错、仅内存状态覆盖探针。`normalize.mts` 只归一登记的临时路径、运行目录和耗时，`check.py` 先验证物理证据再归一。

`projections.mts` 调真实 AgentTranscriptProjections、AgentLoop.snapshotFileState、SessionRuntime / 默认 SessionProjectionDriver。原矩阵的 readFiles/writtenFiles 是测试注入的非生产形状；现已改成无 seed 时返回 `{}` 的 stdio 语义。`file-state.mts` 用真实 TS stdio、工具 checkpoint 与用户可见终态实测 initial/running/terminal/after，非空 SeedState 的字段与保留另由 Rust 用例对照。

以下命令从本树根运行；所有 cargo 使用调用方的同一 target，不建副本仓库或第二份构建目录。

```sh
export CARGO_TARGET_DIR=/Users/da/ws/pilotdeck-rs/_build/wt-745/_build/cargo-target
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
scripts/cargo-slots.sh
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib runtime::session::factory::tests -- --include-ignored --nocapture
python3 src/pilotdeck-gateway/tests/fixtures/session-factory/negative_controls.py --audit
```

60 项变异绑定完整 Rust 测试名和专属失败消息。先应用补丁并提交，使所有变异目标等于 HEAD；再由协调者在宿主运行长负控，OUT 必须为尚不存在的目录：

```sh
python3 src/pilotdeck-gateway/tests/fixtures/session-factory/negative_controls.py --compile "$OUT_COMPILE"
python3 src/pilotdeck-gateway/tests/fixtures/session-factory/negative_controls.py "$OUT_NEGATIVE"
```

可在 OUT 后加单个变异名（如 `same-key-cached`）。每项先跑正控、单独编译变异、运行精确被点名测试、还原并复跑；所有项目汇总后退出。编译失败、错断言、槽位/测试超时、源码不等于 HEAD 或恢复守卫失败均为 INVALID；任何非 KILLED 使全量失败。`--compile` 的 COMPILED 仅表示能编译，不表示被杀。等待槽位与单次进程均最多 900 秒；Rust 门闩最多 8 秒，超时不作为判杀证据。

状态差异在 #745 RESULT 中说明，尚未登记批准：Rust owner 按既定契约串行同 key 创建/替换等待；admission 前置读取错误主动关闭 storage，修复 TS 原路径的遗漏。不得把这些差异当已批准的对拍豁免。真实 provider/UI 实效不在本 fixture 的通过声明内。


`writerInterleavings` 是缺陷复现证据，不是安全验收：TS 两个时序都写出重复 seq，后续 resume 报 `sequence_not_increasing`；Rust 测试固定 read 前/后的交错并复现相同风险。Rust 本包 Native 无 host transport，测试先验证新 submit 到达该明确错误，再用同一新 handle 的真实 recorder append；TS 探针实际完成新一轮 submit。fence/drain 策略留给协调者决策，不把缺陷当成已获准的差异。

`regenerates_identically` 必须执行真实探针，生成器不再接受 `MATRIX_IMPORT_ROOT` 跳过采集。新增金样仍由同一个 `#[ignore]` 守卫逐字节重生成验证。`--audit` 只证明变异文本发生变化、测试存在、失败消息存在；语义非等价与判杀只能由编译和实际测试证明。
