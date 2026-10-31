# A5.3 原生 Module-host 双配置实测

本探针运行真实 Rust `HostClient → ModuleRouter → RouterRuntime → NativeModelInvocationProvider → HyperModelTransport`，分别接固定 TS sidecar 和 Rust sidecar。只用受控 loopback HTTP 服务替代外部模型服务；服务端独立记录请求 body、次数和返回内容。不会连接用户正在运行的 gateway，也不读取用户 HOME/PILOT_HOME。

这是一项尚未完成生产入口接管的 A5.3 实测装置。`ws/http/background`、101 个 method 的四类外部入口分支、两格 TS host 和 retirement 完整 exec/egress/storage 观察均显式 `NOT_RUN`。Module 调用不能替代这些入口的验收；本探针不能关闭 A5.3、A7 或 A8。

## 运行

从仓库 `src/` 执行；`CARGO_TARGET_DIR` 应为本工作树独占目录。先按项目规则检查 cargo 槽位。

```sh
cargo build -p pilotdeck-gateway --example a53_native_host
cargo build -p pilotdeck-transport --bin pilotdeck-agent-loop-sidecar
python3 -I pilotdeck-gateway/tests/fixtures/native-topology/run.py \
  --baseline /absolute/path/to/PilotDeck \
  --node /absolute/path/to/node \
  --sidecar "$CARGO_TARGET_DIR/debug/pilotdeck-agent-loop-sidecar" \
  --probe "$CARGO_TARGET_DIR/debug/examples/a53_native_host" \
  --output /absolute/path/to/new-evidence-directory
```

必须带 `-I`：冻结装置按显式路径加载（#1066 入口守卫拒绝其目录出现在 sys.path），本采集器与每个入口同守卫。

基线固定 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，使用其已有 tsx loader，不下载二进制。运行前后逐文件校验基线 `src/` 与 `package.json` 的实际字节，并记录源文件、执行文件和比较器 SHA256；没有把 node_modules 全目录当成已校验源码。使用现有 `common.parity_lock`，不并行运行全量对拍；28000 段中的 28500–28999 仅绑定一次属于本进程的高位端口。

两种配置各执行四案×五轮，共 40 次：正常 streaming、provider HTTP 401、provider 未配置、token-saver judge complete 后选择 economical streaming。每轮的外部服务 challenge 必须精确出现在 assistant 输出。judge 必须有 `stream:false/model:judge` 请求，之后必须实际请求 economical；失败不能用默认路由冒充成功。HTTP 401 比较 code/message；stream 的 `authentication_error` 与 complete 的 `auth_error` 来自 TS 不同规范化路径，后者另见 `../native-judge/`。

每案保存配置、原始 NDJSON、stderr、HTTP 观察、进程 PID/PPID/PGID/启动时间/命令以及隔离目录；第二种配置还保存完整比较输入。比较器复用现有 `compare.Normalizer` 的实体与时钟规则，仅额外剔除 Module transport 的不透明 messageId/streamId，原始值仍保留。事件类型、顺序、终态、运行身份、结果、错误、实际 HTTP 和 Module 操作均参与核验。HostClient 故意不转发普通 `turn_completed`，因此末尾权威 terminal 前的一个 sequence 缺口是明确规则，其他缺帧/重帧/乱序均拒绝。

`report.json` 分开表示 `moduleMatrixStatus` 和总体 `status`。40 次与 20 对比较完整通过时，前者为 PASS；总体仍为 NOT_RUN，退出码 **2**。任一已执行案、源码校验或观察清理失败则为 FAIL，退出码 **1**。不会输出代表完整 A5.3 成功的退出码 0。首次目录创建等无法写报告的错误也返回非零。

进程抽样只证明已观察到的后代及其清理，不能发现一切短命 helper。探针启动握手前安装 SIGINT；取消会触发 HostClient 的进程组清理。Python 仅清理由 PID 与启动时间共同确认的自有后代，最终核查已观察进程退出；不把这些证据升级为完整 native 退役审计。

## 取消与清理（#1168）

采集器在取租约前预装 SIGINT/SIGTERM 取消处理器（与 `native-witness/collect.py` 的 `lease_signals` 同形），退出时恢复原处理器。两个发布窗原子化：probe 的 `Popen`、`LIVE_GROUPS` 登记与首次后代清点在同一个 deferred-signal 窗内完成（`session_domain.py` #209 E1 形状），Provider 构造到 `Thread.start` 同理；probe 用 `preexec_fn` 解除继承的屏蔽掩码，自身的 SIGINT 取消不被父窗影响。两处清理窗（`run_probe` 的 `finally` 与 `run` 的 provider 清理 `finally`）不可中断：pending 的取消只在清理审计与报告发布完成后投递。取消一律走既有 `finally` 链并退出码 **1**（报告 FAIL），绝不以 0/2 离场；`LIVE_GROUPS` 登记探针组与自建组的组长，清理失败时由租约以 `cleanupPending` 如实发布残留组。

`test_signals.py` 以真实子进程矩阵逐格复验上述保证（15 相位 × INT/TERM，钩子在目标相位真实 `os.kill`）；`negative_signals.py` 逐项变异源码并要求对应格以自己的 #1168 消息失败（提交后在宿主跑：`python3 -I src/pilotdeck-gateway/tests/fixtures/native-topology/negative_signals.py <新证据目录>`）。

## 负控与发现的产品差异

`controls.py --capture <本次输出目录> --output <新证据目录>` 在真实录制数据上做定向变异，检查各变异由对应规则拒绝，并检查 40 轮及 20 对比较的非空完整性。两侧产生相同坏输出也必须失败。

每个负控只接受预先指定的规则前缀（例如 `MATRIX_COVERAGE`、`EVENT_ORDER`、`CONTENT`）；任意其他异常均判 FAIL，不能以“程序报错”冒充识别了指定问题。其他变异逐侧验证拒绝，共错 content 案额外证明两侧坏输出仍相等。

源矩阵必须有 `report.json` 且 `moduleMatrixStatus=PASS` 才可作负控输入；总体 NOT_RUN 不会因此变成完成。另用临时自建 Git 树分别验证正常源身份、同字节文件/目录 symlink、未跟踪及 ignored 源文件拒绝，源树必须是精确匹配的普通文件集合。这些自建源身份控制与 40 份真实拓扑正控分别记账，不算额外原生入口实测。

真实双配置发现 Rust 普通模型失败把内部 status.text 泄漏到公共 `agent_status`。TS `AgentLoop.ts:383–392` 的事件投影只保留 event/detail；`pilotdeck-agent/src/turn.rs` 改用相同投影，回归测试保留 status 的晚构造顺序和 detail。另一个真实缺陷是 complete 把 Provider 错误降为 Other，修复及金样见 `../native-judge/README.md`。
