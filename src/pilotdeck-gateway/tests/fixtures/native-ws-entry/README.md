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

资源数值、范围及未批准状态统一见 `specs/gateway-parity/diffs/A7.2-NATIVE-WS-RESOURCE-001-R2.md`（原条目保留）。fix-r2 的真实 TS 大文本探针覆盖2MiB与100MiB，记录完整文本帧长度和SHA256；`fullQueueEnd` 使用真实TS sendText与Node Writable的确定性回调前驱验证排队后正常end，已有真实TCP探针仍覆盖永不释放/取消两格。

Rust输出预算覆盖上述合法单帧，并独占预留Close/End槽；字节或数据槽耗尽后排入1009，非读对端仍有关闭期限。`read_capacity` 的饱和分支正控是明确的防御状态注入，合法满帧被消费的反例由 `exact_input_frame_is_consumed_before_next_read` 单列。头预算以 `frame.rs` 常量为正本，与真实编码器头长比对并配两项变异。旧字节permit泄漏负控改绑直接检查写后释放，避免预算增大削弱检出。

负控完整分母和绑定以 `negative_controls.py` 为正本；逐项编译，仅一个runtime样本在会话执行，其余由协调者宿主全量。源码不原地变异，故继续使用tree_lock/record_sources，未引入自写快照恢复器。

fix-r3 的 closeFlush 用真实GatewayServer/net.Socket写回调前驱验证close/EOF的释放与取消四格：模拟全局计时器一小时后仍挂起；close释放会完整冲洗并finish，EOF释放后完整交付但保留半开写端。Rust的有界期限及EOF主动shutdown差异以R2为准；期限按仍占许可的已接受字节计算，首次期限固定。暂停时钟测试明确推进2秒/到期；跨host线程等待由manual_clock阻止自动跃迁，60秒墙钟仅作装置故障保护，不作为变异KILLED证据。

largeOutput强制恰两项2MiB/100MiB并逐项比较内容SHA256；fullQueueEnd严格核对before.completed=1、before.finished=false、completed=MAX_OUTPUT_FRAMES、finished/destroyed=true。Node Writable的destroyed来自finish后的autoDestroy，Rust使用writer future完成和许可归还对应生命周期资源释放。空/缺项/错值/错内容均有独立负控。
