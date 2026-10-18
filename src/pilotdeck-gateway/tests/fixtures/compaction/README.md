# Compaction persistence TS fixtures

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。被测 TS 入口：`src/agent/session/AgentSessionEventRecorder.ts` 的 `recordCompactionStarted` / `recordCompactionCompleted` / `recordCompactionFailed` / `deferCompactionCompletion` / `commitDeferredCompaction` / `failDeferredCompaction`、`JsonlTranscriptWriter.recordControlBoundary` / `recordCompactionReplacement`，以及 `src/agent/modules/transport/sidecarDefaultModuleDispatcher.ts` 的 `dispatchTurn` `persist_compaction` 分支。同批源码（含 `durableContextRuntime.ts`、`TurnRunner.ts`、writer/runtime/域校验/JSONL 持久化）经 import 传递闭包的 SHA-256 与整棵 `src` 的来源守卫结果记在 `oracle.json`。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/compaction/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/compaction
cd src
cargo test --locked --offline -p pilotdeck-gateway --test compaction
```

生成器先跑 `tests/baseline_source_guard.mjs` 的 `bindSourceTree`：固定 Git 绝对路径、HEAD 等于 `cc7c4e6`、已跟踪树无改动、整棵 `src` 逐 blob 比对；再按 import 传递闭包复算 `sourceSha256`。每次运行在输出目录下 `mkdtemp` 一个 `.generate-*` 临时目录，只清理本次创建的目录；transcript 落盘行、返回值与错误都来自真实 TS，随机 UUID/场景目录归一化后写入 `oracle.json`；生成器连跑两次逐字节一致（UUID 归一化后）。

`oracle.json.scenarios[]`（6 组，Rust 逐场景重放步骤结果与整份 transcript 文件字节）：

| 场景 | 覆盖 |
|---|---|
| lifecycle | `nextCompactionOperationId`；started（含 `maxContextTokens`）、completed（skipped 与 compacted/带 tier/compactionId/error）、failed；多条 bracket 交错 |
| compacted-replacement | `durableContextRuntime` 的 compacted 分支顺序：started -> 延迟 completion；`TurnRunner` 的 durable 分支：一个 `control_boundary` 原子 replacement（`snapshot.version=1` + 完整 messages）-> commit 写 `compaction_completed` |
| deferred-failure | started -> 延迟 completion -> `failDeferredCompaction` 写 `compaction_failed`；随后的 commit 是无操作 |
| control-boundaries | microcompact / resume / manual / 无 snapshot 的 compact boundary：writer 原样落盘，不补字段 |
| errors | 无 pending 的 defer；无 bracket 的 completed/failed；重复 started；非法 messageCount/maxContextTokens/missing；空 error；error=null 的 V8 TypeError；全部拒绝且不发布 sequence |
| restore | 重开并 `SessionRuntime.restore` 后：pending compaction 清空、commit 无操作、域里仍开着的 bracket 可以正常 completed，第二次 completed 报 `invalid_compaction`；序号/父链连续 |

`oracle.json.turn[]`（18 组，Rust 直接调用 `host::modules::turn::Handler`）：

- 无 `onCompactPersisted` 端口 -> `Host turn capability does not support persist_compaction.`；
- `boundary` 缺失/null/数组/字符串/数字，或 `messages` 缺失/对象/字符串/null/数字 -> `Compaction persistence requires boundary and messages.`，回调不执行；
- 合法输入回调收到原样 boundary/messages，响应 `{persisted:true}`；额外 payload 字段被忽略；
- 回调抛 `disk full` -> 错误原样上抛；
- 缺失/空/非字符串 operation -> dispatcher 字段错误文案。

`TurnRunner.ts:338-374` 的 durable 分支在 Rust 侧由 `turn::persist_compaction_to_recorder` 表达：compact boundary 写 replacement 后 commit；非 compact boundary 或写失败先 `failDeferredCompaction` 再返回错误。TS 的 catch 还会 abort 外层 loop（不在宿主层范围）；Rust 不返回 `{persisted:true}` 假成功，而把错误交给调用方，避免未持久化的压缩面被送进模型。该差异在 `tests/compaction.rs::unsupported_boundary_closes_deferred_bracket` 与 RESULT 中登记。

范围：只证明 compaction 生命周期与 control boundary 的 JSONL 落盘格式、域校验和 `persist_compaction` 分派；`durableContextRuntime` / `TurnRunner` 的调度本身、`try_auto_compact` 预算注入、question/permission/inbox 生命周期与 `completeTurn` 留给后续 PR。
