# ProjectRuntimeFactory 配置与构造 TS 夹具

来源：固定 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的 `src/cli/ProjectRuntimeRegistry.ts`（`buildRuntime`）与 `src/cli/ProjectRuntimeResourcesBundle.ts`（`stage` / `disposeResources`）。9 个场景、50 个步骤，逐项比较输入、构造产物 id、错误树、事件序列（`construct:` / `stage:` / `dispose:`）与所有保留引用的 id/state/leases。

生成器用共享 `../_shared/baseline.mts` 断言基线 tracked tree 干净，按 Git blob 复算 `sourceSha256` 并核对工作树。esbuild 只在内存转译原文件；真实走的是 `ProjectRuntimeRegistry.buildRuntime` → 真实 `ProjectRuntimeResourcesBundle` → 真实 `loadPilotConfig` 与 `resolvePilotDeckRuntimeProfile`（后者的三个纯依赖模块同样内存转译、按名接线）。只有各资源族构造器是记录事件、可注入失败的 fakes；每族在自己的 TS 字段赋值点成为回滚 owner（contextStorage 与 promptCacheCoordinator 按 baseline 不注册 dispose）。配置从 scratch `PILOT_HOME`（`pilotdeck.yaml`）+ 各项目 `.pilotdeck/pilotdeck.yaml` 读取，`now` 固定为 epoch 0。每轮使用 `mkdtemp` 的独占目录，输出文件 `wx` 创建，不覆盖已有夹具。

复现（每次创建新的输出目录，不删除固定路径）：

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
out=$(mktemp -d "${TMPDIR:-/tmp}/pdrs-project-runtime-factory.XXXXXX")
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/project-runtime-factory/generate.mts "$out"
# 重新创建 out 再跑一次，比较两份 oracle.json 与本目录原件。
cd src
cargo test --locked --offline -p pilotdeck-gateway runtime::project::factory
```

覆盖：构造成功与 dispose；config 失败在任何资源族之前（错误原样冒泡）；stage 失败在第一个族（modelRuntime）/ 中间族（pluginRuntime 构造、executionWorld stage）/ 最后一个族（memoryBundle）的回滚顺序与错误；清理失败的 dispose 顺序与单层 AggregateError 聚合；reload 的 generation 切换（旧代带 lease 存活、drain 后释放，配置变更换代生效）；reload 部分失败保留旧代并回滚 partial、invalidate 单代释放与成功 retry。Rust 侧以 `ProjectResourcesFactory`（生产实现）注入同样的记录型 fake resource-family sources 回放此 oracle。

不在 oracle 内的 TS 行为：两个 full bundle 被 reload 同时 retire 时，TS 的 `disposeResources` 各自的 await 点（mcp → allSettled 数组 → memoryBundle）会按微任务序跨代交错；Rust 按代连续清理，不重排跨代微任务交错。代内 dispose 顺序（即上列所有回滚与清理断言）全部在 oracle 内。因此「reload 部分失败保留旧代」场景的旧代一次 retire 一个（先 invalidate 再 reload），见 generate.mts 内注释。
