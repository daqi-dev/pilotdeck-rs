# #1086 P06 resume 与 plain 终态

在 P05 的 control/query 双连接路径上扩展，不重复实现 transport。`sdk_resume_exit.rs` 对真实 CLI 子进程重放原件：逐字核请求（仅请求 ID / run UUID 按结构字段绑定）、stdout/stderr、退出码/信号及完整隔离 runtime 落盘。`sdk-run` 的既有正控继续运行。

## 金样来源与边界

- `source.json` 包含 50 格已有 Node v22.22.0 宿主原件（`case/result/stdout/stderr/events/initial/final/mock/sandbox`）与本包 93 格真实 `runSdkCli`/SDK 函数探针。基线固定为 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，源码和 dist 哈希保存在 supplemental 元数据。
- `generate.mjs` 独占写出 `oracle.json`，强制 50 + 93 行，保留原始 wire/输出与 digest。`#[ignore] terminal_oracle_regenerates_identically` 用指定 Node 版本重新提取并逐字比较；它是原件提取守卫，不冒充重新执行 TS。
- `probe.mjs` 是本包新增真实 TS 观测入口，仅以 EventTarget WebSocket double 替换网络边界。只接受 `ws://127.0.0.1:28999/ws`；其他地址抛 EPERM。它不创建真实 socket，也不证明 OS 对 18789 的权限。Node bootstrap 警告在 import 后排空；原始 stderr 保留，仅按已有 normalization §6 剥开头两行。
- 50 格宿主原件来自协调者的 `_tmp/20261028-issue-123-d1/plan/raw/host-b*/`。93 格函数探针补足旧宿主 `/dev/stdin` EPERM 无法说明的 prompt/输入行为，以及结构化输出和错误边界。函数探针的进程退出不等于真实 TS WebSocket 资源生命周期。
- 特别地，两命令的 `query-resume-rejected`、`response-error`、`failed-code-conversion`、`failed-message-conversion`、`unknown-message-conversion` 共 10 格，TS double 留有 query 连接。这些格仅证明函数输出投影；真实 socket 退出/清理对齐仍须 P08 验证，不能算 D0 CLI 生命周期验收。

## 状态覆盖

| 状态 / 事件 | 格与判据 |
|---|---|
| preflight / control | auth、URL、缺 session 顺序；首 resume 回复后才读 prompt；原始 ID 保留；query 再 resume；完整请求顺序 |
| prompt | 空白/空值、EOF、普通文本、JSON/JSONL 字面 stdin；写 stdin 只容忍 BrokenPipe |
| 前驱拒绝 | 两命令 RPC 拒绝先于空 prompt 验证，stderr / exit 1 |
| 前驱未决 | 两命令各 12 秒未退出；请求收到后的屏障保持真实 mock socket；观察后独立 SIGKILL 清理，不把清理 signal 当 TS exit |
| 前驱取消 | 两命令 SIGINT / TERM 在前驱 request hook 到达后发送；实际 signal 2 / 15，无 abort RPC |
| completed | 无文本零字节成功；structured 的 nullish 回退、空串/false/0/数组/键序/UTF-16；首终态固定，读到流真正结束 |
| failed | 完整 enumerable Error JSON（无 message）；details 对象/数组与标量回退；code/message 的 String 转换及抛错 |
| result_unknown | 缺终态与显式错误码；recovery UUID 与请求绑定；公共 turn.started、UTF-16、首次 error 终态冻结 ID |
| transport exception | close（含 completed 后断流）与 response error 走 stderr，不伪装 stdout completed/unknown |

JSON/JSONL **输出**及 query 活跃取消仍为 P07/P08；JSON/JSONL **输入**在本包视作 prompt 字面文本。P05 已记录的 model 初始化异常形状、无 project 的 model、慢 stdin/control 轮询缺口仍保留，不能据此声称所有 `resume--*` 或 D1 已完成。

## 重生成与验证

```sh
node src/pilotdeck-cli/tests/fixtures/sdk-terminal/probe.mjs NEW_PROBE_DIRECTORY
node src/pilotdeck-cli/tests/fixtures/sdk-terminal/generate.mjs NEW_EXTRACT_DIRECTORY
scripts/cargo-slots.sh
export CARGO_TARGET_DIR="$PWD/_build/cargo-target"
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-cli --test sdk_resume_exit -- --include-ignored --test-threads=1 --show-output
```

生成目录递归创建，每个输出文件用 `wx` 独占写入；重新采集不是自动覆盖已提交金样，须先审原件。恢复 stdout 只实例化期望末尾 JSON 的 `recovery.runId`，实际 stdout 不改写；任意文本中的 UUID、路径或键序不做全局替换。宿主超时后的 close 留在 `cleanupWire`，不作为业务事件重放。

## 负控

脚本位于 `src/pilotdeck-gateway/tests/fixtures/sdk-terminal/negative_controls.py`（沿 P05 布局，使 main anchor selfcheck 可以发现），29 个源码编译期分支/值变异。包含失败/未知误判 completed、首次终态覆盖、runId 快照、structured nullish、错误转换、两次 resume/ID/prompt、提前请求超时、零行 oracle、重生成漂移和 stdin 写入。无运行时环境行为开关。

```sh
python3 -B src/pilotdeck-gateway/tests/fixtures/sdk-terminal/negative_controls.py --check
# 先由协调者提交源码；守卫要求测试、原件、脚本和产品源码均等于 HEAD。
python3 -B src/pilotdeck-gateway/tests/fixtures/sdk-terminal/negative_controls.py --compile-only NEW_COMPILE_DIRECTORY
python3 -B src/pilotdeck-gateway/tests/fixtures/sdk-terminal/negative_controls.py NEW_NEGATIVE_DIRECTORY
```

脚本要求调用方提供 `CARGO_TARGET_DIR`，使用共享 `mutating_sources/run_owned/wait_for_slot`。每项先正控、应用变异、no-run、nonce 收据判杀、恢复与正控复验；全部变异运行后原子发布 KILLED/SURVIVED/INVALID 汇总。锚点过期、HEAD 不匹配和编译失败均非 KILLED。本包未提交时守卫正确拒跑；宿主逐项编译和判杀是交付后的必做接续，不以静态检查替代。
