# 配置与 model_catalog_list 的离线 Node 原件

基线 commit：`cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。
运行时：Node `v22.22.0`，ICU `78.2`，npm `yaml@2.9.0`。
录制调用真实 `loadPilotConfig`、`listModelCatalog` 与注入 transport 的
`GatewayWsConnection`，不启动服务、不监听端口、不请求模型。

- `catalog.json`：上轮的 32 个原件，迁移后 SHA-256 仍为
  `69a8df6daada735c75e8cf90660fb161d7820e7ef228c9db76dd9e0ee8f50830`。
- `diagnostics.json`：2,696 个配置案例，覆盖合法配置、各节错误与优先级、
  provider warning、env、StaffDeck、文件路径与读取错误、YAML alias/native
  类型、credential、模块绑定、locale 与孤立 surrogate。
- `host-boundaries.json`：18 个宿主 intrinsic 案例，单独验证 dispatch 转发。
  它们不计入本地 wire 对拍通过数量。每个原型案例使用独立 Node，避免基线
  deepFreeze 对共享 Object.prototype 的影响污染其他案例。

D6 A 候选对2,728个catalog/diagnostics原件核load诊断：2,711个wire本地逐字节对拍，
17个过滤后多项id含孤立surrogate的明确case经JS_HOST_REQUIRED转发（固定ID见config/tests.rs）。
这是Node与ICU4X非良构UTF16权重不同的精确宿主边界；合法Unicode/CJK继续本地。
18个宿主intrinsic边界另行断言转发。历史JSON原件未改；新真实原件见session-model/illformed-collation.json。

Rust 测试对本地请求逐字节比较完整 response 字符串；诊断对象仅统一属性顺序后比较，
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

## #120：catalog RPC 自身的 errno 原件

`catalog-io-errors.json` 独立录制 13 项 `model_catalog_list` 请求，沿真实
`GatewayWsConnection → listModelCatalog → loadPilotConfig` 调用，不借用
session-model RPC 原件。固定 Node 22.22.0 / ICU 78.2 / cc7c4e6；源码完整
SHA-256 和调用路径在原件顶层。global 配置的 exists/stat 与 read 两个边界
覆盖 ENOENT、EACCES、ENOTDIR、EISDIR、ENAMETOOLONG、ELOOP；权限错误
由实际 chmod 制造，读取竞态只改变自有文件树，不注入 errno/message。
每项观测分别标记 `loader` 与 `rpc`，原始完整 wire/trace 保留在主仓
`_tmp/20261016-debt-cleanup/catalog/raw-recording/cases/*/raw-rpc.json`。
入库 fixture 仅把案例根映射为 `/fixture`，文件字节中的根也作相同映射。

收集脚本为主仓 `_tmp/20261016-debt-cleanup/catalog/record_catalog_io.mts`；
同目录 `source-binding.json` 绑定脚本、源码、fixture 及 13 份 raw 哈希。
复录参数依次为固定 baseline 目录、全新输出 JSON、全新 scratch；沿用
本页前述隔离 HOME/XDG/TMPDIR 和 tsx 调用，必须非 root，并设置两项浏览器
guard。脚本只创建隔离文件，transport 无监听，不启动 gateway 或模型进程。

Rust 必跑测试锁定 13 个唯一非空 ID、RPC method 和两阶段观测，空/截短/重复
负控均拒绝；完整 wire 和 loader diagnostics 分别比较。独立 pairing 从真实
Node 原始 request/response 验证本地应答，随后 forwarded observer-fence
必须成为上游下一段字节，证明 catalog 请求未上游。catalog 自身 fixture 还
检查 global stat/read 的所有未准入 errno、EDQUOT、未知数字与无 errno 错误
仍整体转发。生产映射和判分装置未改。

```sh
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway catalog --locked --offline
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway method_whitelists_equal_source_oracle_errno_sets --locked --offline
```
