# Sidecar 非对象 payload：按入口实测

Node v22.22.0；PDRS_BASELINE=cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85。运行 `node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" generate.mts <新目录>`。完整原文件执行，私有读函数仅追加内存导出；基线源码逐字节验证。输出独占创建，不覆盖金样。

72格：11种坏值及合法对象 × 6入口。wire 两路均拒绝；embedded-reader 也拒绝；terminal-reader 普通分支拒绝、特定取消分支合成 aborted（私有诊断，真实 wire 仍先校验）；host-bridge 吞模块失败且后继继续。不能把 bridge 的结果泛化为 wire。undefined 用 `__pdrsUndefined` 标记，missing 没有 payload 属性。

Gateway lib 的 `sidecar_payload_*` 回放 wire 两路（event/wire 测试，`final` 区分）、嵌入 reader、event 模块及重生成守卫；terminal-reader / terminal-cancelled 两路由 `host/tests/event.rs` 的 terminal 回放测试走 `validate_event` 的 final 分支（TS 私有 `readFinalTerminal` 的对应）。非对象 payload 在 Rust wire decoder 前置拒（wire-final 路已回放该行为），terminal 回放用 wire 可表示的空对象代替非对象坏值——TS reader 对二者走同一「无 AgentLoop result」分支（Node v22.22.0 实测，证据见负控目录同级 probe-standin.log）。Transport lib 的同前缀测试以真实 writer/响应回执回放桥；只注入 null 代表已知模块失败，其余 payload 形状由 gateway 模块回放覆盖。final 委派/取消沿既有测试，未新增生命周期等待。

负控：设置本树 CARGO_TARGET_DIR、PDRS_BASELINE 后，`python3 negative.py <新证据目录> <变异名>`；变异名见脚本。六条路由各有 omit-* 变异（把该路由的 missing 用例 route 改名，由对应回放测试的 replayed 计数击杀）。要求源码已提交且 pristine；默认全跑适合协调者宿主，worker 只跑一格。不把编译错误、零测试或超时算检出。
