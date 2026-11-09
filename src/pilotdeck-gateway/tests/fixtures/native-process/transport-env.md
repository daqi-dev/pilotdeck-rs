# Native CLI legacy transport 环境选择

固定TS cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，真实 AgentLoopDeploymentProfile.ts 模块导入后调用 resolveAgentLoopDeploymentProfile。22格包含默认/native/stdio/tcp、大小写、FEFF与非JS空白NEL、command优先级、raw path空白、child env删除TCP_HOST/TCP_PORT、其余env保留、整数Number语法和精确错误。原件SHA d7f9066d974e68c69e054c86a4ae53d978157be027211eb16ac5da5b1d11a626。

仅两项在首次采集前定为安装身份投影：实际Node execPath→$NODE_EXEC，基线默认JS sidecar路径→$DEFAULT_SIDECAR；显式路径空白、env、错误全保原值。Rust测试比较profile序列化投影，不把JS Object.freeze误作Rust运行功能。

纯Rust安装的Native/无显式override stdio继续使用已安装Rust sidecar与既有args。显式COMMAND为真实可执行command，传一个选中的path；只PATH选择.js/.mjs/.cjs时使用注入PATH上的系统node，无node明确错误、不下载；只PATH为native executable时直接运行该文件且不把它当成另一个sidecar会忽略的参数（这是native扩展）。普通启动不引入Node依赖。显式stdio的子环境去掉两项TCP地址变量；其余变量保留。TCP客户端复用共享有界host引擎，不能借此宣称Rust sidecar监听端或默认resume已经实现。

capture-transport-env.mjs为自写transpile-only loader，只有baseline文件在内存转换，无复制baseline源码/子进程。运行前必须在sandbox外 git -C BASELINE rev-parse HEAD核固定SHA并设置PDRS_VERIFIED_TS_SHA。Node v22.22.0/ICU78.2/TS5.9.3固定；参数 BASELINE_ABS NEW_OUT_ABS，output父目录已存在、output必须不存在且不在baseline内。

Darwin复现使用sandbox-exec deny network*、deny process-fork、deny file-write*，只allow独占scratch写入；HOME/PILOT_HOME/TMPDIR/cwd全部指向该scratch内隔离目录。sandbox缺失明确fail，不裸跑。命令形状：

  PDRS_VERIFIED_TS_SHA=已核SHA HOME=隔离home PILOT_HOME=隔离home TMPDIR=隔离tmp LANG=en_US.UTF-8 /usr/bin/sandbox-exec -f 独占profile NODE_ABS capture-transport-env.mjs BASELINE_ABS NEW_OUT_ABS

实际导入图会触发一次Node内建SQLite ExperimentalWarning；策略在本采集前已定为恰一个完全匹配warning并保留在oracle.metadata.warnings和原stderr，其他或重复warning失败。没有启动SQLite数据库或provider/module/sidecar。metadata记录实际462个TS依赖文件SHA。

stdio候选原始补丁使用Python splitlines时把U+0085误当行结束，git apply拒绝；原件保留，修复仅用真实git diff --no-index重新生成，五源与manifest逐字不变。所有后续patch按Git LF规则并校验git apply --numstat非空精确文件数；不改变oracle使之通过。
