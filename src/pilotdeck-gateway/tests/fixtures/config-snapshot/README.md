# 完整配置快照的真实 TS 原件

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。
被测入口是基线 `src/pilot/config/loadPilotConfig.ts` 的 `loadPilotConfig`，
输出完整的 `PilotConfigSnapshot`（`version`、`schemaVersion`、`loadedAt`、
`contentHash`、`sources`、`diagnostics`、`config`），不是 catalog 投影。

## 目录与条数

`oracle.json` 共 **180** 条，`count` 与 `cases.length` 一致，Rust 测试对条数、
source 数量和每个 `sourceSha256` 长度做精确断言：

- 105 条手写与错误用例：覆盖各配置节、诊断、缺省值、路径、非 ASCII、project
  合并、provider/模型目录、credential、modules、SOP、proxy 迁移；其中 28 条为
  第二轮新增（第一轮评审反例：router 冲突、API key 字面量、清扫周期、tier 键序、
  深嵌套、`__proto__`、孤立代理项，以及键序/原型链/分支覆盖），1 条为既有错误
  用例 `diag-1-empty`（输入 `{}`，`error.code` 与首条诊断的
  `diagnosticCode` 不同），第四轮新增 12 条边界探针：`memory.schedule` 嵌套/平铺
  `.inf`（嵌套报错、平铺保留 Infinity）、非串 `memory.model`、`gateway.port` 的
  0/越界/小数/负数、多斜杠 `agent.subagents.default` 引用与含斜杠 model id，
  第五轮新增 6 条：`$`/Unicode 名 `${}` 字面量、subagents 与 provider 的
  `(0,1)` 取整差异、`headers`/`extraBody` 的 `__proto__` 语义；#310 新增 5 条：
  三条键序分辨（`headers`、`adapters.*.extra`、`runPolicy.failureGuard.toolFailureLimits`
  中大小写/标点/数字/非 ASCII 键对，钉住 `stableStringify` 的 en-US `localeCompare`
  与码元序不同的相对顺序），以及 `anthropic`/`google` 协议在非目录 provider 上的默认能力；
- 18 条来自既有 `tests/fixtures/config/catalog.json` 中 `load.ok` 的用例；
- 57 条来自既有 `tests/fixtures/config/diagnostics.json` 中 `load.ok` 的确定性
  抽样（按成功用例顺序每 18 条取 1 条）。

既有 `config/` 原件是 catalog/诊断对拍，不含 YAML 目录；这里把其中可加载的
真实 YAML 输入交给同一 `loadPilotConfig` 再录完整快照，因此既覆盖旧输入，
也不复制旧断言。

## 生成

生成器使用共享助手 `../_shared/baseline.mts`：先 `assertBaseline` 校验固定
HEAD 和 tracked 文件干净，再 `baselineSourceHashes` 按 commit blob 复算 21 个
被测源码的 SHA-256，并与工作区字节比对。生成过程只创建本次运行的临时
home/project 目录，结束时只删除自己创建的目录。

从仓根执行（Node 22 / 基线自带 tsx；`stableStringify` 的键序依赖 ICU 默认
locale，生成器断言其为 `en-US`，与 Rust 侧一致）：

```sh
PDRS_BASELINE=/path/to/PilotDeck LANG=en_US.UTF-8 \
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/config-snapshot/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/config-snapshot
```

## 归一化

- `loadedAt` 的 `Date` 统一录成 `1970-01-01T00:00:00.000Z`；Rust 侧通过
  `load_snapshot(..., loaded_at)` 传入同一值。
- 每次运行临时根目录记为 `<ROOT>`，生成器的进程 cwd 记为 `<CWD>`；Rust 测试
  用本次独占临时目录和仓根 cwd 做同样的替换。
- 生成器在归一化**之前**先断言：按 14 节转录的哈希输入经基线 `redactConfig` +
  `stableStringify` 复算，必须等于 `loadPilotConfig` 自己产出的 `contentHash`，且
  `config` 不含转录清单之外的节（#310：防止转录清单与基线错得一致）。
- `noGlobal` 用例（不写全局 YAML）会原样记入原件，回放端据此删除复用 home 里的
  上一份 YAML（当前语料没有此类用例）。
- `contentHash` 在路径归一化后，用基线自己的 `redactConfig` +
  `stableStringify` 重新计算；Rust 测试先断言生产 `contentHash` 与
  `snapshot::content_hash(config)` 自洽，再对归一化后的 config 复算并逐字段比较。
  这样既校验哈希算法，又不把生成时的临时路径写进原件。
- `undefined`、`NaN`、`±Infinity`、`-0` 用 `{"$":tag}` 编码；Rust 测试解码后
  按 JavaScript 值语义比较。对象按 `Object.entries` 的枚举顺序比较：canonical
  array-index 键（如 `"2"`、`"10"`）升序在前，其余键按插入序；属性有无与键序
  都算差异。生产侧快照在比较前用 `js_order` 归一化成同一顺序。

## 回放

```sh
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway \
  --test config_snapshot --locked --offline
```
