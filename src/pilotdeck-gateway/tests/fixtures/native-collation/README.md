# #132 原生孤立 UTF-16 排序

`regression.rs` 复用 #1193 的 `../surrogate-collation-matrix/oracle.json`：5 locale × 10 键类 × catalog/session 两方法，共 100 格完整响应逐字节核对。原件不改；UTF-16 字段经 protocol codec 保留。

`native_matrix_matches_node` 直接走产品 response；`matrix_pairing_has_zero_business_upstream` 为每个 locale 启独立测试子进程，再走真实 pairing。认证后业务帧全部原生响应；随后转发 `describe_server` fence，上游下一帧必须恰好等于 fence，证明此前没有业务帧转发。

补充 `oracle.json` 由固定 Node v22.22.0 / ICU 78.2 的 `localeCompare` 生成，含 62 键 × 62 键 × 14 locale = 53,816 对比较：真实 PUA、U+FFFD、孤立高/低、合法 pair、连续孤立项、组合重音、收缩、Thai/Lithuanian、数字和法加倒序 secondary。它是运行时比较器原件；真实 TS 方法原件仍以上述 #1193 矩阵为准。

新生成器只写新目录中的独占文件；Node/ICU 不符即失败。集成测试 `native_collation_regenerates_identically`（ignore）用共享 `regenerates_identically("native-collation")` 逐字节重生。原方法矩阵的既有 ignore 守卫也需实跑。

```sh
export PDRS_BASELINE=/absolute/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/native-collation/generate.mts /absolute/new-output
```

`negative_controls.py` 对产品做独立变异：码元排序、FFFD 折叠、PUA 碰撞、catalog/session 各自残留边界、拆坏合法 pair、丢法加倒序 secondary，以及 Identical 的 normalization/ignorable。每项正控→应用→编译→绑定断言失败→恢复→正控；全部执行后汇总，任何非 KILLED 即非零。过期锚点是错误，不是 SURVIVED。

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/native-collation/negative_controls.py --check-anchors
# 提交后、独占本树运行；必须由调用方提供 CARGO_TARGET_DIR。
python3 -I src/pilotdeck-gateway/tests/fixtures/native-collation/negative_controls.py NEW_OUTPUT_DIR
```

未提交/新增文件不在 HEAD 时 `mutating_sources` 必须拒绝；不建影子仓库、不改 guard。worker 只交脚本，协调者提交后运行。`--compile-only` 可独立确认所有变异可编译，但 COMPILED 不计 KILLED。

本单移除 catalog/session 的排序 host 边界和共享比较器的 PUA 映射。archive、storage transcript、workspace snapshot 的独立边界保持其原任务归属；A2 十 method、A8 总体验收及双家非作者终审不由本夹具代填。

#132 fix-r2：同一负控入口还覆盖 ICU vendor 登记、恢复 test/bench target、放宽 lib-only 守卫四种变异，绑定 `icu_collator_vendor_scope_matches_approved_lib_only_boundary`。执行器按完整测试名选择 lib 或 `unsafe_boundary` 集成目标；编译、负控与恢复正控使用同一目标。所有变异均须先提交，再通过 HEAD 源码守卫运行。
