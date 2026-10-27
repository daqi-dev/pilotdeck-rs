# commands-catalog（#887）

`oracle.json` 来自固定 cc7c4e6 的真实 `listCommands` 与 `GatewayCommandCatalogBundle`。生成器要求 Node v22.22.0、`PDRS_BASELINE` 指向该提交的干净 TS checkout，且其 `node_modules/tsx` 已安装；先校验源码与导入闭包哈希。输出目录应是新的独占目录，文件以 `flag:'wx'` 创建。

## locale 与回放

金样固定为 **en-US**（`generate.mts:11`）。`replay` 在任何文件操作和页面比较之前检查实际 locale；非 en-US 会以 `#887 oracle requires en-US` 明确失败，避免把环境差异误报为产品回归。locale 优先级为 `LC_ALL` → `LC_MESSAGES` → `LANG`，因此只设置 LANG 不足以覆盖已有 LC_*。生产 provider 仍使用调用进程的实际 locale。

```sh
export CARGO_TARGET_DIR=/absolute/path/to/this-tree/_build/cargo-target
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
unset LC_ALL LC_MESSAGES
export LANG=en_US.UTF-8
bash scripts/cargo-slots.sh
cargo test -q --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib runtime::plugins::catalog::
```

`catalog_locale_contract` 与 `plugin_runtime_locale_contract` 共用守卫及契约助手，按 `../node-locale/oracle.json` 的每格在独立子进程中验证：en-US 通过，其余 resolved locale 明确拒绝；不修改并行测试进程的全局环境。每个子进程退出即完成，30 秒仍未退出则杀死并报错。

## 重生成

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --disable-warning=ExperimentalWarning --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/commands-catalog/generate.mts /absolute/new-output
bash scripts/cargo-slots.sh
cargo test -q --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib \
  runtime::plugins::catalog::tests::catalog_regenerates_identically -- --ignored --exact
```

## 负控

先提交产品源码，使其与 HEAD 完全一致；执行器拒绝未提交或未知源码，不能绕过该守卫。必须传入上面的专用 `CARGO_TARGET_DIR`。长套件由协调者在宿主运行；本地只跑一个具名样本。输出目录必须尚不存在。

```sh
python3 src/pilotdeck-gateway/tests/fixtures/commands-catalog/negative_controls.py /absolute/new-negative-output
```

每个变异先编译，再要求被点名完整测试失败并出现该变异的专属断言，最后恢复源码并重跑正控；编译错误、无关 panic、超时属于 INVALID。所有变异逐项记 KILLED / SURVIVED / INVALID 后统一汇总，任一非 KILLED 返回非零。

## 前置于 U5 的 wire limit 解码

当前 `CommandsListInput.limit: Option<u32>` 是已类型化的 provider 输入，不是 JSON 解码器。U5 原生入口必须区分未提供、合法整数和非法输入：不得把 1.5 截为 1、-1 转为 None、超 u32 数值截断。缺省/null 按真实 TS 的 `?? 50` 语义处理；1..100 整数接受，其余（包括字符串、布尔值）返回 INVALID_LIMIT。本轮 Node v22.22.0 实测覆盖上述 11 种输入，实测记录见 #887 评审证据。U5 仍须补充对应的可重生成金样、真实 wire 入口测试与负控；本包未实现 wire decoder，未据此宣称 U5 完成。
