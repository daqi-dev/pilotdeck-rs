# Metadata 环境初始化矩阵

金样由 Node v22.22.0 直接 import 固定 cc7c4e6 基线原文件生成。`src/cli/LocalGatewayBootConfig.ts:39-45` 的默认 cwd 参数与 PILOT_HOME 解析对应 Metadata 初始化所需的两项输入；真实调用在 `src/cli/createLocalGateway.ts:600`。另分别执行 `src/pilot/paths.ts:20` 的 home helper，明确绝对路径 helper 不读 cwd，而 boot 入口仍读 cwd。没有运行整个 server，也没有用复制的函数体替代基线。

每格单独 Node 子进程，import 后 chdir，删除前不读 process.cwd，包装只计数并调用真实 cwd。HOME 为 scratch；输出仅随机根归一化为 `<root>`。生成器使用共享 commit/源码哈希守卫、recursive mkdir 与 wx 独占输出。`regression.rs` 为 metadata 的私有单测模块，每格由共享有界子进程执行器隔离环境/cwd，验证唯一 case receipt；子进程最多 30 秒，超时失败并回收。测试不写进程全局环境。

相对、~、~/子目录、绝对、空字符串各覆盖正常及删除 cwd。Rust 未设 PILOT_HOME 两格保留 None，属于明确保留的既有安全门，不声称与 TS 默认 home 相同；缺失 HOME 的正常 cwd tilde 格同样保留 None。cwd 错误同时验证 from_env 的 ErrorKind/message 与 pairing 的传播；正常及关闭 metadata 时 pairing 仍成功。Rust io::Error 没有 JS 的 name/code/syscall 属性，回归只比较金样中的消息并另断言 NotFound，不虚构这些属性。

生成：设 `PDRS_BASELINE`，运行 `node --disable-warning=ExperimentalWarning --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/metadata-from-env/generate.mts <新输出目录>`。ignored 守卫为 `metadata::from_env_tests::metadata_from_env_regenerates_identically`。

负控：提交后设置调用方 `CARGO_TARGET_DIR` 和两个 `CARGO_PROFILE_*_DEBUG=line-tables-only`，在树根运行 `python3 src/pilotdeck-gateway/tests/fixtures/metadata-from-env/negative_controls.py <新证据目录> [变异名...]`。脚本只认 HEAD，通过共享 mutating_sources/run_owned/wait_for_slot 保证锁、还原和有界 cargo。新文件/源码未提交时拒跑，不调整守卫；不设 target 直接失败。不建副本仓库或第二构建目录。每项要求全部正控 PASS → 指定矩阵运行时断言 RED → 还原 PASS，编译错与准备失败不计击杀。建议每批 1–2 项，超过 20 分钟的余项由协调者宿主跑。

| 变异 | 配对断言 |
|---|---|
| cwd-none | 5 个显式 PILOT_HOME 的删除 cwd 格 |
| cwd-message | 同 5 格保留 uv_cwd 消息 |
| normal-home-layout | 相对、~、~/子目录、绝对、空字符串的正常 home |
| normal-cwd-layout | 同 5 格的正常 cwd |
| unset-default-home | 未设 PILOT_HOME 的正常/删除格均 None |
| missing-home-default | 未设 HOME 的 tilde 正常格仍 None |
| pairing-swallow | 5 个删除格从直接调用方传播失败 |
