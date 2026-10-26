# `resolve_settings` 快照响应投影的真实 TS 原件

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。
被测入口是基线的生产绑定 `src/cli/createLocalGateway.ts:1402`
（`resolveSettings: async () => toGatewayResolvedSettings(configStore.getSnapshot())`），
生成器 import 真实 `createLocalGateway`，逐用例独立临时 home/project 树上
boot 完整网关后调用 `gateway.resolveSettings()`，响应逐字录制；不复制任何
基线函数体。Rust 侧对应 `config/settings.rs` 的 `resolved_settings`（未开启
原生组件，默认 dispatch 仍走 TS 宿主）。

## 目录与条数

`oracle.json` 共 **13** 条，`count` 与 `cases.length` 一致，Rust 测试对条数、
source 数量和每个 `sourceSha256` 长度做精确断言。三类输入：

- 成功（11 条）：`minimal`（sources=[env/bootstrap, default 文件]，env source 无
  `path` 有 `phase`，文件 source 有 `path`+`contentHash` 无 `phase`）、
  `full-sections`（全节 + 多层密钥形 key 递归脱敏，含 `credentialSource` 与
  `maxContextTokens` 因 `credential`/`token` 子串而被脱敏的实测行为）、
  `unicode-lookalike-keys`（extraBody 的 `toKen`（U+212A KELVIN SIGN）在
  非 Unicode 大小写不敏感匹配下不折成 `token` 而保留原值，`TOKEN`/`token`
  照常脱敏；Unicode 折叠会把它误判成 `token`，见 `snapshot.rs` 的
  `secret()`）、
  `unknown-field-warning`（`recoverable: true` + `path` 的告警）、
  `provider-excluded-hint`（`hint` 存在必须保留的分支）、
  `router-unknown-fallback`（快照诊断带 `hint: undefined` 自有键，响应必须省略）、
  `env-merge-source`（env/merge source，无 `path` 有 `contentHash`+`phase`）、
  `project-config`（priority 20 的 project source，sources 顺序
  [env, default, project]）、`project-only`（无全局文件，default source 整体缺席）；
- 外部 provider（2 条）：`external-provider-env-ref`（`apiKey: ${PARITY_KEY}` 经
  env 解析，`credentialSource: "environment"`，响应中 apiKey 仍被脱敏）、
  `external-provider-catalog`（目录 provider 无显式 apiKey，经目录环境变量解析）；
- 未配置（2 条）：`unconfigured`、`invalid-missing-model`——真实入口在网关装配
  阶段 `loadPilotConfig` 抛 `PilotConfigError`，没有响应体，金样录
  `{name, code, message, diagnosticCode, hint}`；Rust 侧与 `ConfigStore::new`
  返回的 `Error` 同字段比对。

## 生成

生成器使用共享助手 `../_shared/baseline.mts`：先 `assertBaseline` 校验固定
HEAD 和 tracked 文件干净，再 `baselineSourceHashes` 按 commit blob 复算 6 个
被测源码的 SHA-256，并与工作区字节比对。生成过程只创建本次运行的临时
home/project 目录（`mkdtemp`），结束时 `dispose()` 网关、只删除自己创建的目录，
不碰 `~/.pilotdeck`。

从仓根执行（Node 22 / 基线自带 tsx；`stableStringify` 的键序依赖 ICU 默认
locale，生成器断言其为 `en-US`，与 Rust 侧一致）：

```sh
PDRS_BASELINE=/path/to/PilotDeck LANG=en_US.UTF-8 \
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/settings/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/settings
```

boot 用离线环境（`PILOTDECK_SKIP_BROWSER_OPEN` 等 5 个跳过变量置 `1`），
不读进程环境其余变量。

## 归一化

- `loadedAt`：store 对快照和全部 sources 用同一个 `Date`，其 ISO 串统一替换为
  `1970-01-01T00:00:00.000Z`；Rust 侧通过 `StoreOptions.loaded_at` 传固定时钟。
- 每次运行临时根目录记为 `<ROOT>`，生成器的进程 cwd 记为 `<CWD>`；Rust 测试
  用本次独占临时目录做同样的替换。
- 生成器在归一化**之前**先断言：按 14 节转录的哈希输入经基线 `redactConfig` +
  `stableStringify` 复算，必须等于真实入口返回的 `contentHash`，且 `config`
  不含转录清单之外的节（防止转录清单与基线错得一致）。
- `contentHash` 在路径归一化后，用基线自己的 `redactConfig` + `stableStringify`
  重新计算；Rust 测试先断言生产 `contentHash` 与 `snapshot::content_hash(config)`
  自洽，再对归一化后的 config 复算并逐字段比较。
- `undefined`、`NaN`、`±Infinity`、`-0` 用 `{"$":tag}` 编码；Rust 测试解码后
  按 JavaScript 值语义比较。对象按 `Object.entries` 的枚举顺序比较，属性有无与
  键序都算差异（`src/tests` 侧快照已用 `js_order` 归一化成同一顺序）。

## 回放

```sh
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway \
  --test settings --locked --offline
```
