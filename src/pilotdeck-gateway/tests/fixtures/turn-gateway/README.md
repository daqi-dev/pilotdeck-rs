# A7.2-pr5 ConnectionGateway 金样

固定真相源 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。
`generate.mts` 通过共享 `assertBaseline` / `baselineSourceHashes` 校验基线 commit、工作树与逐文件源码哈希；Rust 每次回放通过 `test_baseline::oracle` 再核 provenance。

生成器编译并执行真实 `GatewayWsConnection`、`InProcessGateway.submitTurn/emitForSession/abortTurn/disconnectInteraction`、`SessionRouter`、`AgentHandle`、`TurnRunner`、projector、coordinator、replay、`FileRunRegistry` 和 interaction coordinator/buses。受控端口是 transcript writer、Agent 原生事件生成器与提交策略/配置服务。没有手写 Gateway 事件期望或 WS 帧期望。

- `cases`：17 个 WS 场景，逐步骤记录 **JSON.stringify 原始字符串**、宿主 admission、消费结束状态、真实 registry 事件（仅剔除时间戳）、终态 replay。覆盖 permission notification 先于普通帧、SessionEnd 先于 deferred completion、唯一 final 包装、error 优先、并发 busy、close 与 pending permission、busy 后 close 的 Set 语义、重复 run 从 WS seq=0 重放、冲突、finally 错误、registry append 失败、显式/生成 runId 的 abort/close、并行 describe。
- `timerCases`：0/5/30000ms × 5 种时序。控制时钟只标记到期，调度栈和 ready continuation 内不执行 timer 回调，后续宿主 timer 回合才执行。覆盖步骤中到期、步骤后到期、dispose 前/后到期、已到期旧回调与新 replay generation 交错。Rust 对拍经过实际 `TurnGateway` 线程及其 `HostSerialTimer`，不靠墙钟或轮询 sleep。
- `moduleSequences`：真实 TS `ModuleOperationHost.acceptSequence` 的独立序列探针。permission 事件额外携带 `sequence:41` 作为跨层哨兵；WS seq 从 0 开始、registry seq 从 1 开始，哨兵必须原样保留。`negative.py` 覆盖三种序列两两混用的六个方向。A6 Module 传输的完整接线与流校验属于 pr6；本单不重写 Module sequence owner。

复现（Node v22，基线目录只读）：

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/turn-gateway/generate.mts /tmp/pdrs-turn-gateway-oracle
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib runtime::turn::gateway
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib \
  real_ts_turn_gateway_regenerates_identically -- --ignored
python3 src/pilotdeck-gateway/tests/fixtures/turn-gateway/negative.py \
  _tmp/20261023-a72-pr5-gateway/negative
```

负控只临时改产品源码，原样回放金样与测试，逐例还原并校验源码字节；编译失败、超时不计杀伤。必须独占工作树，不与格式化、其他 cargo 或人工编辑并行。产物目录保存每条完整输出和 `summary.json`；可在最后追加控制名只跑指定项。

生产构造入口为 `TurnGateway::start` → `connect` → `HostedConnection::{receive,closed,notify}`。宿主 builder 使用注入的 coordinator 构造 `TurnSubmitter`，已有 FileRegistry / SessionOwner / projector 等依赖继续复用；外部事件走 `TurnGateway::emit`。原始 `ConnectionTurnGateway` 不对外暴露，避免连接 future 被别的执行器 poll。`GatewayRequests` 留给既有宿主方法/读面实现，不提供 TS fallback；pr6 补真实 LiveSession/HostClient 与宿主策略服务。
