# 工具运行时场景夹具

`oracle.json` 来自固定 TS 基线（`cc7c4e6`）真实运行：用确定性的假工具驱动真实的 `ToolRegistry`、`ToolRuntime`、`ConcurrentToolScheduler` / `SequentialToolScheduler` 与 `toolSchedulerAdapter`，用虚拟时钟录下结果、事件与拒绝消息。`sourceSha256` 覆盖这五个入口的相对 import 闭包。

```sh
PDRS_BASELINE=/absolute/fixed/PilotDeck node --import /absolute/fixed/PilotDeck/node_modules/tsx/dist/loader.mjs src/pilotdeck-gateway/tests/fixtures/tool-runtime/generate.mts /absolute/output-parent
```

每次运行在输出父目录下用 `mkdtemp` 新建独占目录，`flag:'wx'` 写入。连续两次输出字节一致才更新正本；正本只追加场景。

场景字段：`schemaJson` / `inputJson` 是双方都按 JSON 文本解析的原始输入（保留自有 `__proto__` 键和未排序的整数键）；`schemaPrototypes` 在解析后给指定路径的对象设显式原型（null、数组或对象），模拟程序化或合并出的 inputSchema。

## 有意偏离一：并发分组只收只读工具

TS `ConcurrentToolScheduler` 只看 `isConcurrencySafe`；基线的 `agent`（`agent.ts:166-167`：`isReadOnly` 为 false、`isConcurrencySafe` 为 true）以及配置描述符造出的同类工具都会进并发阶段。Rust 的并发分组条件是 `isConcurrencySafe && isReadOnly`（只对 concurrencySafe 的工具再问 isReadOnly，抛错按非只读处理），这类工具改到串行阶段。

理由：并发阶段出错时 batch 会丢弃兄弟（见偏离二），只有并发组里确实全是只读工具，才能保证不会截断写入副作用。

可观察影响：并行度降低，事件交错与 `startedAt`/`completedAt` 不同；没有错误时结果内容与副作用不变。并发组里另一个工具出错时，TS 中的这类工具在后台跑完，Rust 中它在串行阶段，根本不会启动（不会有执行一半的写入）。

场景 `non-readonly-safe-tools-run-concurrently-in-ts` 与 `non-readonly-safe-tool-outlives-concurrent-error-in-ts` 标了 `rustSerializesNonReadOnly`。它们不进通用回放，由 `non_readonly_safe_tools_run_sequentially` 显式断言：结果去掉时间戳后与 TS 相同；TS 四个调用都在 t=0 启动，Rust 的事件序列逐条写死；出错场景的拒绝时刻与 TS 相同，且 agent 调用从未启动。其它场景的对比没有放宽。

## 有意偏离二：并发阶段首个错误

场景 `concurrent-error-lets-started-siblings-finish`（`drain: true`）录下了 TS 行为：`Promise.all` 在 `settledAt` 立即拒绝，已启动的兄弟调用在后台跑完（`settledAt` 之后的 `end` 事件）。

Rust 的拒绝时刻与 TS 一致；出错那一轮仍把每个兄弟 poll 一次，然后 batch 丢弃它们，不让它们继续跑。理由：

- 按偏离一，并发组只有只读工具，丢弃不会截断写入；
- 端口契约不允许脱离所有权的任务；若等兄弟跑完再报错，一个不响应取消的兄弟会让 batch 一直挂到外层 deadline，而 TS 已经返回。

回放对该场景断言：Rust 返回时刻等于 TS `settledAt`；Rust 事件等于 TS 在 `settledAt` 及之前的事件；TS 之后只多出兄弟的 `end`；没有残留的工具 future。`concurrent_error_rejects_immediately_and_drops_started_siblings` 另用两个永不结束的兄弟钉住同一行为。
