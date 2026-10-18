# Host module TS fixtures

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。`oracle.json` 保存源码 SHA-256、81 个路由响应、11 个错误投影及 90 个字段校验结果；每条预期由真实 TS 执行产生。Node 22、tsx 4.22.3 与其 esbuild 使用基线已有依赖，不安装新包。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/host-modules/generate.mts src/pilotdeck-gateway/tests/fixtures/host-modules
cd src
cargo test --locked --offline -p pilotdeck-gateway host::tests::modules
```

生成器直接 import `createSidecarDefaultModuleDispatcher`。client 的 `SidecarTurnProtocol` 与 dispatcher 五个字段助手未导出，因此只在内存追加 export，经基线 tsx 的 esbuild 转为 ESM；相对依赖重定位至原基线文件，不修改基线、不复制其实现到仓库。`moduleResponseFor` 实际调用、缓存、回放同一请求；UUID 注入为 call.messageId，故响应 ID 可与 Rust 的稳定 ID 逐值比较。执行前检查基线 commit 和两个源文件无工作区差异。

路由覆盖八个已注册 module 的未知/空/错型/空白/孤立 UTF-16 operation、model preparationId 校验顺序，以及 checkpoint/skills/knowledge 的 client 兜底文案。后三者属于协议允许但 dispatcher 未注册的 module，可经真实 stdio 到达 client 兜底；Python 夹具回放全部 11 个合法 module。任意不在协议名单中的 module（unknown、孤立代理字符）会先被协议层拒绝，其 unsupported 金样仅直接调用 client 私有路由入口。X0 所有业务操作仍未实现，均返回所在 module 的 unsupported 文案；本夹具不声称完成 A6.4–A6.7 业务适配。

错误覆盖缺省 code、显式 code、空串 code、带额外字段的 canonical、无效 canonical、false retryable、负数/零/小数 retryAfterMs、非有限数字被省略、孤立 UTF-16 字符。Rust `ModuleError::from_js_fields(message, fields)` 从 JS 值提取 code/retryable/canonical/delay，丢弃类型不符的字段；wire 投影再校验 canonical.code/message 和有限数。错误回放先恢复非有限数，再调用这一产品 API，测试自身不筛除错类型。字段助手分别覆盖整个 record 缺省、对象缺键、显式 undefined 和 null；两个 optional 助手对前两种字段缺省及 undefined 返回缺省，对 null 报错，不做 trim 或数值字符串转换。生成器打印两个 optional 助手的缺键/undefined/null 六条真实执行结果。

Rust 默认测试回放整个金样；Python `module-routing`/`module-errors` mode 经真实 stdio 并发发多条请求，逐值核对响应和关联 ID。dispose 的完成/拒绝/协议错/断线/终态遗留回调/提前取消/外层 future drop 由 Rust 对抗测试覆盖。

本地资源边界：Rust 清理在所有 callback future 真正退出后启动，每轮恰好一次；close 失败也清理。dispose 的异步错误和 panic 不改写已知轮结果，以 close_timeout 为清理上限。TS dispatcher 对 iterator.return 的异步 rejection 使用 allSettled，但没有此超时，且同步 throw 仍会外抛；Rust 的边界是有意的宿主清理隔离。外层 future drop 后在仍运行的 Tokio runtime 调度清理，不保证进程/runtime 强制结束后的异步工作。
