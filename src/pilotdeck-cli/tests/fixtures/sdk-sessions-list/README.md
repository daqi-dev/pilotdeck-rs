# #983 sessions 默认 / list

`oracle.json` 由 `generate.mjs` 从协调者 Node v22.22.0 实录机械提取，不执行 TS、mock 或 sandbox-exec。必须用 `PDRS_D1_PLAN` 指定原始实录或归档副本的根目录；未设或为空即带 `#983 set PDRS_D1_PLAN` 消息失败，不使用默认路径。Rust 重生成守卫显式把它传给 Node 子进程；原始批次见该目录 `HOST-RESULTS.md`。收录 `sessions-default--*`、`sessions-list--*` 共 76 格，另有 `sessions-unknown--argv`。

每格记录输入、原件相对路径、文件 SHA-256、原始 stderr、比较视图、退出/信号、原始顺序的 WS 帧、初始完整文件树。仅按 `specs/cli-parity/normalization.md` §6 去掉 TS stderr 开头一次完整 SQLite 两行；不调整 Rust 输出。请求 UUID 仅在 mock 重放数据中替换为 `<request>`，实际 CLI stdout/stderr 不改。

`tests/sdk_sessions_list_exit.rs` 启动真实 Rust CLI 和独占 `127.0.0.1:28999` 的 WebSocket mock；与 D0 共用 `/tmp/pdrs-parity.lock`，按 `.guard` flock、目录和 `owner.json`（pid/timestamp/token）协调，不回收他人锁、不换端口。nextest 的独立测试进程通过目录租约互斥；同进程仍保留 Tokio mutex。目录或 guard 忙时最多等待 180 秒，超时显式失败；调度迟到也不得超期取锁。取消等待不修改前驱租约，退出只释放 inode 与 owner 均属于自己的目录；释放时 guard 短暂竞争最多重试 2 秒，失败保留租约并报错。信号由收到请求事件触发；子进程均带终止上限、失败清理及 wait，服务端消费到 EOF。测试不连接 18789；本测试不替代 D0 的内核隔离验收。

13 个 `parity_lock_*` 用例用私有目录和确定性轮询/释放钩子覆盖发布、释放、等待、永久占用、取消、迟到唤醒、guard 竞争和 owner/目录替换。这些是测试资源协调协议，不是新增 TS 产品行为金样。

矩阵比较 stdout、stderr、exit/signal、完整落盘及 hello/RPC。补充的 15 个 transport 边界格属于读码推断，不作为新增 TS 金样；鉴权 close 的 Rust 应答 reason 为 `auth_failed`，TS mock 原件为 `""`，CLI 四面相同。此内部 WS 帧差异尚未批准为对拍豁免。

重生成守卫（先按项目要求设置 Cargo 环境及检查 slots）：

```sh
cargo test -p pilotdeck-cli --test sdk_sessions_list_exit \
  sessions_list_oracle_regenerates_identically -- --exact --ignored
```

`NODE22` 优先指定已存在的 Node v22.22.0；未指定时使用 PATH 上的 `node`。守卫先执行所选程序的 `--version`，要求成功退出且报告 `v22.22.0`；不符即带 `#983` 消息失败，不启动生成器、不回落到其他版本。输出目录递归创建、文件以 `wx` 独占写入，原件缺失/内容变化均失败。

50 个负控（原 28 个 CLI/金样变异 + 17 个租约变异 + 5 个 transport 上限/金样变异）定义在 `src/pilotdeck-gateway/tests/fixtures/sdk-sessions-list/negative_controls.py`，复用该目录现有 `negative_process` 与 `negative_suite`，以便 main 的锚点检查器发现。每项先编译，使用当前 nonce + 精确测试名 + 格 ID 判杀；恢复后重跑正控，全项汇总。必须提供 `CARGO_TARGET_DIR`，全部受保护源码/执行器等于 HEAD 才可运行；有未提交修复或新文件时拒跑。提交后协调者执行：

```sh
python3 src/pilotdeck-gateway/tests/fixtures/sdk-sessions-list/negative_controls.py \
  _tmp/20261026-issue-983/negative-host
```

同一脚本加 `--node-runtime` 可独立验证解释器选择：PATH 回落、NODE22 优先、错误版本拒绝和版本命令失败拒绝。每格用隔离子进程环境覆盖 Node 启动条件，正常生成仍调用真实 v22.22.0；错误条件必须在生成器启动前触发精确断言。该模式不改源码，可在未提交修复树运行。

fix-r3 的大帧回归与原生 Node/`ws` 对照原件见 `../sdk-transport-limits/`；它不混入上述 77 格旧实录。hello 直接使用协议常量，缩进状态机由 sessions 与 skills migrate 共用；`hello-version` / `pretty-indent` 的锚点已同步。
