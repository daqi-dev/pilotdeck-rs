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

#132 对 2,728 个 catalog/diagnostics 原件核 load 诊断及完整 wire 响应；
包括此前转发的 17 个孤立 surrogate 排序案例，现全部原生逐字节对拍。
合法 Unicode/CJK 继续使用同一 ICU 数据。
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

维护版收集脚本为 `tests/record_catalog_io.mts`；初录脚本和来源哈希仍保留在
主仓 `_tmp/20261016-debt-cleanup/catalog/`。维护版要求固定基线、全新输出和
scratch，并拒绝 root；参数依次为 baseline 目录、输出 JSON、scratch。
它只创建隔离文件，transport 无监听，不启动 gateway 或模型进程。复现：

```sh
CATALOG_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
CATALOG_SCRATCH="$(mktemp -d /private/tmp/pdrs-catalog-io.XXXXXX)"
mkdir -p "$CATALOG_SCRATCH/isolation/home" "$CATALOG_SCRATCH/isolation/xdg" "$CATALOG_SCRATCH/isolation/tmp"
env HOME="$CATALOG_SCRATCH/isolation/home" \
  XDG_CONFIG_HOME="$CATALOG_SCRATCH/isolation/xdg" \
  XDG_CACHE_HOME="$CATALOG_SCRATCH/isolation/xdg" \
  XDG_DATA_HOME="$CATALOG_SCRATCH/isolation/xdg" TMPDIR="$CATALOG_SCRATCH/isolation/tmp" \
  PILOT_HOME="$CATALOG_SCRATCH/isolation/home" \
  PILOTDECK_SKIP_BROWSER_OPEN=1 SKIP_BROWSER_OPEN=1 \
  node --import "$PWD/scripts/gateway-parity/watcher.mjs" \
  --import "$CATALOG_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/record_catalog_io.mts "$CATALOG_BASELINE" \
  "$CATALOG_SCRATCH/catalog.json" "$CATALOG_SCRATCH"
```

维护版已在全新目录复录13项，request/response/load/io/env/process字段与入库
原件逐项一致；真实stat size随scratch字节长度变化，不修改既有fixture。

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

公开父测试始终跑完整13项，不读取行号环境变量；专用ignored worker由共享
`test_process` helper以清空环境启动，30秒/64KiB总输出为界，要求恰一个
libtest通过结果及一个来自实际案例ID的回执。父级固定集合与13个唯一回执
对账。旧CJK及两项locale启动也复用此门；零匹配、无/错/重复回执、失败、
超时、超输出均有必跑负控。ignored worker不单独算验收，它们必须被父测试
调用并通过回执核对；CI正常测试仍执行全部父入口。
