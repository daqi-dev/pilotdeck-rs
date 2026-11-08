# Native input admission oracle

固定原始 TS cc7c4e653b15c8b2cec7bcf2d15d69396ae85 的真实 InputProcessor、PluginRuntimeExtensionResolver 和 TurnInputProcessor；Node v22.22.0 / ICU 78.2，生成器先逐字核验三个源码 Git blob，并要求独占新输出路径。

```sh
PDRS_ORACLE_OUT=/private/tmp/unique-input-oracle.json /Users/da/opt/node-v22.22.0-icu78-darwin-arm64/bin/node --import /Users/da/ws/pdrs-runner/resources/PilotDeck/node_modules/tsx/dist/loader.mjs generate.mts
cmp oracle.json /private/tmp/unique-input-oracle.json
```

16 条含文本/未知命令/插件空体和非空体、32000/8000 UTF16 截断、碰撞、lone surrogate、Unicode 空白、重复命令、isMeta、blocks 与 session snapshot 变更；纯 admission 与 turn wrapper 同采。此金样不冒充真实 native 侧车或远端 provider 联调。Rust tests 与真实集成需另由协调者在 mbp2 验证。
