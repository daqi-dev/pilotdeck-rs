# SDK 会话配置与校验的真实 TS 原件

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。
被测入口：

- `src/cli/ProjectRuntimeRegistry.ts`：`setSdkSessionConfig`、`assertSdkModelAllowed`、
  `setSessionThinkingForSdk`、`applyFlagSettingsForSdk`、`clearSdkSessionState`，
  以及 `:1807-2056` 的模型/选择器/组织策略校验助手；
- `src/pilot/config/sdkSessionSettings.ts`：全部 397 行（`normalizeSettings`、
  `resolvePilotSdkSessionSettings`、`validatePilotSdkSettingSources`）。

Rust 对应 `src/runtime/sdk_session.rs`。本目录 `oracle.json` 共 **319** 条：

| 分区 | 条数 | 内容 |
|---|---:|---|
| `settingsSources` | 10 | `validatePilotSdkSettingSources`：合法/非法、空数组、null、未知源、重复、顺序无关 |
| `settingsValidate` | 63 | `validatePilotSdkSessionSettings`：类型错、范围错、未知键、null/undefined/缺键三分、thinking/subagents 各分支 |
| `settingsResolve` | 85 | `resolvePilotSdkSessionSettings`：宿主层叠、源优先级、managed 仅被选中时参与、YAML 文件读取与错误、normalize 输出逐字段 |
| `validators` | 105 | managedPermissions / managedTools / managedModels / taskBudget cap / 组织 settingSources / 模型解析 / managed 模型 / 组织模型 / provider 拒绝 / handler 默认判定 / skills |
| `registry` | 56 | 注入假 runtime、假插件运行时、假 task budget 与假信号，逐步骤录 `setSdkSessionConfig` 的状态变化、flags、thinking、handler 登记/退订、clear、abort |

每个 `registry` 用例记录步骤输入、结果、`taskBudget:*` 事件与完整状态
（configs / plugins / projects / thinking / overrides / handler keys）。
`undefined`、`NaN`、`±Infinity`、`-0` 用 `{"$":"..."}` 标记；路径根记为 `<ROOT>`。

## 生成

溯源守卫与 `workspace-snapshot` / `config-store` 相同：`../../baseline_source_guard.mjs` 的
`bindSourceTree`（显式绝对 Git、固定 HEAD、每个 tracked `src` 字节对 Git blob）加
tracked 树干净断言；`sourceSha256` 是两个入口的 relative-import 闭包（728 个文件）
的 commit blob SHA-256。生成器断言 ICU 默认 locale 为 `en-US`，每次使用独占
`mkdtemp` 根并只删除该根。

```sh
PDRS_BASELINE=/absolute/path/to/PilotDeck LANG=en_US.UTF-8 \
node --disable-warning=ExperimentalWarning \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/sdk-session/generate.mts <输出目录>
```

生成器只把基线源码读进内存，经 esbuild 转成 CJS 后由 `new Function` 执行；
`../pilot/index.js` 注入真实的 `sdkSessionSettings` 三个导出，
`../gateway/dialog/errors.js` 注入真实 `DialogGatewayError`。`Object.create`
跳过 constructor，只初始化这些方法用到的字段；假 `PluginRuntime` 只实现
`refresh`/`snapshot`/`createView`，假 `AbortSignal` 按第 1/2 次
`throwIfAborted` 触发。

连续两次生成逐字节一致：

```sh
a=$(mktemp -d); b=$(mktemp -d)
# 分别生成到 $a、$b
cmp "$a/oracle.json" "$b/oracle.json"
```

## 回放

```sh
cd src
cargo test --locked --offline -p pilotdeck-gateway --test sdk_session
```

回放按生成器构造的键序重建每个 result/state，再以 `stringify_json` 逐字节比较
（不排序键）。Rust 侧使用真实 `PluginRuntime`（builtin 注入）、真实
`TaskBudgetStore` 适配器与真实文件系统；插件 manifest 解析器由
`SdkPluginLoader` 注入（A6.3-pr7b 的接缝）。`baseline::oracle(ORACLE)` 校验
commit、`sourceSha256` 闭包与（设置了 `PDRS_BASELINE` 时的）工作树字节。
