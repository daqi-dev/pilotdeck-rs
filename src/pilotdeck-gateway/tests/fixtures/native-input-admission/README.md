# Native input admission oracle

固定原始 TS cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85 的真实 InputProcessor、PluginRuntimeExtensionResolver 和 TurnInputProcessor；Node v22.22.0 / ICU 78.2，生成器先逐字核验三个源码 Git blob，并要求独占新输出路径。

```sh
PDRS_BASELINE=/absolute/path/to/fixed/PilotDeck PDRS_ORACLE_OUT=/private/tmp/unique-input-oracle.json node --import /absolute/path/to/fixed/PilotDeck/node_modules/tsx/dist/loader.mjs generate.mts
cmp oracle-boundaries.json /private/tmp/unique-input-oracle.json
```

历史 oracle.json 的 16 条含文本/未知命令/插件空体和非空体、32000/8000 UTF16 截断、碰撞、lone surrogate、Unicode 空白、重复命令、isMeta、blocks 与 session snapshot 变更；纯 admission 与 turn wrapper 同采。此金样不冒充真实 native 侧车或远端 provider 联调。Rust tests 与真实集成需另由协调者在 mbp2 验证。

原 `oracle.json` 原字节保留；新 `oracle-boundaries.json` 23 条增加 blocks 非 meta、冒号与连字符名称、32000/8000 等值及截断点切分代理。逐条比对 TurnInputProcessor 的 messages/shouldCallModel、InputProcessor 的 diagnostics/command，含 snapshot.late。`meta-title-oracle.mts` 实际调用固定 TS TurnRunner，确认 meta 回合仍先调用辅助 title provider 但不启动 main loop；不得把 title 请求当作主模型执行。Rust 侧真实 tail / title 仍须实测。

该夹具使用显式 PDRS_ORACLE_OUT 独占文件协议，与默认写 oracle.json 的通用重生成器不同；复采上面的命令与 cmp 是其回归步骤。基线 checkout 必须固定只读，生成器在导入前及发布前复核源码。meta-title-oracle.json 目前是 TS 行为调查证据，尚不是 Rust 验收通过证据。
