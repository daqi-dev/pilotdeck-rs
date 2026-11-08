# MCP UTF-16 context oracle

固定 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85；生成器实际调用 PluginRuntime session contribution lease、PromptAssembler 和 buildCachePlan，先核四个 TS Git blob 与 Node v22.22.0 / ICU 78.2。动态插件 MCP 指令经真实 2048 UTF-16 截断形成孤立高代理，再检查默认 prompt、custom 抑制/扩展开关与缓存 fingerprint；服务器名还含 XML 属性字符和孤立代理。

```sh
PDRS_ORACLE_OUT=/private/tmp/unique-context-utf16.json /Users/da/opt/node-v22.22.0-icu78-darwin-arm64/bin/node --import /Users/da/ws/pdrs-runner/resources/PilotDeck/node_modules/tsx/dist/loader.mjs generate.mts
cmp oracle.json /private/tmp/unique-context-utf16.json
```

Rust `context_prepare` 使用同 fixture 核真实 `DefaultContextRuntime.prepare_for_model` 的 systemPrompt、systemPromptParts 和 cachePlan fingerprint；本候选在提交前尚未 Rust 编译或运行。
