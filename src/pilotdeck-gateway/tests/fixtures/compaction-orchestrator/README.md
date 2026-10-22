# Compaction policy / orchestrator / recovery oracle

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。`record.mts` 执行真实
AutoCompactionPolicy、CompactionOrchestrator（经 DefaultContextRuntime）、
ContextOverflowRecovery、CompactionEngine、MicroCompactionEngine、SnipEngine、
host dispatcher、durable context 和 transcript writer。辅助模型为固定流式 mock，
预算为显式序列；期望值由 TS 实际运行生成。

| 分组 | 精确条数 |
| --- | ---: |
| policyCases / policyBudgetCases | 4 / 4 |
| recoveryCases / fallbackCases / malformedRecoveryCases | 41 / 20 / 3 |
| orchestrationCases | 35 |
| identityCases | 2 |
| hostCases | 6 |
| loopCases | 2 |

编排覆盖 warning/blocking、micro 改写及无改写、缺预算/策略/摘要、手动强制、
摘要失败、post-summary snip（含非消息预算）、target-not-reached、emergency 的
额外预算评估/reactive summary/micro/head truncate、最终 overflow、NaN、定制
truncator 的引用变化，以及 micro 后 ratio 恰为 0.90、最终 ratio 恰为 0.90
（tier 为 emergency）和 1.0 的边界。
`orchestrationCases` 比较调用顺序、摘要请求、结果、提交前后
JSONL 原始字节；两条 recovery 串联案例仅验证决策，不冒充模型重试。

`loop.mts` 的两条案例运行真实 TS AgentLoop：第一次主模型返回 context_overflow，
真实 recovery 决策触发真实 CompactionEngine，随后实际发起第二次主模型调用。
第二次分别成功与 authentication 错误终止。Rust `compaction_recovery_loop.rs`
通过已有 transport 的 OriginalAgentFactory / ExecutionServer 驱动生产 turn runner，
接入本次 gateway context 实现；交换通道全部在内存中，无监听端口、无新增依赖。
比较真实主模型/摘要调用次数、完整阶段顺序、事件类型序列、最终结果与持久化回调次数。
随机 blockId 不属于该观测投影；此处没有排序或重排观测字节。

`admissionProbe` 录制真实 TS AgentHandle 第二次手动 compact 的拒绝。
Rust `compaction_maintenance_reentry_matches_real_ts` 在同层
`LiveSessionHandle::begin_maintenance` 回放拒绝文本，并验证维护/turn 相斥、
正常与错误释放后的再次接纳。锁不属于 stage orchestrator。

回放：

```sh
cd src
CARGO_HOME=/Users/da/ws/pilotdeck-rs/_build/cargo-home \
CARGO_TARGET_DIR=/Users/da/ws/pilotdeck-rs/_build/wt-a6-recorder/src/target \
cargo test --locked --offline -p pilotdeck-gateway \
  --test compaction_orchestrator --test compaction_recovery_loop
```

生成器调用共享 `bindSourceTree` 断言全 src 干净树，调用
`sourceClosureSha256` 绑定 650 文件 import 闭包，断言 Node 22 / en-US。
每次 Node 调用创建独占 `.run-*`，自身不删除目录。ignored 测试复用
`tests/common/scratch.rs::Scratch` 的归属检查清理，两次生成输出必须与入库
oracle 逐字节一致；所有回放先调用 `baseline::oracle(ORACLE)`。

```sh
# 共享守卫内部调用 Git；本任务禁止 Git，须由协调者在允许的环境执行。
cd src
PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck \
PDRS_GIT=/usr/bin/git LANG=en_US.UTF-8 \
CARGO_HOME=/Users/da/ws/pilotdeck-rs/_build/cargo-home \
CARGO_TARGET_DIR=/Users/da/ws/pilotdeck-rs/_build/wt-a6-recorder/src/target \
cargo test --locked --offline -p pilotdeck-gateway --test compaction_orchestrator \
  generator_twice_is_byte_identical -- --ignored
```

本轮受“不得执行任何 Git 命令”约束，使用同一 record / loop 函数实际录制两次，
逐字节一致；650 项源码哈希与 pr8a 已有 oracle 全部吻合。此核验不替代本次
固定 HEAD / 全 src 干净树断言。完整命令、原始输出、运行时探针、变异负控、
沙箱限制及协调者接续项见主仓
[`_tmp/20261020-a66-pr8b/RESULT.md`](/Users/da/ws/pilotdeck-rs/_tmp/20261020-a66-pr8b/RESULT.md)。
