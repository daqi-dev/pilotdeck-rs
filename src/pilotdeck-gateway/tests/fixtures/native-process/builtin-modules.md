# 显式内置 modules 配置

固定 TS cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85 的真实 loadPilotConfig 与真实 owner selector 采集。15 格覆盖 absent、6 个单独内置绑定、全部内置绑定、skills/knowledge disabled、未知字段/未知槽、合法 external tools、非法必需 disable 和非法 provider。builtin-modules-oracle.json 投影 config.modules、原诊断和 selector 结果；从首次采集即不包含动态 loadedAt/source path，没有事后归一化。

TS 依据：parseModulesConfig.ts:24,95-121 对所有6个core槽归一为 enabled:true/provider:pilotdeck；runtimePorts.ts:85-104/141 仅 external binding 才覆盖 model/tools/context；AgentLoopDeploymentProfile.ts:createAgentLoopBindingFactory 仅 external agentLoop 才产生替代 factory；ProjectSessionRuntimeBundle.ts:250-267,318-328 对 builtin skills 保留现有贡献、仅 external knowledge 添加 query；createLocalGateway.ts:853-862 对 builtin skills 复用本地 SkillManager。enabled knowledge 的内置形状没有额外凭空创建 query tool，跟 absent 一致。

Rust 只接 normalized 内置形状：6槽白名单、恰两个 own 字段 enabled:true/provider:pilotdeck；skills 和 knowledge 的恰单字段 enabled:false 保持既有处理。不接受 external/custom owner，不删整个 guard，不改变 default owner，也不忽略 selector。现有 production_profile_and_registration 负控已改成真正合法但未装配的 external tools protocol binding，保留 loader.load 成功及 profile 拒绝的原 nonce；negative_controls.py 锚点/条数/判杀规约均不变。

测试 selector：real_ts_builtin_module_profiles_keep_default_owners；同时跑 production_profile_and_registration 与 negative_controls.py --check。新测试比较实际 Rust loader 的 modules 和 diagnostics，再调用生产 profile admission；external binding 断言精确错误，非法配置在 loader 前置失败。完整模型/工具/sidecar能力还应以新 immutable bin 重跑真实 host fixture（可在已有 child MCP probe 配置中显式设置 all builtin），不能用 selector 单测代替实际回合。

采集器 capture-builtin-modules.mjs 为自写可携 AST-less TS transpile loader；baseline 源只在内存转换，不复制进repo，不执行外部模块。参数 BASELINE OUT_DIR，输出父目录需存在、OUT_DIR 必须不存在，且不能在 baseline 内。Node v22.22.0/ICU78.2/TS5.9.3 固定；调用前先 git -C BASELINE rev-parse HEAD 核固定 SHA，再传 PDRS_VERIFIED_TS_SHA；脚本本身不运行 git/子进程。metadata 记录真实导入的462个 TS 文件 SHA。

必须在 Darwin sandbox-exec 下 deny network*、deny process-fork、deny file-write*，仅允许独占 scratch 子树写入。HOME/PILOT_HOME/TMPDIR 均指向 scratch 内新建目录，cwd 也为该 scratch。sandbox 缺失明确失败，不裸跑。命令为：

  PDRS_VERIFIED_TS_SHA=已核固定SHA HOME=独占home PILOT_HOME=独占home TMPDIR=独占tmp LANG=en_US.UTF-8 /usr/bin/sandbox-exec -f 独占profile NODE_ABS capture-builtin-modules.mjs BASELINE_ABS NEW_OUT_ABS

全量 module import 会触发 Node22 自身 SQLite experimental warning；先前 fail-on-any-warning 采样因此失败且原件保留。随后在全15格重采前预先固定策略：恰一个 name=ExperimentalWarning、message=SQLite is an experimental feature and might change at any time，原样写入 metadata.warnings，任何其他/重复 warning 失败。没有启动 SQLite 数据库或 provider/MCP/module网络调用。stderr 原始 warning 不抹去，不能要求它为空。

mbp2 原件目录 _tmp/builtin-modules-sol-20261108；capture-r3 与 reproduced 15格均 exit0、oracle 逐字相同，SHA b419ac52c52838721c01f0c22affe41c3b83ce055a2e4925e5284ac20289734a。portable 路径护栏版本另独占重采验证同一 SHA。capture-r1 语法错误和 capture-r2 warning 策略失败原件均保留，不计通过。Rust 编译由 root 在 mbp2 排，不由采集器执行。
