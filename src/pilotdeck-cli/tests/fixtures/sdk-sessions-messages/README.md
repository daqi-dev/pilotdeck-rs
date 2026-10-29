# #1028 sessions messages

`oracle.json` 保存 52 格真实 Node v22.22.0 CLI 观测：D1 规划中的全部 39 个 `sessions-messages--*` 格，加 13 个返回形状/ID 边界格。每格比较完整 stdout、stderr、exit、signal，同时检查 hello/RPC 参数、连接结束及完整落盘。非空数组中的所有消息字段均保留；空数组、缺 messages、null、false、0、空串、对象以及整个 result 缺失/null 都有实录。

`source.json` 是可移植的原件输入。前 39 格来自协调者的 D0 宿主实录，保存场景定义、原始 stdout/stderr、退出/信号、原始 WS 帧、完整初末文件树与采集记录；只缩减总场景清单并移除 result 中的宿主 argv/env/profileSha256/processTree 元数据。后 13 格由本目录 `probe.mjs` 执行真实基线 CLI 采集，记录 Node 版本、TS/编译入口 SHA-256、四面、原始 WS 帧和初始文件树；探针在保存前断言最终文件树不变。补充探针使用隔离 home、动态本地端口和每格 12 秒上限，不充当 D0 内核隔离验收。

`generate.mjs` 只读取自身目录的入库 `source.json`，核原件摘要后机械生成金样，不执行 TS、不访问规划目录。它不需要任何仓外数据；`PDRS_BASELINE` 只在手动重采补充观测时使用。输出目录用 `recursive:true` 创建，文件用 `flag:'wx'` 独占写入。标准守卫以所选 Node 的真实 `--version` 成功结果验证 v22.22.0，再逐字节比较生成结果。

唯一 CLI 输出归一化是 `specs/cli-parity/normalization.md` §6 允许的 TS stderr 开头 SQLite 完整两行。请求 UUID 只在 mock 回放中替换为 `<request>`，CLI 输出不变。补充响应保留原始 JSON 字符串，避免 serde_json 在载入测试定义时拒绝孤立 UTF-16 代理项；这些字符仍通过实际 transport/JS codec 与完整 stdout 比较。

Rust 测试 `tests/sdk_sessions_messages_exit.rs` 原样沿用 P02 的隔离目录、进程回收、wire 回放和有界租约实现；本包范围不允许把 P02 私有助手抽成共享模块，因此只在新文件复用同一实现，不改变租约语义。获取 `/tmp/pdrs-parity.lock` 最多等待 180 秒，释放 guard 最多 2 秒，不回收前驱锁。每格 mock/CLI 最多 15 秒。SIGINT/SIGTERM 在收到 `read_session_messages` 后发送，覆盖请求前驱永不回复时的取消；没有新增 transport 生命周期逻辑。原有 P02 的超时/租约边界测试和 128 MiB WebSocket 上限继续有效。

先设置调用方的 `CARGO_TARGET_DIR`、两个 `line-tables-only` profile，并运行 `scripts/cargo-slots.sh`，然后在 `src/` 执行：

```sh
cargo test --offline -p pilotdeck-cli --test sdk_sessions_messages_exit \
  -- --include-ignored --show-output --test-threads=1
```

ignored `sessions_messages_oracle_regenerates_identically` 调用 `regenerates_identically("sdk-sessions-messages")`。默认使用 PATH 中的 Node，也支持 `NODE22`；无错误版本回落。手动重采补充原件（结果先审核，不自动替换金样）：

```sh
PDRS_BASELINE=/path/to/PilotDeck node \
  src/pilotdeck-cli/tests/fixtures/sdk-sessions-messages/probe.mjs NEW_OUTPUT_DIR
```

负控位于 `src/pilotdeck-gateway/tests/fixtures/sdk-sessions-messages/negative_controls.py`，与 P02 同处以便 main 的 anchor_selfcheck 发现。18 个源码/fixture 变异分别覆盖 RPC、session/project 参数、首 ID、空白 ID、usage、messages 投影/nullish/标量/完整数组、result 错误、成功退出码、52 格覆盖数及金样/原件重生成。三项 `--node-runtime` 控制覆盖 PATH 与 NODE22 版本选择、版本命令非零退出。

源码变异通过共享 `mutating_sources`，所有产品源码、测试、fixture 和执行器必须等于 HEAD；新文件未提交时拒跑。每项顺序执行正控、应用变异、`cargo test --no-run`、负控、恢复正控，使用 `negative_suite` 的当前 nonce 回执与精确测试名/消息判杀；全部执行完再汇总，任一 SURVIVED/INVALID 非零退出。必须使用调用方传入的 `CARGO_TARGET_DIR`，不使用影子仓库或第二构建目录，不使用 quiet/nocapture。预计长于 20 分钟的全量由协调者提交后在宿主执行：

```sh
python3 -B src/pilotdeck-gateway/tests/fixtures/sdk-sessions-messages/negative_controls.py \
  --check
python3 -B src/pilotdeck-gateway/tests/fixtures/sdk-sessions-messages/negative_controls.py \
  NEW_OUTPUT_DIR
```

输出目录拒复用。`--compile-only NEW_OUTPUT_DIR [mutation ...]` 可独立核每个变异可编译；`--node-runtime NEW_OUTPUT_DIR` 不改源码，可在未提交树执行。每个版本信号文件和 Rust nonce 回执均原子发布。
