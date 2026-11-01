# U7 settings 写与重载五方法金样（#1318）

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，Node v22.22.0 / ICU 78.2
（生成器断言 `process.version === 'v22.22.0'` 与 `Intl.Collator` 解析为 `en-US`，
须 `LANG=en_US.UTF-8`）。覆盖 `apply_flag_settings` / `update_settings` /
`reload_output_styles` / `reload_config` / `reload_extensions` 的参数校验序、
能力门、WS 错误帧投影（`{code,message}`，无 code 的原始抛错 →
`gateway_request_failed`）、事件流与会话 / registry / store 后置状态。

生成器加载真实三层栈：真实 `SessionRouter`（真实 beginTurn/close 与
`asAgentHandle` 会话，key `f` 的 dispose 以真实 rejection 走 close-fault 格）、
真实 `InProcessGateway`（仅 interactionCoordinator 在原型链上包一层记录
close-session 原因串，#1215 模式）、真实 `GatewayWsConnection`（假文本传输）、
真实 `ProjectRuntimeRegistry.prototype` 的 `applyFlagSettingsForSdk` /
`reloadOutputStylesForSdk`（真 Map 状态；resolve / pluginRuntime 为桩）、真实
`GatewayRuntimeRefreshBundle`（录音协作者；configStore delegate 始终指向当前场景
树的真实 `createPilotConfigStoreSync` store）、真实 `updatePilotLocalSettings`
写盘（每场景树一套 scratch home/project，行间按需复用，如 `t1` 被
update-merges / rewrite-same-value / clear-value / noop 四行共用）。`updateSettings`
闭包体为 `createLocalGateway.ts:1384-1404` 逐字副本（createLocalGateway 本身
要起整个 CLI，离线不可运行）；`createLocalGateway.ts` 计入 sourceSha256，
闭包任何编辑都会改变 oracle 的 provenance。

`oracle.json` 全由生成器输出，不手写期望：54 个 case 行（每行 `{id, method,
params, setup, events, response, post}`）+ `assemblyCases.capability`（裸组装
五方法的能力门答案）+ `setupKeys`。update 系行写真实 YAML（`agent` flow map
按 yaml 包规范空格形重排），事件含 `store-event:` / `log:` / `sdk-config-change:`
/ `broadcast:config_changed:` / `mark-*-dirty:` / `registry-reload`。无时钟 /
PID / 路径进 oracle（scratch 路径不录制；changedPaths 与事件均为内容函数）。
金样在本机 macOS 与 cb8（Linux）各以同基线重生成，逐字节一致。

复现（从仓库根，输出目录必须为新目录）：

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --disable-warning=ExperimentalWarning \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/settings-gateway/generate.mts /tmp/settings-golden-1
node --disable-warning=ExperimentalWarning \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/settings-gateway/generate.mts /tmp/settings-golden-2
cmp /tmp/settings-golden-1/oracle.json /tmp/settings-golden-2/oracle.json
cmp /tmp/settings-golden-1/oracle.json \
  src/pilotdeck-gateway/tests/fixtures/settings-gateway/oracle.json
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib runtime::settings
```

Rust 重放（`runtime/settings/gateway_tests.rs`）逐行执行同一 setup 词表，经
`SettingsGateway::dispatch` 重建 WS 帧，比对 response / events / post 三格
（JSON 结构等价：对象键序经 serde 归一，不钉键序字节）；
`#[ignore]` 的 `settings_gateway_fixture_regenerates_identically_from_ts` 为
重生成守卫（`PDRS_BASELINE` 下实跑）。

负控（独占本 worktree，不与任何编译或写入并发；六个变异的目标文件在本分支
新增，须提交后由 `mutating_sources` 在 HEAD 字节上执行）：

```sh
CARGO_TARGET_DIR=… python3 \
  src/pilotdeck-gateway/tests/fixtures/settings-gateway/negative_controls.py \
  _tmp/20261026-issue-1318/negative-controls <可选变异名子集>
```

每次仅临时改一处产品实现，要求被点名测试以自己的 `#1318` panic 消息失败
（每变异绑定专属判杀标记：`case apply-busy`、
`case update-merges-store-paths`、`the bare assembly must answer reload_config`、
`dispatch must own the five settings methods`、`case apply-effort-low`、
`#1318-r1 update-drop: dropping the future must detach the capture listener`；
前五个由 `settings_gateway_replays_the_real_ts_oracle` 判杀，最后一个由
`update_settings_detaches_the_capture_listener_when_dropped_mid_reload` 判杀；
`case <id>` 标记与金样行的绑定由脚本启动断言机检）；
编译错误、信号退出、锚点过期（出现次数 ≠ 1）都不算杀死，直接报错退出。
每轮 finally 恢复原始源码并核 SHA-256；输出每条补丁、原始日志、失败摘要，
结束后应重跑正常测试和 fmt/clippy。

本组件未接线 `dispatch.rs` / `native_process`（归 #1320）；registry 的
`applyFlagSettingsForSdk` / `reloadOutputStylesForSdk` 与
`updatePilotLocalSettings` 生产写盘件为镜像端口（`MirrorWriter` 保持同一
allowlist / YAML 编辑语义），随接线单评估替换。已知未收敛差异：
`reload_extensions` 的非字符串 `projectKey` / `changedPaths` 元素被
`String(value)` 强转而 TS 原值透传（金样 54 行均为字符串，不触发；两格
`changedPaths:[1]` / `projectKey:5` 随 #1320 接线补入），登记于
`specs/gateway-parity/diffs/U7-EXTENSIONS-RELOAD-STRING-COERCION-001.md`，
前置于 #1320。
