# Host module TS fixtures

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。`oracle.json` 保存源码 SHA-256、863 个路由响应、11 个错误投影及 90 个字段校验结果；每条预期由真实 TS 执行产生。Node 22、tsx 4.22.3 与其 esbuild 使用基线已有依赖，不安装新包。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/host-modules/generate.mts src/pilotdeck-gateway/tests/fixtures/host-modules
cd src
cargo test --locked --offline -p pilotdeck-gateway host::tests::modules
```

生成器直接 import `createSidecarDefaultModuleDispatcher` 与原生 `createHostPlanTodoModuleHandler`（resolve 返回 undefined，表达未接 plan/todo 端口）。client 的 `SidecarTurnProtocol` 与 dispatcher 五个字段助手未导出，因此只在内存追加 export，经基线 tsx 的 esbuild 转为 ESM；相对依赖重定位至原基线文件，不修改基线、不复制其实现到仓库。`moduleResponseFor` 实际调用、缓存、回放同一请求；UUID 注入为 call.messageId，故响应 ID 可与 Rust 的稳定 ID 逐值比较。执行前检查基线 commit 和三个源文件无工作区差异。

路由矩阵为 13 个 module × 65 个 operation：operation 包含 `src/config/contracts.json` 六份契约的全部 59 个不同方法名（含 get_metadata），再加未知/空/错型/空白/孤立 UTF-16 六种边界。另有 model 三条字段负例、其余七个已注册 module 各两条缺 preparationId/缺 operation 负例，以及缺 preparationId 的 get_metadata，共 18 条。真实 TS 表明只有 model 的非 metadata 路径前置校验 preparationId；其他七个 module 的 unknown operation 不要求该字段，但缺 operation 仍先报字段错误。

X0 使用无已准备模型、无流、空工具列表、无 metadata/plan-todo/context execution 端口的运行时探针；context execution 缺省，而非假装有端口的空对象。TS 类型声明要求 context 存在时提供 execution，此处刻意探测运行时缺省分支，不代表完整合法宿主组合验收。真实分支给出 model metadata 不可用、无效 canonical request、未知 preparation、capability 必填字段及 plan/todo 不可用错误。close_stream 对不存在的流返回 closed，list_tools 返回空数组，均由真实 dispatcher 执行。完整模型请求、工具执行、规划及上下文业务适配仍属 A6.4–A6.7；这些金样不表示业务端口已实现。

checkpoint/skills/knowledge 属于协议允许但 dispatcher 未注册的 module，覆盖 client 兜底文案。Python 夹具只拒绝 unknown 与孤立代理 module，其他 module 全部发送；过滤后必须等于 oracle 的 `stdioCounts`（routing 733 / errors 11），并核对被滤集合、分区总数和 messageId 唯一性。未知协议 module 的金样仅直接调用 client 私有路由入口。

错误覆盖缺省 code、显式 code、空串 code、带额外字段的 canonical、无效 canonical、false retryable、负数/零/小数 retryAfterMs、非有限数字被省略、孤立 UTF-16 字符。Rust `ModuleError::from_js_fields(message, fields)` 从 JS 值提取 code/retryable/canonical/delay，丢弃类型不符的字段；wire 投影再校验 canonical.code/message 和有限数。错误回放先恢复非有限数，再调用这一产品 API，测试自身不筛除错类型。字段助手分别覆盖整个 record 缺省、对象缺键、显式 undefined 和 null；两个 optional 助手对前两种字段缺省及 undefined 返回缺省，对 null 报错，不做 trim 或数值字符串转换。生成器打印两个 optional 助手的缺键/undefined/null 六条真实执行结果。

Rust 默认测试回放整个金样；Python `module-routing`/`module-errors` mode 经真实 stdio 每批最多并发 16 条请求，逐值核对响应和关联 ID，避免金样扩容填满管道或 callback 队列。dispose 的完成/拒绝/协议错/断线/终态遗留回调/提前取消/外层 future drop 由 Rust 对抗测试覆盖；另覆盖普通线程上 drop 已 poll 的 execute，以及重连中 pending/cached/write-failure 模块回调与排队事件，断言每轮 dispose 一次且调用时活动回调归零。

本地资源边界：Rust 清理在所有 callback future 真正退出后启动，每轮恰好一次；close 失败也清理。callback 退出依赖协作式取消，close_timeout 只限制 dispose 本身；dispose 的异步错误和 panic 不改写已知轮结果。TS dispatcher 对 iterator.return 的异步 rejection 使用 allSettled，但没有此超时，且同步 throw 仍会外抛；Rust 的边界是有意的宿主清理隔离。已 poll 的外层 future drop 后使用创建 TurnScope 时捕获的 Tokio runtime 调度清理；未 poll 的 execute 不会创建 TurnScope 或触发 dispose，也不保证进程/runtime 强制结束后的异步工作。
