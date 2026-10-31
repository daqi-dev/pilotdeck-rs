# #1011 投影缓存第一阶段金样

固定 `OpenBMB/PilotDeck@cc7c4e6`，Node **v22.22.0**。只交缓存矩阵与原件，Rust 产品接线留待协调者评审。

`generate.mts` 直接 import 固定基线的 `ProjectSessionStorage`、`SessionRuntime`、projection driver、`ProjectSessionFactory`、`SessionRouter`。模型、工具、MCP 没有外部调用；project/home 在输出目录的独占 `work-*` 内。没有改 TS 原件。`oracle.json` 包含完整来源闭包 SHA256、30 场景 × before/held/settled、15 个边界探针（含 4 个首次 factory load）、原始 transcript/cache UTF-8 字节及 SHA256、方法调用与 hydrate 参数。原件嵌在金样中，全部受逐字节重生成守卫约束；没有时钟/路径掩码。

状态为 `missing/fresh/stale/corrupt/concurrent`。日志有两个 metadata 事件，fresh 缓存到 2，stale 缓存到 1；corrupt 是 `{broken\n`。concurrent 的第一个追加卡在 `node:fs/promises.appendFile`，此时原生 JSONL writeTail 已登记，第二个进入真实 runtime 队列。仅门控本格独占 transcript 路径的磁盘追加；没有在 provider.append 入队前人为暂停。metadata 的投影输出大小固定。

事件映射：load = `storage.restore()`；append = `events.append()`；recreate = `router.markSessionDirty/getOrCreate`，进入真实 factory；checkpoint = `projectionCheckpointBinding.flush()`；drain = `storage.flush()`（存储 drain，不把它冒称 handle 的 active turn drain）；shutdown = `router.shutdown()`。每场景先用真实 factory 建好活动 handle，load 是显式再 restore。所有快照都是只读观测，不调用 flush。

非 concurrent 格 held 停在 cache.load（load/recreate）、disk.append（append）、cache.save（checkpoint/shutdown）、log.flush（drain）。concurrent 格 held 保持首 append 阻塞：load 已进入 persistence.load 并等 writeTail；recreate 已进入旧 storage dispose；checkpoint 已进入 runtime.read；drain/shutdown 已进入 runtime.flush。pending 前驱格另有真实缓存读取作为进展见证；取消的是等待方 Promise，storage 没有 cancellation API。每个门闩/主操作/播种/清理等待上限 10 秒，生成器总上限 120 秒；pending 观测后释放门闩仅为清理，不能据此宣称不完成的前驱会自己结束。

实测事实：普通 append / restore 不写 cache；合法过期前缀重放后缀，损坏缓存回退日志；turn_result 自动调度写；flush/dispose 写；保存失败与三次 capture 冲突均 fail-soft。**并发 recreate 的新 handle 停在序号 2，而旧 storage 完成到 4**，这是基线风险，不能作为 Rust 对齐通过判据。Rust 第二阶段须沿用已批安全差异并补相应测试。

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
node --disable-warning=ExperimentalWarning --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/session-factory/projection-cache/generate.mts /path/to/new-output
export CARGO_TARGET_DIR=/Users/da/ws/pilotdeck-rs/_build/wt-1011/_build/cargo-target
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
scripts/cargo-slots.sh
cd src
cargo test --offline -p pilotdeck-gateway --lib \
  runtime::session::factory::tests::projection_cache_regenerates_identically -- --exact --ignored --nocapture
```

生成器使用 recursive mkdir 与 `flag:'wx'` 独占输出，重复 OUT 拒绝，写 oracle 前只删除本次自建的 `work-*` 根（外来的 `work-*` 不动）。ignored guard 的临时目录由既有共享 helper 管理。配对负控见 `negative_controls.py`，用共享 `negative_process` / `negative_suite`，先编译变异再运行正控→负控→恢复正控。新增文件和修改测试尚未进入 HEAD 时禁止运行；协调者提交后在宿主设置上述环境并传独占 OUT。`--audit` 仅静态锚点检查。

supplemental 行有三种形状：`cold-*` 是顶层 before/held/settled；`invalid-*`、`turn-result`、`save-reject`、`capture-race`、`predecessor-reject` 只有 before/settled；`predecessor-pending`/`cancel-waiter` 把 held 观测嵌在 `details.held`（cancel-waiter 另有 `details.waiter`）。Rust 消费方（`projection_cache_tests.rs`）按各自行形状读取，不改 oracle 判定内容。

第二阶段已接线：`runtime/session/projection_cache.rs` 提供投影检查点与 FIFO 存储队列，`factory.rs` 的持久会话投影接入可丢弃 checkpoint，`projection_cache_tests.rs` 逐格断言 oracle。产品负控覆盖缓存过期不刷新、损坏缓存不拒绝、并发丢条、turn_result 不调度、capture 只试一次、生成器遗留与 owner、守卫清单缺 lib.rs；ignored 基准 `projection_cache_append_benchmark` 量 metadata append 的 O(n²) 修复（不进 CI 判定）。**缩围说明：Rust 检查点只落 2/13 行（`agent-transcript.metadata`、`agent-transcript.turn-summary`，见 `envelope()`），conversation 恒走日志重放，其余 11 行未注册投影不伪造空缓存，冷启动重放**（已按 plan 预注册）；矩阵判据对齐的是投影值、持久日志、writer 序号与缓存字节/水位，不是 TS 判定表的全部字段。

fix-r1：检查点读取复用 transcript 侧 50MiB 上限（`crate::storage::TRANSCRIPT_MAX_BYTES`，超限视为缺失走重放，差异登记见 `specs/gateway-parity/diffs/A7.2-SESSION-PROJECTION-CHECKPOINT-BOUND-001.md`）；`fresh-*`/`stale-*`/`cold-fresh`/`cold-stale` 断言 `hydrated_rows == 2`、`invalid-codec` 断言 2（codec 行单独回退），负控相应补 `oversized-checkpoint-accepted`、`hydrate-branch-removed`、`envelope-over-rejected`。
