# #1071 原生进程夹具

pilotdeck-gateway --native 显式启动单 project 的 Rust 宿主；无该首参数时保留 A4 代理入口。原生装配已接入文本主代理、文件读写与执行工具（含 execute_code 的完整嵌套工具门禁）、交互权限、模型标题、session/run 持久化、history/恢复及取消；MCP 会话工具、独立 skills 贡献、原生 subagent fork、附件与已支持的 model/SDK overrides 由各自产品模块接线并有专项测试。仍未装配的 module bindings 与styles/hooks 之外的插件贡献家族会明确拒绝，不能把旧“全部集成拒绝”当成当前产品合同。SDK 特殊 hooks 事件与后台任务相关补齐仍在进行，本页不宣称完整 CLI/全部 native 格已经验收。实际机器能力以 describe_server/注册方法和相应专项合同为准。

启动参数均显式提供。home 与 project 必须指向已存在目录；argv 可给相对路径，按进程 cwd 词法解析为绝对 project 身份，物理规范路径另用于相同/互含隔离检查，不能把 symlink 身份偷换成 realpath；token 文件非空；sidecar 必须指向现存文件，内部规范为绝对路径；监听仅接受 `127.0.0.1` 的高位端口或 `0`。`--sidecar-arg` 可重复，`--drain-ms` 默认 5000、范围 1..300000。`--storage-root` 可选：给出时按原样注入 `PILOTDECK_STORAGE_ROOT` 并启用 snapshot 能力，缺省或空白值保持 snapshot 关闭（生产默认关闭，见决策卡 `state/decisions-20261101-1231-u3-scope.md` 选项 B）；非 UTF-8 值按启动参数硬错误拒绝；与 home/project 不同，相对值按进程 cwd 解析，不要求绝对或已存在。配置从隔离 home/project 加载，继承的 `PILOTDECK_CONFIG_PATH`、代理 upstream、sidecar TCP 地址被移除；未给 `--storage-root` 时继承的 `PILOTDECK_STORAGE_ROOT` 与 `PILOTDECK_SNAPSHOT_ROOT`（含空白值）一并移除，给出时只注入 `PILOTDECK_STORAGE_ROOT`、继承的 `PILOTDECK_SNAPSHOT_ROOT` 原样保留。继承到非空的 managed/storage/snapshot 根时拒绝启动（校验先于 `Options::environment`，所以「继承的 `PILOTDECK_SNAPSHOT_ROOT` 原样保留」只在直接调用该函数时可见；经入口时能活下来的继承 snapshot 根只有空值，非空值一律先拒绝）。

token 仅移除末尾 CR/LF，保留空格与 tab；全部为空白则拒绝启动，客户端须发送处理后的精确 token。环境键值以有损 UTF-8 捕获，非法字节替换为 U+FFFD，因此不保证含非 UTF-8 字节的路径仍可访问；此处说明既有行为，不改变鉴权或路径规则。

```sh
pilotdeck-gateway --native \
  --home /absolute/isolated-home --project /absolute/project \
  --listen 127.0.0.1:0 --token-file /absolute/isolated-home/token \
  --sidecar /absolute/target/debug/pilotdeck-agent-loop-sidecar
```

`generate.mts` 仅以 Node v22.22.0 执行固定 TS 基线的真实 shutdown coordinator/bundles、默认 LSP、title 和 sidecar turn composition。信号子进程 `signal.mts` 接入真实 coordinator，未启动完整 TS CLI。`oracle.json` 包含基线 commit、传递源码 SHA 与原始观测；Rust 忽略测试 `native_process::tests::regenerates_identically` 可逐字节重生成核验。

`process.mts` 用真实 gateway/sidecar 二进制和仅本机临时 HTTP provider：WS 登录、文本轮次、落盘、run 查询、挂起流取消、SIGINT/SIGTERM、关停期间重复信号、启动失败、A4 非 UTF-8 字节透传。所有监听使用 OS 分配端口，每次新建 home/project。子进程就绪按入口分流：native 入口在 stderr 的 `native listening host:port` 整行出现时 resolve；A4 入口在 stdout 的 `PilotDeck server listening: http://host:port` 整行出现时 resolve（#1131 起 A4 读 `local_addr()` 打印内核实际端口，与 TS 对齐，差异账本更正见 `specs/gateway-parity/diffs/A4-LISTEN-PORT-LOG-001-R1.md`）。子进程输出会按任意边界分块，半行地址不再被采纳，避免无端口 URL 落到 http 80；resolve 值必须是完整 `IPv4:port`（专属断言 `#1071 readiness address carries a port`），且因两个入口都以 `:0` 请求，端口必须非零（专属断言 `#1131 readiness reports the kernel-assigned port`）。#1071 时代的 A4 预留端口与 bind 冲突有界重试已随该产品修复移除；native 子进程自行 bind `:0` 回报真实地址、不重试。除已登记的具名断言外，其余失败（超时、别的场景错误）保持未分类。WS 连接在 open 前即挂 error 监听，连接失败走有界拒绝而非未捕获异常。进程 argv、PID、二进制前后 SHA、消息及落盘 SHA 写入独占输出目录。此记录没有覆盖短命后代的 exec/网络见证，不能替代 `session_domain.py` 的 native 审计。

