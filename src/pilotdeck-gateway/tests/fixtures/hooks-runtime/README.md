# HookRuntime / executors / lifecycle TS fixtures

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。所有期望值由真实 TS 产生（`tests/fixtures/hooks-runtime/generate.mts`），无实现源码复制。

生成（仓库根目录，Node v22.22.0，基线 lockfile 的 tsx）：

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/hooks-runtime/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/hooks-runtime
```

生成器先接 #280 共享溯源守卫 `tests/baseline_source_guard.mjs` 的 `bindSourceTree`（显式绝对 Git、HEAD==`cc7c4e6`、已跟踪文件干净、逐 blob 对照）；`oracle.json.sourceSha256` 记录本夹具依赖的 25 个 TS 文件（逐文件对照 `git show`）。`Date` 被固定在 `2026-01-01T00:00:00.000Z`，`pendingAsyncHooks[].startedAt` 因此不随运行变化。

- `parse`：52 个 `parseHookOutput(stdout)` 用例，覆盖首个 `{` 行的 JSON 解析/非法 JSON 立即停止、JSON 原语/数组、`async` 判定、`asyncTimeout` 的 ceil/有限/正数规则、`hookSpecificOutput` 全字段、非法子对象、`decision` allow/deny、重复键、整数键与 `__proto__`。
- `command`：15 个真实子进程用例。脚本写入本次 `mkdtemp` 树，`scripts` 记录脚本字节；覆盖 stdin（hookInput JSON）、exit 0/1/2/3、signal、非法 UTF-8、CRLF、无效 JSON 后置行、500 ms 超时、运行前/运行中 abort、env 值、shell 参数展开。
- `http`：12 个本地回环 mock 用例。真实 `fetch` 与 mock 记录同一请求；`request` 只记 `content-type`/`x-a..x-d`（`host`、`user-agent` 等由客户端自动生成的头不跨实现比较），body 完整记录。覆盖 200/204/400/500、非法 UTF-8、header 环境变量替换（未 allow 的变量必须为空）、非字符串/非法 header 值、非法 URL、预中止、运行中中止、连接拒绝。`<url>`/`<refused>` 是回放时替换的占位符。
- `callback`：21 个注入 handler 用例。覆盖 trim/空名、重复注册的 generation 与旧 registration 失效、string/output/void/Promise/throw Error/throw string、dispose 后 execute/register、dispose 等待在途回调。
- `runtime`：主 settings 的 5 次 `HookRuntime.run`（顺序与事件、matcher/condition 过滤、pluginName 前缀、prompt/agent 未配置、订阅者抛错不影响执行），另有 `dispose` 后错误、`pluginRoot` 覆盖 cwd、SessionEnd 1500 ms 超时、pending async hook 的 describe/complete/collect/remove/cancel 与 dispose 后 complete/collect、callback hook 经 HookRuntime 的注入路径、dispose 排空在途 run。
- `lifecycle`：真实 `LifecycleRuntime.dispatch` 的 effects/messages/events/errors/`pendingAsyncHooks` 序列化与 field 顺序；dispose 后错误；`NullLifecycleRuntime` 空结果；`HostLifecycleRuntime` 的请求形状、`parseDispatchResult` 校验与 `moduleFailure` 错误投影；`buildTurnEnvironment` 六个用例（ASCII、全角 NFKC 近似、相对 cwd、`..` 归一、超长/空段）。
- `scripts`：command/runtime/lifecycle 用例共同引用的脚本字节；Rust 回放写入自己的 scratch，不共享生成器目录。

回放：`tests/hooks_runtime.rs`。每次用例运行独占 `tests/common/scratch.rs` 临时目录，结果按 stringify 逐字节比较；`<dir>`/`<cwd>` 由回放侧替换。负控包括：超时后进程组 leader 必须已被回收（`timed_out_command_leaves_no_process`）、未 allow 的环境变量不得进入 header、`complete(output=async)` 必须返回 false、非法 dispatch result 必须拒绝、callback dispose 后 handler 不得再执行。
