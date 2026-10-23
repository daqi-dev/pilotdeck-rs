# Host module handler binding TS fixtures

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。`oracle.json` 保存六份源码的 SHA-256、27 个配置组合，以及每个组合的（a）manifest 广告集合 `dispatcher.manifest.hostModules`、（b）实际可调用 handler 列表（`Object.keys` 于 dispatcher 的 handler 注册表）、（c）12 个探针 `module_call` 的真实 TS 响应。生成器只用真实 TS 构造器：`createSidecarDefaultModuleDispatcher`、`createDefaultSidecarTurnComposition`（其 `forTurn` 恰如客户端那样绑定每轮的 `planTodoHandler` 与 `capabilityResultObserver`，agentLoopSidecarClient.ts:341-354）、`SidecarTurnProtocol`（经 esbuild `export {}` 暴露私有符号，原文原位编译）；不修改、不复制基线实现。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --disable-warning=ExperimentalWarning --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/host-module-binding/generate.mts [outdir]
cd src
cargo test --locked --offline -p pilotdeck-gateway host::tests::binding
```

生成器断言基线 HEAD 等于固定 commit，且 `git status --porcelain --untracked-files=no` 为空；`sourceSha256` 与 `git show <commit>:<path>` 的 blob 哈希核对。写入先落临时目录再复制；`#[ignore]` 测试会把生成器重跑到独有临时目录并逐字节比较（两次生成运行字节一致，sha256 见 RESULT）。

覆盖范围（27 组合 × 12 探针）：

- 组合维度：model 的 `materializeRequest`/`metadata`；budget 缺席/无方法/三个方法两两组合；turn 三个回调独立与全开；permission/lifecycle/event 各自装/不装；context 缺席 / 装了没有 execution port（`no-runtime`，handler 注册但零广告）/ 四个可选方法任意组合；plan_todo 广告开（`unresolved`：`forSession` 返回 undefined）或关；`mixed-*` 与 `full-composition` 交叉组合。
- 生成器内先做 TS 侧不变量断言：每个被广告的模块都有注册 handler，且注册表不超出路由集合。
- 探针：8 个路由模块各发一次未知 operation（注册的模块给各自的 does-not-support 文本，未注册的给 `Unsupported sidecar host module: X`，先于任何 payload 解析——对齐 agentLoopSidecarClient.ts:1155-1158 的 handler 查找顺序）；4 个 plan_todo 探针钉住两个独立开关：`read`（resolving 时 `{"snapshot":{"items":[]}}`）、缺 `sessionId`（`Missing sessionId.`，在 resolve 之后才校验）、错会话身份（`Plan/todo capability identity does not match the active host turn.`）、未知 method（`Unsupported sidecar plan/todo method: unknown`）。
- 每个响应经真实 `SidecarTurnProtocol.moduleResponseFor` 投影（`uuid: () => request.messageId`），并断言同请求重放一致（`replayed` 标记）。

Rust 回放（`src/host/tests/binding.rs`）：按每个组合的 spec 重建 `ModuleRouter` 的真实 handler 绑定，逐项核对（1）`missing_handlers` 为空（广告 ⊆ 实际 handler）、（2）`callable_modules` 与 TS 注册表逐序相等、（3）`turn_manifest` 合成的 `hostModules` 与金样逐字节相等、（4）全部 324 个探针经 `module_response` 的完整响应逐字节相等。另有两个负控方向（广告无 handler 即拒绝合成 manifest；plan/todo 广告关闭时 handler 仍可调用）。
