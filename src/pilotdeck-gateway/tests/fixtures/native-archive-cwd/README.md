# #1154 archive 配置项目默认值

`generate.mts` 以 Node v22.22.0 调用只读 cc7c4e6 的真实 createLocalGateway 及三个 InProcessGateway archive 方法。HOME/XDG/PILOT_HOME/TMPDIR 全在自有 scratch，配置为离线假 provider，不提交模型请求；复用现有 `scripts/gateway-parity/watcher.mjs` 的 shallow polling 适配沙箱 EMFILE（不覆盖热加载）。输出目录递归创建，所有文件 `flag:'wx'`，finally 恢复 cwd/环境并只删除自有 scratch。

矩阵三方法（manifest/entries/artifact）× 四 projectKey（missing/null/empty/absolute）× 三 cwd（project/other/deleted）=36 格。记录完整返回值或错误 code/message，以及实际 cwd 可读性；只将临时根替换为 `{root}`。missing/null 均读配置项目；空串按进程 cwd 解析。真实 TS 的传递源码哈希随金样入库。

Rust `native_process::tests::metadata_cwd::archive_matrix_matches_ts` 用固定用例名驱动36个独立子进程，每个30秒上限且需唯一回执；每格有同名 ignored worker。固定用例名决定输入并逐字段核对金样维度。30格与 TS 原件比较；empty-other/deleted 的六格保留 native 单项目 gate，单独登记 `specs/gateway-parity/diffs/A7.2-NATIVE-ARCHIVE-CWD-001.md`，不称为 TS 对齐。ignored `archive_regenerates_identically` 逐字节重生成整个金样。

负控复用 `negative_process.mutating_sources/run_owned/wait_for_slot` 及 `negative_suite.matrix_main`，要求源码与 HEAD 一致；必须由调用方设置 CARGO_TARGET_DIR。命令：`python3 negative_controls.py --check`；`python3 negative_controls.py --compile-only <新目录>`；`python3 negative_controls.py <新目录> [变异名]`。每个变异先编译，再按完整测试名和专属失败消息/nonce 回执判杀，恢复后重跑正控，全部跑完汇总。长套件交协调者提交后宿主执行。

`negative_regeneration.py <新目录>` 将同一个 archive-node-version 变异绑定实际 ignored 重生成守卫；先由主脚本 compile-only 编译该变异。需要 PDRS_BASELINE 与 CARGO_TARGET_DIR；不接受新文件/未提交源码绕过 HEAD 守卫。没有修改共享执行器，也未纳入 generator_provenance 集中清单。
