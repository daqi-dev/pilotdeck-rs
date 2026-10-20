# Compaction engine / manual host oracle

固定 TS 基线：`cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。`record.mts` 直接执行
`CompactionEngine`、`stripMultimediaFromMessages`、预算投影、`DefaultContextRuntime`、
`createDurableContextRuntime` 和真实 transcript writer；模型是固定流式 mock，
未复制 TS 实现，也不调用外部模型。

共 75 个案例：46 engine、1 media strip、7 truncate、6 budget projection、
12 manual durable、3 configured budget。回放精确断言条数，以保序 JSON 字节比较
请求、结果、生命周期、事件，以原始 JSONL 字节比较替换前后的 transcript。
冷却使用固定时钟；entry / operation / compaction 各有独立确定性 ID。
中止案例覆盖默认 reason、显式字符串 reason、空字符串 reason、流中中止，以及冷却期内的
第二次调用（其错误文本必须仍是首次中止的文本）；manual durable 的
`abort-throw` 案例锁定 failed 记录里的同一文本。

运行：

```sh
cd src
CARGO_HOME=/Users/da/ws/pilotdeck-rs/_build/cargo-home \
CARGO_TARGET_DIR=/Users/da/ws/pilotdeck-rs/_build/wt-a63-pr1/src/target \
cargo test --locked --offline -p pilotdeck-gateway --test compaction_engine
```

重生成由 `generate.mts` 调用共享 `bindSourceTree`（全 src 干净树与固定提交）
及 `sourceClosureSha256`（650 文件 import 闭包），并断言 Node 22 / en-US。
Rust 的 ignored regeneration 测试在两个独占 `Scratch` 子目录运行生成器，
比较两次输出及本 oracle，随后由共享 Scratch owner guard 清理。
单独运行 Node 生成器时保留 `.run-*`，不自行删除任何目录。

```sh
# 共享守卫内部使用只读 Git；须在本次任务的禁止 Git 限制解除后由协调者运行。
PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck \
PDRS_GIT=/usr/bin/git LANG=en_US.UTF-8 \
CARGO_HOME=/Users/da/ws/pilotdeck-rs/_build/cargo-home \
CARGO_TARGET_DIR=/Users/da/ws/pilotdeck-rs/_build/wt-a63-pr1/src/target \
cargo test --locked --offline -p pilotdeck-gateway --test compaction_engine \
generator_twice_is_byte_identical -- --ignored
```

本次受“不执行任何 git 命令”限制，oracle 先由同一个 `record.mts` 真实录制两次，
逐字节一致。录制前逐项核对了已入库 `compaction/oracle.json` 的全部 650 个
guard-bound SHA-256；独立枚举确认新生成器的完整相对 import 闭包正是该集合。
没有以这项复用核验冒充共享守卫的本次执行：固定 HEAD / 全 src 干净树仍待协调者
跑上面的 guarded regeneration。原始命令、探针、输出和两处实现变异负控由协调者归档。

手动端覆盖本次单次 full-summary 适配及 durable bracket；自动策略、
post-summary snip / emergency / overflow 的完整编排由 pr8b 接续。
这些高压路径不能由本 oracle 的低压手动案例推定通过。