YAML 顶层 hooks 在固定 TS 配置层是未知字段，生产 NativeConfigSource 投影会丢弃它；空值与非空值均不能据此成为可执行 hooks。top_level_hooks_are_ignored_like_ts 除启动接纳外，核对真实配置/agent 投影没有该字段，并构造实际 SessionTools/turn_ports，确认未注入 interaction.hooks 或 context_config 的 hook authority。执行 hooks 的来源是受支持、被明确提供的插件贡献与 SDK 会话配置，不是该未知字段；SDK 特殊 hooks 事件的剩余范围仍由专项实现/验收跟踪。standalone skills 已支持；styles/hooks 之外的插件贡献家族仍受 profile guard 限制。prepare_attachment_turn 仍重验当前 profile，不能将旧“加入顶层 hooks 一律拒绝”的行为继续作为预期。

`default_mode_unchanged` 是源码守卫：精确移除唯一 opt-in 片段后计算 A4 源码 SHA；它不等同运行时行为证明。`explicit_entry_selects_parser` 启动真实二进制，以 native 专属参数错误和 A4 地址错误区分分支，涵盖首参、非首参及 `--native=true`。

清理矩阵调用真实 TS `LocalGatewayLifecycleBundle`，覆盖成功、单错、双错、前驱释放/取消/永不完成。Rust `production_cleanup_matrix` 走真实生产装配，在实际资源 dispose 后注入故障，核对顺序、首错、project 诊断和 deadline。TS 吞错、Rust 返回首错仍是未批准差异，见 `specs/gateway-parity/diffs/A7.2-NATIVE-PROCESS-SHUTDOWN-001-R1.md`。

```sh
# 从仓库根运行；每次 cargo 前先运行 scripts/cargo-slots.sh。
export CARGO_TARGET_DIR="$PWD/_build/cargo-target"
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
export PDRS_BASELINE=/absolute/read-only/PilotDeck
cargo build --manifest-path src/Cargo.toml -p pilotdeck-gateway --bin pilotdeck-gateway -p pilotdeck-transport --bin pilotdeck-agent-loop-sidecar
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib native_process::tests -- --include-ignored
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --test native_process executable_native_and_a4 -- --exact --include-ignored --nocapture
```

负控仅在提交后的干净 HEAD 上运行；不得为运行它创建影子仓库或替换守卫。脚本要求调用方提供 `CARGO_TARGET_DIR`，逐个记录编译、正控、带 nonce 的目标断言失败、恢复正控，跑完所选变异后统一汇总；任何 INVALID/SURVIVED 均失败。`--check` 不改源码，锚点过期报错；`--compile-only` 是协调者的编译预检，不能视为 KILLED。完整变异可能超过 20 分钟，由宿主协调者运行。

`--check` 将断言限定在对应文件的目标测试函数体内（包括同缩进的既有嵌套测试模块），并有顶层/嵌套兄弟函数移动断言的内存负例。守卫覆盖 process/signal/generate/oracle 与共享重生成依赖，summary 记录 `guardedSha256`。运行进程用例前显式构建当前源码的 sidecar，记录构建命令、日志和 `sidecarSha256`。取消断连由生产 `turn_ports` 的确定性信号状态用例判杀；信号退出码由 Node 的具体断言判杀，进程超时不算杀。`readiness-address-port`（native stderr 正则）与 `a4-readiness-address-port`（A4 stdout 正则）各绑定观察者的 `#1071 executable readiness address carries a port`，`a4-requested-address`（产品回退请求地址）绑定 `#1131 executable readiness reports the kernel-assigned port`；非零退出本身不是判杀依据，别的场景失败或超时只能 INVALID。Node 仅在二进制完整性检查通过后发布断言记录。重生成负控修改 oracle 字节，只承认对应字节比较失败。

```sh
python3 -B src/pilotdeck-gateway/tests/fixtures/native-process/negative_controls.py --check
python3 -B src/pilotdeck-gateway/tests/fixtures/native-process/negative_controls.py /absolute/new-evidence-directory
```


本轮七条产品负控保持原 mutation ID 并重绑当前真实行为，没有删除旧 mutation、改判杀语义或修改任何 FROZEN 登记件。configured-hooks 反向注入旧的未知字段拒绝，绑定接纳/隔离合同；configured-modules 跳过当前 module guard，合法 loader 输入的启用模块仍须被未装配的 native profile 拒绝，已支持的禁用模块接纳继续保留；configured-mcp 丢失 per-session MCP 真实注册必须被原注册 nonce 抓住；configured-contributions 丢失真实 skill 贡献内容/工具登记必须被新 nonce 抓住；configured-subagents 重新拒绝已支持配置必须失败；cleanup-skips-project-after-error 与 signal-exit-status 分别追踪当前 dispose/observer 顺序及 server_host 的成功关停出口。71 条现有变异都保留；--check 只核绑定，绝不等于实际 KILLED。真实编译、目标 nonce 判杀、恢复正控与 source guard 必须由协调者在干净提交后执行，并保留逐条原始结果。
