# model_catalog cwd 回归（#755 pr3）

Node v22.22.0、固定基线 cc7c4e6，原入口 `listModelCatalog` 和 `GatewayWsConnection.handleRequest`，12 格逐个新进程。HOME/config 均隔离，mock API key 不发网络请求；删除 cwd 前未调用 process.cwd，避免 Node cwd 缓存掩盖 uv_cwd。

| home | cwd 正常 | cwd 删除 |
|---|---|---|
| 相对 / 空串 | 本地成功，与 TS frame 对拍 | 本地 ENOENT / uv_cwd，与 TS frame 对拍 |
| 绝对 / `~` / `~/…` / 未设 PILOT_HOME | 本地成功，与 TS frame 对拍 | TS 成功；Rust 保留 Forward，见 A4-MODEL-CATALOG-CWD-001（未批准） |
| 未设 PILOT_HOME 且无 HOME | 保留 None / Forward | 保留 None / Forward；不读取真实用户 home |

Rust 14 格在 bounded test worker 中运行，测试父进程验证退出状态与唯一 receipt。正常、未设 PILOT_HOME 的默认 home/cwd、完整 catalog 输出均有断言；catalog/session 两个直接适配点覆盖错误与回退。无 HOME 的两格仅验证 Rust 既有策略，不声称与 TS homedir 的系统回退对拍。

生成：设置 `PDRS_BASELINE` 后运行 `node --disable-warning=ExperimentalWarning --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/model-catalog-cwd/generate.mts <新目录>`。生成器 recursive mkdir、wx 写文件。ignored 守卫：`model_catalog::cwd_tests::model_catalog_cwd_regenerates_identically`。

负控：`python3 src/pilotdeck-gateway/tests/fixtures/model-catalog-cwd/negative_controls.py --check` 仅检查锚点；提交后用 `<新证据目录> <变异名>` 运行一项或小批。15 项覆盖 cwd 吞错、绝对 home 误报错、缺 HOME、错误 name/code/message、正常 home/cwd、两个直接路由及帧 id/code/message。每项绑定被点名测试专属失败消息，并验证正控→运行时失败→恢复通过；判杀用共享 `negative_suite.judge`：失败格取自外层 libtest 的 `failures:` 清单（仅 stdout 最后一段；嵌套 worker 自己的清单与 panic 回显都在 stderr，不参与——`--nocapture` 下 `... FAILED` 也可能被 receipt 挤到下一行），且该格自身 panic 块（内嵌 worker 输出）携带专属消息，别处 panic 或回显里的消息不判杀（#889 fix-r1）。逐项记 KILLED / SURVIVED / INVALID 后汇总，任一非 KILLED 即非零退出；编译/设置/超时错误记 INVALID，锚点过期立即报错。判杀单测（合成交错日志、不跑 cargo）：`python3 src/pilotdeck-gateway/tests/fixtures/model-catalog-cwd/test_negative_controls.py`。

必须使用调用方设置的本树 `CARGO_TARGET_DIR`；未设报错。脚本使用 `mutating_sources` 默认 HEAD（`NEGATIVE_BASELINE` 仅允许提交前抽样指向临时提交对象）、`run_owned`、`wait_for_slot`；源码未提交就拒跑，不建影子仓库或备用构建目录。全量可能超过 20 分钟，由协调者提交后在宿主分批运行。
