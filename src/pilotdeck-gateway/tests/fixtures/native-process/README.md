# #1071 原生进程夹具

`pilotdeck-gateway --native` 显式启动单 project 的 Rust 宿主；无该首参数时保留 A4 代理入口。当前生产装配支持文本主代理、原生只读工具、默认空 provider LSP、模型标题、session JSONL、run registry 与取消。配置的 MCP、plugins/skills/hooks、subagents、附件及每轮 model/SDK override 尚未装配，入口明确拒绝相应请求。它不是完整 native 格验收结论。

启动参数均显式提供。home 与 project 是已存在的绝对目录，二者不得相同或互相包含；token 文件非空；sidecar 为绝对文件路径；监听仅接受 `127.0.0.1` 的高位端口或 `0`。`--sidecar-arg` 可重复，`--drain-ms` 默认 5000、范围 1..300000。配置从隔离 home/project 加载，继承的 `PILOTDECK_CONFIG_PATH`、代理 upstream、sidecar TCP 地址被移除；另有 managed/storage/snapshot 根时拒绝启动。

```sh
pilotdeck-gateway --native \
  --home /absolute/isolated-home --project /absolute/project \
  --listen 127.0.0.1:0 --token-file /absolute/isolated-home/token \
  --sidecar /absolute/target/debug/pilotdeck-agent-loop-sidecar
```

`generate.mts` 仅以 Node v22.22.0 执行固定 TS 基线的真实 shutdown coordinator/bundles、默认 LSP、title 和 sidecar turn composition。信号子进程 `signal.mts` 接入真实 coordinator，未启动完整 TS CLI。`oracle.json` 包含基线 commit、传递源码 SHA 与原始观测；Rust 忽略测试 `native_process::tests::regenerates_identically` 可逐字节重生成核验。

`process.mts` 用真实 gateway/sidecar 二进制和仅本机临时 HTTP provider：WS 登录、文本轮次、落盘、run 查询、挂起流取消、SIGINT/SIGTERM、关停期间重复信号、启动失败、A4 非 UTF-8 字节透传。所有监听使用 OS 分配端口，每次新建 home/project。进程 argv、PID、二进制前后 SHA、消息及落盘 SHA 写入独占输出目录。此记录没有覆盖短命后代的 exec/网络见证，不能替代 `session_domain.py` 的 native 审计。

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

```sh
python3 -B src/pilotdeck-gateway/tests/fixtures/native-process/negative_controls.py --check
python3 -B src/pilotdeck-gateway/tests/fixtures/native-process/negative_controls.py /absolute/new-evidence-directory
```
