# #749 原生 WS 监听层

`NativeWsServer::bind(address, host, token, version)` 是显式库入口：调用者提供已经装配的 `TurnGateway`、127.0.0.1 高位端口（测试用 0）、token；不读取默认 home，也不访问 TS gateway upstream。`run(stop, budget)` 停止接纳后排空连接，最后关闭 serial host。取消 `run`（包括尚未 poll 的 future）也会请求宿主关闭。`TurnGateway::start_with_cleanup` 让 factory/project 清理发生在宿主 executor 退出之前；初始化失败时部分资源仍由调用者负责。

本目录不是可启动的生产 bin 装配。默认 `main.rs` 保持原字节；生产 `GatewayRequests`、`SubmitHostServices` 与资源族装配仍待补齐，因此本补丁本身不能解除 #590 的原生进程前置。

## TS 金样

`generate.mts` 用 Node v22.22.0 加载固定基线的真实 GatewayServer、GatewayWsConnection、TextWebSocketConnection。真实 TCP/WS 检验握手、token、并发、同连接 abort、断开、停止接纳；deferred Promise 明确控制前驱完成。关闭帧的三格另用 EventEmitter socket 明确给出每次 data 边界，执行真实两层实现：同批 close/tail 为 0，下一批 data 激活尾部为 1，应用 invalid JSON close 同批尾部为 1。测试替身仅控制未在此验证的业务和 I/O 输入时机。

`native_ws::tests::regenerates_identically`（ignore）重生成并比较 oracle。#748 装置的 `live_ws_text` / `live_ws_tool`（ignore）通过真实 WS 跑完整 native submission→HostClient→固定 TS sidecar，仍使用 CARGO_TARGET_TMPDIR 内独占用例目录。

## 负控

`negative_controls.py --check` 核单一开关锚点和具名测试消息；运行参数 `<独占输出目录> [开关名...]`，`--compile-only` 逐项编译。必须设置调用方的 `CARGO_TARGET_DIR`。源码不做文本替换，使用共享 tree_lock、工作树 SHA 记录与结束核对（同 `_shared/mutant_switch.py` 合同），不是 HEAD 等价声明。每项先编译，再正控、变异、恢复正控；异常也跑恢复，逐项原子发布汇总、最终核静态分母。nonce 回执只接受绑定测试/消息；编译失败或无关 panic 不算杀。执行形态登记在 `specs/negative-controls/README.md`，不改共享执行器。会话仅跑一个样本，全部 runtime 变异由协调者在宿主运行。

## 资源保护差异

`resources.mts` 的真实 TS 探针证明该入口用自写 WS，不是 ws WebSocketServer；100MiB+1/1TiB 长度头均等待，无 1009；write(false) 后继续写；实际 GatewayServer 的 cork 写前驱遇客户端 FIN 仍阻塞 close，destroy 后结束。65 个 held requests 同时准入。原始记录见 fix-r1 RESULT。

本单按用户安全要求限制原生入口：payload 100MiB（缓冲另容14字节头）、输出64个等待槽（另最多1帧在写）/1MiB（包括在写帧的保守10字节头预算）、在途请求64项。输出/请求超限强制关闭，不保证发送关闭帧；入站超限1009。首次 close/EOF 后1秒同时限制读写，保护业务前驱挂起时的 socket 回收。差异登记 `A7.2-NATIVE-WS-RESOURCE-001.md`；既有 `regenerates_identically` 同时覆盖新增 observations，无新增 clock/PID mask。
