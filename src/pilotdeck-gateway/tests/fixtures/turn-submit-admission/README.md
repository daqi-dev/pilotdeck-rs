# turn-submit-admission 真实 TS 金样

`generate.mts` 在固定 PilotDeck 基线上执行真实 `InProcessGateway.submitTurn`，聚焦 A7.2-pr6a-7 的接纳与预检窗口：真实 `GatewayTurnReplacementCoordinator`（受控 storage port）、真实 `GatewayInteractionCoordinator`（真实 native reconnect port）与真实 `GatewayManualCompactionCoordinator`（受控 router.compact）；`normalizeSessionModelSelection` 为真实模块（受控 loadPilotConfig）。不复制产品方法体。session、run registry、authorizer、replacement storage 与 compaction router 是显式可控 port；session 事件脚本化，成功轮次经真实接纳窗口后由受控边界发 input_accepted / text_delta / turn_completed / SessionEnd。

每个 case 保存全部可见业务事件、逐事件 wire（JSON 字符串）、宿主调用（refresh/prepare/submit/after）、session.submit 的 agentInput 与 options 原值、registry/storage/interaction/compact/authorizer 调用、compact abort 原因、active 与 ended/thrown；`reachesRegistry` 的 case 最终重读 FileRunRegistry 的事件与状态。受控 setTimeout 用于 `compact-timeout-abort`：真实 `GatewayManualCompactionCoordinator` 的超时定时器被挂起，由步骤显式触发，abort reason 原值入金样。TS 未注入依赖即使被产品 catch 吞掉，生成也必须失败（Proxy 抛错）。

覆盖（与 `_tmp` 计划矩阵逐格对应）：stale binding 重连（connectionId/generation/非法 binding/全新 binding/同 connectionId 非数字 generation → thrown）、replacement claim 冲突与命中（含 transcript 写预留）、`/compact` 用法/成功/忙/失败/超时 abort、`/plan` 用法与重写、trustedContext 无授权器/非法决定/抛错/批准（逐上下文调用）、modelSelection 与 modelOverride 折叠（pump 派生 override 亦入原值）、sdkSessionConfig 校验窗口（接线端口的 set 调用原值、接线/未接线 × 非法 config 的 preflight 窗口拒绝）。全新 binding 轮次同时捕获 `turn_ended` 对 elicitation/permission 总线的拒绝。

生成命令（Node v22；`PDRS_BASELINE` 指向固定 `cc7c4e6` 的干净 checkout）：

```sh
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/turn-submit-admission/generate.mts /tmp/turn-submit-admission-generated
```

生成器调用共享 `assertBaseline` / `baselineSourceHashes`，检查 commit、tracked 状态与逐文件已提交源码哈希。两次输出必须逐字节一致。`regenerates_identically("turn-submit-admission")` 是默认忽略的真实重生成守卫。

```sh
PDRS_BASELINE=/absolute/PilotDeck \
  cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib \
  runtime::turn::admission::tests::real_ts_submit_admission_regenerates_identically -- --ignored --exact
```

`negative_controls.py <证据目录>` 独占临时修改 admission 产品实现（stale 判定、claim 冲突/预留判定、用法文案、端口缺省行为、plan 重写、normalize 折叠、sdkConfig 校验窗口、compact 定时器 drop 取消、connectionId 失配门），逐条编译并运行对应测试，要求失败测试的 panic 块来自 `runtime::turn::admission` 下的用例，再逐字节恢复。不要与另一构建或实现写入同时运行。
