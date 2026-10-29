# #749 原生 WS 监听层

`NativeWsServer::bind(address, host, token, version)` 是显式库入口：调用者提供已经装配的 `TurnGateway`、127.0.0.1 高位端口（测试用 0）、token；不读取默认 home，也不访问 TS gateway upstream。`run(stop, budget)` 停止接纳后排空连接，最后关闭 serial host。取消 `run`（包括尚未 poll 的 future）也会请求宿主关闭。`TurnGateway::start_with_cleanup` 让 factory/project 清理发生在宿主 executor 退出之前；初始化失败时部分资源仍由调用者负责。

本目录不是可启动的生产 bin 装配。默认 `main.rs` 保持原字节；生产 `GatewayRequests`、`SubmitHostServices` 与资源族装配仍待补齐，因此本补丁本身不能解除 #590 的原生进程前置。

## TS 金样

`generate.mts` 用 Node v22.22.0 加载固定基线的真实 GatewayServer、GatewayWsConnection、TextWebSocketConnection。真实 TCP/WS 检验握手、token、并发、同连接 abort、断开、停止接纳；deferred Promise 明确控制前驱完成。关闭帧的三格另用 EventEmitter socket 明确给出每次 data 边界，执行真实两层实现：同批 close/tail 为 0，下一批 data 激活尾部为 1，应用 invalid JSON close 同批尾部为 1。测试替身仅控制未在此验证的业务和 I/O 输入时机。

`native_ws::tests::regenerates_identically`（ignore）重生成并比较 oracle。#748 装置的 `live_ws_text` / `live_ws_tool`（ignore）通过真实 WS 跑完整 native submission→HostClient→固定 TS sidecar，仍使用 CARGO_TARGET_TMPDIR 内独占用例目录。

## 负控

`negative_controls.py --check` 核单一开关锚点和测试消息；运行参数为 `<独占输出目录> [开关名...]`。必须由调用者设置 `CARGO_TARGET_DIR`，并且受检源码/fixture 与 HEAD 完全一致。脚本复用 `mutating_sources`、`run_owned`、`wait_for_slot` 和 `negative_suite`，不做源码文本替换；每项先 `cargo test --no-run --features pdrs-mutants`，再正控、变异、恢复正控，全部完成后统一汇总。任何 INVALID/SURVIVED 非零退出。

新文件在提交前无法通过 HEAD 守卫；正式负控由协调者提交后在宿主运行，不另建仓库或构建目录。`--compile-only` 也遵守同一守卫。全量可超过会话预算，交付会话只尝试一个样本。具体原始记录及未完成项见交付目录 RESULT.md。
