# #1071 原生进程夹具

`pilotdeck-gateway --native` 显式启动单 project 的 Rust 宿主；无该首参数时保留 A4 代理入口。当前生产装配支持文本主代理、原生只读工具、默认空 provider LSP、模型标题、session JSONL、run registry 与取消。配置的 MCP、plugins/skills/hooks、subagents、附件及每轮 model/SDK override 尚未装配，入口明确拒绝相应请求。它不是完整 native 格验收结论。

启动参数均显式提供。home 与 project 是已存在的绝对目录，二者不得相同或互相包含；token 文件非空；sidecar 为绝对文件路径；监听仅接受 `127.0.0.1` 的高位端口或 `0`。`--sidecar-arg` 可重复，`--drain-ms` 默认 5000、范围 1..300000。配置从隔离 home/project 加载，继承的 `PILOTDECK_CONFIG_PATH`、代理 upstream、sidecar TCP 地址被移除；另有 managed/storage/snapshot 根时拒绝启动。

token 仅移除末尾 CR/LF，保留空格与 tab；全部为空白则拒绝启动，客户端须发送处理后的精确 token。环境键值以有损 UTF-8 捕获，非法字节替换为 U+FFFD，因此不保证含非 UTF-8 字节的路径仍可访问；此处说明既有行为，不改变鉴权或路径规则。

```sh
pilotdeck-gateway --native \
  --home /absolute/isolated-home --project /absolute/project \
  --listen 127.0.0.1:0 --token-file /absolute/isolated-home/token \
  --sidecar /absolute/target/debug/pilotdeck-agent-loop-sidecar
```

`generate.mts` 仅以 Node v22.22.0 执行固定 TS 基线的真实 shutdown coordinator/bundles、默认 LSP、title 和 sidecar turn composition。信号子进程 `signal.mts` 接入真实 coordinator，未启动完整 TS CLI。`oracle.json` 包含基线 commit、传递源码 SHA 与原始观测；Rust 忽略测试 `native_process::tests::regenerates_identically` 可逐字节重生成核验。

`process.mts` 用真实 gateway/sidecar 二进制和仅本机临时 HTTP provider：WS 登录、文本轮次、落盘、run 查询、挂起流取消、SIGINT/SIGTERM、关停期间重复信号、启动失败、A4 非 UTF-8 字节透传。所有监听使用 OS 分配端口，每次新建 home/project。进程 argv、PID、二进制前后 SHA、消息及落盘 SHA 写入独占输出目录。此记录没有覆盖短命后代的 exec/网络见证，不能替代 `session_domain.py` 的 native 审计。

YAML 顶层 `hooks` 在固定 TS 配置层是未知字段，会被投影丢弃；本入口按该诊断明确拒绝该字段（包括空值），不把它当作已装配的 hooks。实际 hooks 来源是插件贡献和 SDK session hooks：磁盘 plugins/skills 非空被拒绝，内置插件为空，`sdkConfig` 请求被拒绝。每轮既有 `prepare_attachment_turn` 会重新校验 profile，进程测试另验证启动后加入 hooks 被拒绝。

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

`--check` 将断言限定在对应文件的目标测试函数体内，并有兄弟函数移动断言的内存负例。守卫覆盖 process/signal/generate/oracle 与共享重生成依赖，summary 记录 `guardedSha256`。运行进程用例前显式构建当前源码的 sidecar，记录构建命令、日志和 `sidecarSha256`。取消断连由生产 `turn_ports` 的确定性信号状态用例判杀；信号退出码由 Node 的具体断言判杀，进程超时不算杀。Node 仅在二进制完整性检查通过后发布断言记录。重生成负控修改 oracle 字节，只承认对应字节比较失败。

```sh
python3 -B src/pilotdeck-gateway/tests/fixtures/native-process/negative_controls.py --check
python3 -B src/pilotdeck-gateway/tests/fixtures/native-process/negative_controls.py /absolute/new-evidence-directory
```
