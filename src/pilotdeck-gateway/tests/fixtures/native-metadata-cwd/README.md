# #1154 原生 Metadata cwd

`generate.mts` 在 Node v22.22.0、只读 cc7c4e6 基线实跑 GatewaySessionModelBundle 两个消费者、InProcessGateway 的列表方法、listProjects/paths 存储查找，以及 ProjectRuntimeRegistry.setSessionCwd。八格为 relative-project、relative-other、absolute-other、absolute-deleted、relative-deleted、symlink-scan、collision-deleted、collision-other。symlink-scan 区分注册扫描与完整枚举；collision-deleted/other 分别验证异常整环捕获与正常命中非 direct ID。`oracle.json` 记录响应的稳定投影（ok/source、错误 code/message、列表长度及文件相对路径、存储选择布尔），仅把临时根替换成 `{root}`，不保留时间戳。生成器递归创建输出目录，oracle 使用 `flag: 'wx'`，finally 恢复 cwd 并清理自有临时目录，不调用 provider。

Rust：`native_process::tests::metadata_cwd::matrix_matches_ts` 有界驱动八个独立进程；每格穿过真实 Requests.request / Services.resolve_model_selection，直接核对 process_chat_dir 的选址。只有 ignored worker 修改 cwd，每个 30 秒期限，成功须有唯一子进程回执。ignored `regenerates_identically` 重生成逐字校验。session cwd 差异见 `specs/gateway-parity/diffs/A7.2-NATIVE-SESSION-CWD-001.md`；原件值负控检查金样完整性，两个 Cwd::set 产品变异检查 native 保留的限制。

负控：调用方必须设置 CARGO_TARGET_DIR；`python3 negative_controls.py --check` 检查唯一锚点；`--compile-only <新目录>` 先逐变异编译；`<新目录>` 跑正控、变异、恢复正控，逐条汇总。matrix_main 内部使用 negative_process.mutating_sources/run_owned/wait_for_slot，不绕过源码=HEAD 检查；任何 stale anchor / 编译失败 / 错误断言均为 INVALID，非零退出。新增文件未提交时交宿主执行。整套重编预计超过 20 分钟，由协调者在宿主执行。

`negative_regeneration.py` 用同一个 `oracle-node-version` 变异绑定实际 ignored 重生成守卫；先由主负控脚本 compile-only 验证该变异能编译，再运行此脚本的正/负/恢复三步，使用共享 negative_suite.run_cases。PDRS_BASELINE 必须显式指向固定 TS 基线。

本装置使用明确的 lib ignored 守卫及独立负控，没有登记到 generator_provenance integration-test 集中清单；仅跑该清单不会覆盖本装置。`--check` 与变异执行串行运行。重生成负控外层同时守卫 runner 与共享执行器字节须等于 HEAD。

fix-r1：chat scanner 重构后的两个锚点已同步；matrix_main 使用 exact=True，只运行绑定的 matrix_matches_ts，避免同模块新增 archive 矩阵改变旧套件的正控计数。empty-matrix 直接注入空 cases 验证拒绝，不只改期望数量。
