# 配置与 model_catalog_list 的离线 Node 原件

基线 commit：`cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。
运行时：Node `v22.22.0`，ICU `78.2`，npm `yaml@2.9.0`。
录制调用真实 `loadPilotConfig`、`listModelCatalog` 与注入 transport 的
`GatewayWsConnection`，不启动服务、不监听端口、不请求模型。

- `catalog.json`：上轮的 32 个原件，迁移后 SHA-256 仍为
  `69a8df6daada735c75e8cf90660fb161d7820e7ef228c9db76dd9e0ee8f50830`。
- `diagnostics.json`：2,696 个本地案例，覆盖合法配置、各节错误与优先级、
  provider warning、env、StaffDeck、文件路径与读取错误、YAML alias/native
  类型、credential、模块绑定、locale 与孤立 surrogate。
- `host-boundaries.json`：18 个宿主 intrinsic 案例，单独验证 dispatch 转发。
  它们不计入本地 wire 对拍通过数量。每个原型案例使用独立 Node，避免基线
  deepFreeze 对共享 Object.prototype 的影响污染其他案例。

Rust 测试逐字节比较完整 response 字符串；诊断对象仅统一属性顺序后比较，
保留数组顺序、字段有无、文本、code/path/source/hint/recoverable。
原件中的 `<PILOT_HOME>` 只替换隔离目录位置，relative 配置路径仍为相对路径。
credential 全部为测试用占位符。

在仓根复现新增原件（选择新的 scratch 路径）：

```sh
mkdir -p _tmp/config-oracle/{home,xdg,tmp}
HOME="$PWD/_tmp/config-oracle/home" \
XDG_CONFIG_HOME="$PWD/_tmp/config-oracle/xdg" \
XDG_CACHE_HOME="$PWD/_tmp/config-oracle/xdg" \
XDG_DATA_HOME="$PWD/_tmp/config-oracle/xdg" \
TMPDIR="$PWD/_tmp/config-oracle/tmp" \
PILOT_HOME="$PWD/_tmp/config-oracle/home" PILOTDECK_SKIP_BROWSER_OPEN=1 \
node --import /Users/da/ws/pilotdeck-rs/resources/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/record_config_diagnostics.mts \
  /Users/da/ws/pilotdeck-rs/resources/PilotDeck \
  src/pilotdeck-gateway/tests/fixtures/config/diagnostics.json \
  _tmp/config-oracle/recording
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway config::tests --locked --offline
```

生成输入为 `tests/config_cases.mjs`；基础录制器为
`tests/record_config_catalog.mts`，数据导出器为 `tests/record_config_data.mts`。
以上脚本均不属于冻结 A2 尺子。
