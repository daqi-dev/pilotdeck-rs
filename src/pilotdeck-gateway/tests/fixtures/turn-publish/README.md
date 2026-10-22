# turn-publish

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的真实 TS 发布金样。生成器执行未修改的 `InProcessGateway.submitTurn`、`FileRunRegistry`、projector、coordinator/replay、`AsyncQueue` 和 `GatewayWsConnection`。源码逐份校验提交内容与工作文件哈希，运行前后均校验基线；Rust 经 `test_baseline::oracle` 检查溯源。

宿主注入仅提供 session 事件源、接纳/配置/权限等空服务；不实例化真实 Gateway 宿主、不读取用户配置。registry 调用层以显式 gate 注入延迟/拒绝，storage 故障在真实 `FileRunRegistry.persist` 边界注入。请求指纹由真实 submitTurn 计算。生成器不抄写发布流程、期望帧、registry 状态转换或 WS 包装逻辑；所有期望来自真实实现。

14 场景逐步记录：live 事件、真实 WS 帧（包括 `seq` / `final`）、durable 调用与成功落盘事件、registry 状态/revision/lastSeq/events、replay。场景覆盖 accept/重复回放/冲突、accept 的 registry/storage 失败、preflight 成功/失败、连续 append 失败只报一次 unknown、失败后继续成功、session end 后发布终态、pending append 中又来事件、倒序释放写入 gate、关闭等待、迟到事件与关闭交错、重复业务终态、无 registry。

成功终态的普通事件和最后的 WS 包装同时存在，后者复用前者；逐步比较不会把它们计为两次业务裁决。输入事件的 `sequence: 71/72/73` 标签不作为 WS 或 registry 游标，外部事件保留的 `moduleSeq: 99` 也不影响两者。WS 从 0 起，registry 从 1 起。发布器不接收或分配 Module envelope 游标；本夹具不运行 Module 传输，真实 Module/host 接入归 pr6。

TS 可见性细节：replay 在 append 前更新，live 等 append 成功；重复业务终态的第二次 append 被 registry 拒绝，live 输出一次 result_unknown；session end 的 Promise.all 快照之后到达的事件仍可能进入 registry，但队列关闭后 enqueue 会被丢弃。末帧由真实 WS 层等 submitTurn 完成后生成，error 优先于 completion。末帧不是额外的 durable 事件。

## 重生成

从仓库根执行，使用基线已有 Node 22 / tsx，所有输出写工作区或 /tmp：

```sh
export PDRS_BASELINE=/path/to/PilotDeck
export TSX_DISABLE_CACHE=1
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/turn-publish/generate.mts /tmp/turn-publish-first
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/turn-publish/generate.mts /tmp/turn-publish-second
cmp /tmp/turn-publish-first/oracle.json /tmp/turn-publish-second/oracle.json
shasum -a 256 /tmp/turn-publish-first/oracle.json /tmp/turn-publish-second/oracle.json
```

当前 SHA-256：`3ce7d752a63fec4999ce505cd79fc121b400bf39784206e40f05f6011a605bec`。

## 对拍与负控

```sh
# 先调用项目 cargo-slots.sh；使用本树独立 CARGO_TARGET_DIR。
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib runtime::turn::publish
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib \
  real_ts_turn_publish_regenerates_identically -- --ignored
python3 src/pilotdeck-gateway/tests/fixtures/turn-publish/negative.py /tmp/turn-publish-negative
```

`negative.py` 必须独占本树运行。它临时变异产品源文件（包括一次 WS `final` 标志变异），测试与金样不变；先跑原版，再对每个变异要求编译成功且目标断言失败。原始 SHA、变异前后文本、完整输出、退出码写输出目录；finally 恢复源文件原字节并核验。不要与编辑器、cargo 或另一负控并发写本树。负控覆盖丢/重复/提前 final、提前可见/关闭、顺序、重复回放、冲突、accept/preflight/storage 失败、单次 unknown、迟到事件。

另有真实 Rust `FileRegistry` 适配器的双 worker 测试：65 帧持久化后再读 live 流，核 FIFO、durable 结果一致和关流后拒收。

生产 submit/ConnectionGateway 接线仍归 pr4/pr5；本夹具用测试适配器串起真实 Rust connection 与发布组件。终态回收计时差异 A7.2-REPLAY-RETENTION-TIMER-001 不在本单修改。

并发入口约定：活动发布期间，宿主通过 `TurnPublisher.emit` 发外部事件，通过 `agent_event` / `gateway_event` 发 pump 事件；不要绕过发布器直接调用底层 coordinator.emit。这样一批投影、replay 记录、append 入队、finish 与消费端清理各自保持 TS 同步段的原子性，等 I/O 时释放串行锁。额外双线程测试同时驱动 pump/外部入口，比较 replay、durable 和 live 的同一顺序。
