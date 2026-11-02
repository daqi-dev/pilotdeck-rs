# #1327 pr1 loadEnabledChannels 金样

固定 `cc7c4e6`，Node **v22.22.0 / ICU 78.2**。生成器原样转译并运行基线 `loadEnabledChannels.ts`；15 个动态导入的本地 mock 类只录构造参数/注入错误，无外部服务。共享基线守卫记录完整源码闭包哈希。`import-failure.mjs` 在一个有 60 秒上限的子进程中证明动态加载失败后继续后续 key。

8 个场景：未配置、truthiness 与空 token、null token fallback、未启用、全 15 key、可选字段、Error/字符串构造失败、导入失败继续。输入及原始 TS 输出同存 `oracle.json`，`adaptersUndefined` 区分 undefined/null；选项中的 undefined 用 `<undefined>` 显式记录；输入含 U+2028，不按非换行 Unicode 字符拆 JSON。Rust 按这些输入重放并逐格比较 result/constructions/errors。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/channel-loader/generate.mts /new/evidence/output
```

生成器及子驱动都用 `flag:'wx'`，不覆盖现有输出。`channel_loader_regenerates_identically` 是需显式运行的忽略测试。Rust 注册表缺少构造器时的 `channel module not registered` 是未接线组件的诊断，由独立 registry 测试覆盖，不声称该字符串来自 TS 动态 import。
