# Native multi-project request gate oracle

真实固定 TS InProcessGateway 的未配置 always-on 与 SOP，以及真实 ProjectRuntimeRegistry 的 mcpServerStatusForSdk / resolve；仅注入内存 router 和带空插件的 project runtime，不启动网络或 provider。基线 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85；Node 22.22.0 / ICU 78.2。生成前后核两源 Git blob；设置唯一输出：

PDRS_BASELINE=/Users/da/ws/pdrs-runner/resources/PilotDeck PDRS_ORACLE_OUT=/private/tmp/unique-native-gates.json /Users/da/opt/node-v22.22.0-icu78-darwin-arm64/bin/node --import /Users/da/ws/pdrs-runner/resources/PilotDeck/node_modules/tsx/dist/loader.mjs generate.mts

预期 cmp oracle.json，独占输出不可覆盖。12 格覆盖 projectKey null、5、foreign × 4 方法；mcp 的缓存 runtime 只记录真实 refresh 与空 plugin 情况，不能冒称网络 MCP 实测。settings 数值 5 的已登记 coercion 差异继续由 U7-EXTENSIONS-RELOAD-STRING-COERCION-001 覆盖，native-settings 金样仍保留原件。此夹具本身不代表 Rust 已编译或通过。
