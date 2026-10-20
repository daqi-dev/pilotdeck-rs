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
- `command`：27 个真实子进程用例。脚本写入本次 `mkdtemp` 树，`scripts` 记录脚本字节；覆盖 stdin（hookInput JSON）、exit 0/1/2/3、signal、非法 UTF-8、CRLF、无效 JSON 后置行、500 ms 超时、运行前/运行中 abort、env 值、shell 参数展开；另有 >64 KiB 的 stdout/stderr（管道并发读取）、leader 先退出但孙进程持 stdout 的超时/abort、孙进程忽略 TERM（3 s 宽限后 SIGKILL）、正常完成但浮动孙进程短暂持管道（close 等 EOF）、以及 TERM handler 需要 1 s 清理的 timeout/abort（`markerFile`/`markerWaitMs` 在结果返回后核标记，证明 3 s 宽限没有被提前的 SIGKILL 截断），以及 `PILOTDECK_SHELL_PATH` 空串回落与裸名 PATH 查找。`background-survives-success` 由脚本把 `sleep 30` 的 PID 写入 `<dir>/bg.pid`：TS 成功后 500 ms 后台进程必须仍存活，生成器核存活后再自行 SIGKILL；回放同样核存活并由测试清理。
- `http`：27 个本地回环 mock 用例。真实 `fetch` 与 mock 记录同一请求；`request` 只记 `content-type`/`x-a..x-d`（`host`、`user-agent` 等由客户端自动生成的头不跨实现比较），body 完整记录。覆盖 200/204/400/500、非法 UTF-8、header 环境变量替换（未 allow 的变量必须为空）、非字符串/非法 header 值、非法 URL、预中止、运行中中止、连接拒绝；另加 latin1 header 字节（`José` → E9）、CJK ByteString 错误，以及预中止 + 非法 header 时 header 校验先于 abort；另有大小写不同的同名头（`Content-Type` 与默认 `content-type`、`X-A` 与 `x-a`，Headers 按大小写不敏感合并成一条、`, ` 连接）与完全同名覆盖。`request.headerLines` 逐条记录选定头的每一行原始值（`req.headers` 会折叠重复头，单看它测不出拆成两行的情况）。另有 `appendHeader` 语义 9 例：值先去首尾 HTTP 空白（空格/`\t`/`\r`/`\n`）再校验、合并；只有 NUL/CR/LF 在构造 Headers 时报错（早于 abort 检查），0x01/0x7f 等到发送才失败为 `fetch failed`（预中止时报 abort）。回放侧 hyper 服务端解析时也会去掉单个值的首尾空白，所以单值 trim 只由合并用例 `header-trim-join` 判出（负控实测）。
- `prompt`：3 个 `PromptHookExecutor` 用例（evaluator 回显 prompt），`$ARGUMENTS` 只替换首处且替换串走 GetSubstitution（`$&`、`` $` ``、`$'`、`$$`；`$1`/`$<n>`/`${x}` 原样）。
- `callback`：21 个注入 handler 用例。覆盖 trim/空名、重复注册的 generation 与旧 registration 失效、string/output/void/Promise/throw Error/throw string、dispose 后 execute/register、dispose 等待在途回调。
- `runtime`：主 settings 的 5 次 `HookRuntime.run`（顺序与事件、matcher/condition 过滤、pluginName 前缀、prompt/agent 未配置、订阅者抛错不影响执行），另有 `dispose` 后错误、`pluginRoot` 覆盖 cwd、SessionEnd 1500 ms 超时、pending async hook 的 describe/complete/collect/remove/cancel 与 dispose 后 complete/collect、callback hook 经 HookRuntime 的注入路径、dispose 排空在途 run。
- `lifecycle`：真实 `LifecycleRuntime.dispatch` 的 effects/messages/events/errors/`pendingAsyncHooks` 序列化与 field 顺序；dispose 后错误；`NullLifecycleRuntime` 空结果；`acrossTurns` 在同一会话 runtime 上连续两次 dispatch（第二轮仍执行 hook，pending async 编号跨轮递增）；`HostLifecycleRuntime` 的请求形状、`parseDispatchResult` 校验与 `moduleFailure` 错误投影；`buildTurnEnvironment` 六个用例（ASCII、全角 NFKC 近似、相对 cwd、`..` 归一、超长/空段）。
- `scripts`：command/runtime/lifecycle 用例共同引用的脚本字节；Rust 回放写入自己的 scratch，不共享生成器目录。

回放：`tests/hooks_runtime.rs`。每次用例运行独占 `tests/common/scratch.rs` 临时目录，结果按 stringify 逐字节比较；`<dir>`/`<cwd>` 由回放侧替换。负控包括：超时后进程组 leader 在 3 s 宽限结束、延迟 KILL 后必须已被回收（`timed_out_command_leaves_no_process`）、宽限内后代必须存活且延迟 KILL 必须到达（`timeout_result_returns_before_the_group_kill_grace`）、孙进程持管道/忽略 TERM/>64 KiB 输出（真实 TS 金样）、成功后后台进程必须存活且由测试清理（`background-survives-success`）、stdin writer 必须在收尾时中止（`command_cleanup_aborts_stdin_writer`）、HTTP header 超时投影为 `fetch failed`、未 allow 的环境变量不得进入 header、`complete(output=async)` 必须返回 false、非法 dispatch result 必须拒绝、callback dispose 后 handler 不得再执行、Handler 每轮 dispose 后第二轮仍可执行且 pending/callback 跨轮保留。
