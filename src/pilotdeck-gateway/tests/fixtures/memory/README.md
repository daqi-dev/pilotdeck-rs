# memory 管理投影与 bundle 生命周期夹具

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。出处：
`src/cli/ProjectMemoryBundle.ts:41-86`、`src/cli/createLocalGateway.ts:1238-1259`。
源码 SHA-256 位于 `oracle.json.sourceSha256`；生成器先用共享
`tests/baseline_source_guard.mjs` 的 `bindSourceTree` 校验整个 `src/` 树，
再逐文件核对固定提交的 blob。

## 生成

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/memory/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/memory
```

重复执行两次 `oracle.json` 必须逐字节一致。生成器不创建或删除固定路径；
所有期望值来自真实 TS 执行，不使用手写 replica。

## 覆盖

- `bundleCases`：真实 `ProjectMemoryBundle`，注入假 provider。
  `factory-success`（完整 options、三个资源键、dispose 幂等且异步、只调一次
  disposer）、`factory-throws`（工厂抛错仍消耗唯一 stage 机会）、
  `dispose-before-stage`（先 dispose 后 stage 报 already disposing）、
  `factory-undefined`（空 resources）、`native-unconfigured`（无工厂、无 config，真实 native 工厂早退不建 EdgeClaw）、`native-create-provider`（兼容
  `createProvider`，包装 provider/service，dispose 调 service.close）、
  `provider-factory-preferred`（两个入口并存时优先 providerFactory）、
  `memory-only-provider`（可选成员缺省仍保留三个键）、
  `dispose-rejects`（disposer 抛错时 dispose promise 拒绝，重复调用观察到同一拒绝）、
  `native-disabled`（无工厂 + `config.enabled:false`，真实 native 工厂早退不建
  EdgeClaw）。
- `projectionCases`：从 `createLocalGateway.ts` 提取未修改的
  `memoryList`/`memoryWipe` 方法体，注入假 `dialog.projects.resolveProjectKey`、
  `registry.resolve` 和 provider。覆盖 list 的 100 上限、sessionKey 真值过滤
  （空串全量、空白串不 trim）、wipe 的 session/project 分支与
  `INVALID_MEMORY_SCOPE`、provider 缺失的 `CAPABILITY_UNAVAILABLE`（先于
  scope 校验）、provider 抛错原样传播、projectKey 缺省 fallback 与解析后键、
  `resolveProjectKey` 拒绝与 `registry.resolve` 抛错原样传播（不折叠成
  `CAPABILITY_UNAVAILABLE`）。

## 回放

```sh
cd src
cargo test --locked --offline -p pilotdeck-gateway --test memory
```

`tests/memory.rs` 精确断言 10 个 bundle case、8 个 projection case 及每个
step；回放顺序、事件、工厂入参、返回/错误与调用序列均来自 oracle。
