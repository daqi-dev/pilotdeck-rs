# Provider cancellation timing fixture

`oracle.json` 保存固定 TS 基线真实执行的 196 格原值：四协议 × headers / body / response_text / count × fulfilled / rejected / idle × 续体先 / 父先 / 同刻父先注册 / 同刻父后注册，另加四协议同栈拒绝并取消。每格保留 timer 注册顺序、Node 实际 `_idleStart + _idleTimeout`、事件、错误 message 和 RetryProgress。重试上限为 1，delay 为 100ms，让错误之后多出的重试可见。

复采命令（在本目录执行）：

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" probe.mts
```

探针只注入 fetch / Google SDK promise 与 iterator；Google 非 2xx body 使用真实 SDK，body 的独立 undici timeout 用 `TypeError: terminated` timer 注入，不访问网络或用户配置。原件的 Node 版本和 TS 源码 SHA 在 fixture 顶部。

所有定时器均为 Node 原生 timer；busy wait 令双方 overdue，精确同刻必须核对实际 deadline 与注册顺序。跨 tick 的候选丢弃后重跑。OpenAI chat / Google 的 count 格实际不发 provider 请求，保留 `reachable: false` 与本地估算原值。

`observable.name` 对抛出错误记录其类型；错误事件记录规范错误类型（自定义 stream error 的 own name 也保留）。Error 原型上的非枚举 name 另存 `rawErrorName`，不把 JSON 中的 `{}` 推断成丢失的异常类型。count 入口吞掉异常后仅暴露 estimatorError，故 name 为 null。`http_timing_tests.rs` 一张表逐格核对 name、message、交付形式和 RetryProgress 数；count 还核对 source / tokens。
