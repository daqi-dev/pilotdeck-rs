# SOP knowledge 权限与模型选择实证

`model-id-oracle.json` 来自固定 TS `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的真实 `createKnowledgeQueryTool` + `createKnowledgeModulePort`，用内存 fetch 记录 wire，不访问外部服务。五格覆盖无 credential 的 camel/snake/both 模型选择输入及有 credential 时两种键在外呼前拒绝；scope 使用虚构身份。Rust 对应 `native_process::knowledge::tests::fixed_ts_model_selection_keys_reach_real_knowledge_consumer`，比较实际 execute 产生的 POST input 或原 error code/零外呼。

复采在固定 TS checkout 的依赖安装环境中运行：

```sh
PDRS_BASELINE=/absolute/PilotDeck node --import /absolute/PilotDeck/node_modules/tsx/dist/loader.mjs /absolute/capture-model-id.mts /absolute/unique-output.json
```

输出独占创建，采前后核固定 commit 与两源 Git blob。只证明直接 knowledge consumer 的参数规则，不外推到可选 SOP authority 或任意远端服务的最终授权。无 credential 保留调用方模型选择、覆盖 scope 的行为是 TS 兼容合同；有 credential 时拒绝调用方身份/模型选择的既有策略不变。
