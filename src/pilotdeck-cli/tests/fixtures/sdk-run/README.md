# #1058 run 正常路径金样

收敛方向：用真实 Node v22.22.0 观测约束新会话、prompt、双连接、query 初始化、submit 与正常文本；不将整个 run 或 D1 标成完成。

`source.json` 是唯一原件包，`generate.mjs` 机械提取 `oracle.json`；重生成不会重新执行 TS。`sdk_run_exit.rs::run_oracle_regenerates_identically` 为 ignored 守卫，必须显式运行。生成器目录 recursive、文件 wx；目标已存在时拒绝覆盖。

## 两种来源必须区分

1. 36 个完整 CLI 宿主格：来自主仓 `_tmp/20261028-issue-123-d1/plan/raw/host-*` 的 Node v22.22.0 D0 沙箱运行。包含 stdout/stderr、退出/信号、完整磁盘、带连接编号的 wire 和原始结果文件。输出只剥除已登记 SQLite 两行警告，stdout 不归一化。
2. 补充函数探针：`probe.mjs` 在 Node v22.22.0 中导入固定基线 `dist/src/cli/sdkCli.js`，执行真实 `runSdkCli` 与 SDK，并以实现了 EventTarget 的内存 WebSocket 替身提供确定性协议响应。**没有创建真实网络连接，也不是 D0 OS 沙箱 CLI 运行**。替身仅接受 `ws://127.0.0.1:28999/ws`，不接触 18789。stdin 使用真实 `/dev/stdin`，父进程传只读普通文件；输入 JSON/JSONL 按字面传递。含输出、wire、磁盘和六份 TS/src/dist 文件 SHA-256。模型 disabled/ambiguous 两个异常观测保留在 source，但不入成功对拍：double 无真实 socket，不能证明异常生命周期退出。

本环境嵌套 sandbox-exec exit 71，不无沙箱重跑完整 CLI。旧宿主 stdin 格的 EPERM 不能当成功证据；补充探针只证明函数/SDK 的输入与投影行为，新增格的完整 CLI + D0 端口 EPERM 预检仍交协调者补测。

## 比较边界

- CLI 字节、退出码/信号、完整初始/最终磁盘精确比较。仅 stderr 按 `specs/cli-parity/normalization.md §6` 去一次 Node 启动警告。
- 真实 Rust CLI 连接测试只绑定 127.0.0.1:28999；直接沿用 P02/P04 的同一个 `/tmp/pdrs-parity.lock` lease 算法，不抢锁、不回收前驱。180 秒取锁上限、15 秒单格上限；所有子进程 RAII 清理。
- wire 原始 UUID 保存在金样。回放仅绑定实际请求 envelope.id、params.runId 与响应引用；不替换 prompt/输出正文。不改变对象键顺序，逐字节检查 hello/request。外来 stream id 原样保留以验证过滤。
- 孤 UTF-16 surrogate 保留在 `row.raw` 中；只删去 serde_json 无法承载的辅助 `frame.event` 投影，保留 runId。完整事件仍在 source；Rust 的 JS JSON codec 解析真正原始帧。assistant 文本单元先合并，再编码输出。
- control 与 query 分别闭合，完成 payload 不等同流结束；final event 或成功 response 结束流。未实现 JSON/JSONL 输出、异常/structured/unknown 终态与取消仍为 #123 P06–P08 缺口。

## 重现

```sh
export CARGO_TARGET_DIR=/Users/da/ws/pilotdeck-rs/_build/wt-1058/_build/cargo-target
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
scripts/cargo-slots.sh
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-cli --test sdk_run_exit -- --include-ignored --test-threads=1 --show-output
# 函数探针：OUTPUT 必须是新目录；不用于替代宿主 D0 全进程采集
PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck node src/pilotdeck-cli/tests/fixtures/sdk-run/probe.mjs OUTPUT
```

负控位于 `src/pilotdeck-gateway/tests/fixtures/sdk-run/negative_controls.py`，与既有共享执行器/扫描器同目录树。只接受调用方 CARGO_TARGET_DIR，先 no-run 再运行并绑定 nonce 收据，逐项汇总。新增源码未提交时 HEAD 守卫必须拒绝；本包不绕过，提交后由协调者先全量 `--compile-only`，再跑 `drop-stdin` 一个样本，余项按宿主资源安排。

## fix-r1 边界补充

`../sdk-run-validation/` 直接调用真实 SDK 的模型解析与序号处理方法，补 12 个模型格和 12 个序号格。它只证明字段选择/验证，不证明异常 query 的 socket 生命周期。Rust 的两个纯验证函数由实际 run/stream 路径调用；单测逐字比较 Node 观测结果，独立 ignored 重生成守卫重新执行真实 TS。

`run_token_file_reaches_gateway` 持有相同端口租约，验证有效 token 文件进入网络路径后的拒绝连接字节；成功路径仍由 `run--token-file` 原金样回放。旧 preflight 测试不再保留 P02 run 占位断言。

stdin 仍是明确缺口：Rust 在 current_thread 运行时同步 `read_to_end`；control 在等待 stdin 和 query 期间没有独立轮询。TS `sdkCli.ts` 使用异步文件读取且未设置字节上限（本轮仅读码，未实测慢 stdin/心跳/取消）。只换 spawn_blocking 不能补 control 轮询；完整修复须补永不 EOF、取消、ping 和内存策略矩阵，前置于 #123 P06–P08 生命周期验收。本轮不擅增字节上限。

负控表以 `--check` 输出为准。三个产品变异常量已删除；runner 对真实表达式/代码块变异，并增加纯函数模型/序号和 canPrompt/canElicit/旧占位回退配对。HEAD 守卫拒绝本轮未提交新文件时，先提交再由协调者运行 `--compile-only` 全表和负控全表；拒绝不是编译通过或 KILLED。
