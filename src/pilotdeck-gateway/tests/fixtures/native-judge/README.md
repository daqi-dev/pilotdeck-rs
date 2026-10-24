# Native provider judge 实测

`generate.mts` 在固定 TS 基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 上运行真实 `createNativeModelInvocationProvider` → registry → `createRouterRuntime`。两侧都使用临时 loopback HTTP 服务；只替换远端模型服务，不替换 provider 或 judge invoker。独立服务端记录每个请求的 method、path、body。金样先由 TS 采集，Rust 逐项比较。

覆盖 7 案：simple/complex 成功选择、401 auth error、400 request error、503 retryable error、未注册 judge provider、已注册但缺 judge model。每案分别实测 direct complete 与 token-saver decision；比较错误类别、code/message/retryable、decision、全部事件和实际 HTTP 请求。direct 请求故意带 `stream:true`，线上 body 必须为 `false`；401/400 judge 请求仅 1 次，503 为 3 次，缺 provider/model 为 0 次。两个 missing 案的配置均先通过真实配置加载，缺口仅在注册表或运行时模型定义注入。

本夹具把 provider 的 request/stream retry 均设为 0，以隔离 judge 的重试分类。它不证明完整 complete provider 协议矩阵、默认请求重试策略、sidecar 四格、gateway 外部入口或 TS host 退役已完成。

```sh
PDRS_BASELINE=/absolute/path/to/PilotDeck PDRS_GIT=/usr/bin/git LANG=en_US.UTF-8 \
node --import /absolute/path/to/PilotDeck/node_modules/tsx/dist/loader.mjs \
src/pilotdeck-gateway/tests/fixtures/native-judge/generate.mts /new/output/directory
```

`tests/baseline_source_guard.mjs` 校验完整源树 Git 身份，记录四个入口的传递源码闭包；Rust 经 `common/baseline.rs` 校验固定 commit/sourceSha256。生成器拒绝覆盖已存在的 `oracle.json`。测试：`cargo test -p pilotdeck-gateway --test native_judge`；重采校验：设置 `PDRS_BASELINE`、`PDRS_GIT` 后加 `-- --ignored`。

TS 锚点：`src/model/providers/ModelInvocationProviderRegistry.ts:150`、`:269`；`src/model/streaming/streamModel.ts:76`、`:140`；`src/router/tokenSaver/classifyAndRoute.ts:307`、`:327`。Provider 错误保持类身份是 code/message 及 retryable 传递的必要条件。
