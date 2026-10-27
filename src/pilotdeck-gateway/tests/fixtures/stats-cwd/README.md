# stats cwd 回归（#879 pr2）

Node v22.22.0、固定基线 cc7c4e6，原入口 `TokenStatsCollector` 构造器和 `createRouterRuntime`，
22 个场景逐个新进程。HOME 与 PILOT_HOME 均隔离；删除 cwd 前不调用 `process.cwd()`，避免
Node cwd 缓存掩盖 `uv_cwd`。

| PILOT_HOME | cwd 正常 | cwd 删除 |
|---|---|---|
| 相对 / 空串 | 成功，写 `<home>/router/stats.jsonl` | `ENOENT` / `uv_cwd`，构造器抛出 |
| 绝对 / `~` / `~/nested/../home` / 未设 | 成功，`cwdReads=0` | 成功，`cwdReads=0` |
| `HOME=""` + `~` | 成功，写相对 `router/stats.jsonl` | 成功，相对路径不存在 |
| `HOME=""` + 未设 / `~/data` | 成功，cwd 内 home | `ENOENT` / `uv_cwd` |
| 绝对 `filePath` | 成功，写指定文件，`cwdReads=0` | 同左 |
| `stats.enabled=false` | 成功，不建文件，`cwdReads=0` | 同左 |

`filePath` 是 TS 配置解析不接受的构造器直传项，仅用于覆盖 `TokenStatsCollector` 的
`config.filePath` 分支；生产配置看不到它。`HOME` 缺失的 `os.homedir()` passwd 回退未实测，
见 `specs/gateway-parity/diffs/A6.4-STATS-CWD-001.md`。

Rust 22 格各跑两个 bounded test worker：collector 与 `RouterRuntime::with_deps` 分别在独立
子进程、独立临时 root / cwd / home 中运行，父进程对每个子进程分别校验退出状态与唯一
receipt（每格 2 个 receipt）。runtime 入口经 test-only 访问器读取 runtime 自己持有的
collector 实际选中的 `jsonl_path`，不从 collector 副作用或测试预计算的期望路径取值。
测试名：
`runtime::model::stats::cwd_tests::{relative,empty,absolute,tilde,tilde_child,unset,empty_home_tilde,empty_home_unset,empty_home_child,file_path,disabled}_{normal,deleted}_cwd`。

生成：设置 `PDRS_BASELINE` 后运行
`node --disable-warning=ExperimentalWarning --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/stats-cwd/generate.mts <新目录>`。
生成器 recursive mkdir、`flag:'wx'` 写 `oracle.json`，并记录源码 SHA256。ignored 守卫：
`runtime::model::stats::cwd_tests::stats_cwd_regenerates_identically`。

负控：`python3 src/pilotdeck-gateway/tests/fixtures/stats-cwd/negative_controls.py --check` 只检查
锚点；提交后用 `<新证据目录> [变异名...]` 运行一项或小批。13 项覆盖迟到的 `/` 回退、错误
message/kind、绝对 home 误读 cwd、相对 home 基准、router 目录、disabled/filePath 短路，
`~` 逐字返回、router 三个直接适配点，以及 runtime 入口选错 home。每项绑定被点名测试的专属
消息，逐项记 KILLED / SURVIVED / INVALID 后汇总，任一非 KILLED 即非零退出；锚点过期直接
失败退出，不记为 SURVIVED。编译 / 设置 / 超时错误记 INVALID，不冒充测试失败。判杀用共享
`negative_suite.judge`（#889 fix-r1）：失败格取自外层 libtest 的 `failures:` 清单（仅 stdout
最后一段；嵌套 worker 自己的清单与 panic 回显都在 stderr，不参与——`--nocapture` 下
`... FAILED` 也可能被 receipt 挤到下一行），且该格自身 panic 块（内嵌 worker 输出）携带
专属消息，别处 panic 或回显里的消息不判杀。判杀单测（合成交错日志、不跑 cargo）：
`python3 src/pilotdeck-gateway/tests/fixtures/stats-cwd/test_negative_controls.py`。
`absolute-guard` 只要求声明的四格中至少一格转红：裸 `~` 在 `stats.rs:1215` 提前返回，
不经过被变异的 `expanded.is_absolute()`，`tilde_deleted_cwd` 结构上不会红；其余三格必须
命中各自的 `#879 stats cwd collector matrix:` 消息，失败在无消息 panic 上不算杀。其余
12 项仍要求声明的每一格都命中专属消息。`runtime-home` 的 18 格是除 `disabled-*`
（读 home 前返回）与 `file-path-*`（使用 `config.filePath`）外的全部格；把 router 传给
collector 的 home 改成 `/wrong` 后，这 18 格都须以
`#879 stats cwd runtime matrix:` 消息转红。

必须使用调用方设置的本树 `CARGO_TARGET_DIR`；未设报错。脚本使用 `mutating_sources` 默认
HEAD、`run_owned`、`wait_for_slot`；源码未提交就拒跑，不建影子仓库或备用构建目录。提交前
样本可用 `NEGATIVE_BASELINE=<临时 git commit-tree 对象>` 指向同字节的临时基线（不移动
HEAD），默认值仍是 HEAD。全量约 39 次 cargo 测试，可能超过 20 分钟，由协调者提交后在宿主
分批运行。
