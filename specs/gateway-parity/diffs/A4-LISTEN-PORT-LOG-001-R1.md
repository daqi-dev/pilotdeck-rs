---
id: A4-LISTEN-PORT-LOG-001-R1
original: A4-LISTEN-PORT-LOG-001
---

## 类别

更正（bug 已修复）：原条目登记的「A4 默认路径就绪日志上报请求的监听地址而非内核实际分配端口」产品缺陷已由 #1131 修复闭合，原「已知未收敛差异」结论不再成立。本更正登记修复后的对齐现状与机器判据，不改变原条目字节。

## 基线依据

固定 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，Node v22.22.0。`src/gateway/server/GatewayServer.ts:71-76` 在 `listen(server, options.port ?? 18789, host)` 后读 `server.address().port`（请求端口 0 时为内核分配端口），`url`/`wsUrl` 由该实际端口构造；`src/cli/pilotdeck.ts:252-253` 打印 `PilotDeck server listening: ${server.url}` 与 `WebSocket: ${server.wsUrl}`。#1131 金样 `src/pilotdeck-gateway/tests/fixtures/proxy-listen-address/oracle.json` 的 `addressLines` 由 `generate.mts` 对固定基线真实 CLI 以 `--port 0` 两次实测采集（stdout 两行、`/health` 200），动态端口归一为 `<PORT>`；`regenerates_identically` 守卫逐字节重生成核验。原始探针另见原条目所引 `_tmp/20261026-issue-1071/fix-r3/probes/ts-gateway-port0.log`。

## Rust 行为

`src/pilotdeck-gateway/src/main.rs:39-41` 在 `TcpListener::bind(listen)` 后读 `listener.local_addr()?`，随后向 stdout 打印 `PilotDeck server listening: http://{address}` 与 `WebSocket: ws://{address}/ws` 两行（LF 结尾）。`PILOTDECK_PROXY_LISTEN=127.0.0.1:0` 时上报内核实际分配端口；显式端口保持不变。真实二进制实测（fix-r2，`_tmp/20261026-issue-1131/fix-r2/probes/rust-a4-port0-r1.log`）：请求 `127.0.0.1:0`，stdout 报 `http://127.0.0.1:<kernel-port>` 与 `ws://127.0.0.1:<kernel-port>/ws`。`--native` 路径不受影响（`native_process.rs` 读 `server.local_addr()` 后向 stderr 打印实际地址）。

## 范围

仅 A4 默认路径的就绪日志与据此构造地址的消费方；不涉及转发字节、鉴权或其余 A4 行为。#1071 fixture `tests/fixtures/native-process/process.mts` 原以「预留具体端口 + close 后 spawn + bind 冲突有界重试」绕开该缺陷，#1131 后改为与 native 路径一致：以 `:0` 启动、从 stdout 新两行读回实际地址并断言端口非零，预留与重试已删除（原条目「范围」节所述绕开装置不复存在）。差异账本本条目更正不改变 #1071 其余结论。

## 机器判据

`tests/proxy_listen_address.rs::reports_reachable_actual_address` 起真实二进制，经 `common/baseline.rs::oracle` 溯源后要求两行 stdout 与 TS 金样逐字节一致（含 LF）、端口非零、可 TCP 连接、固定端口保持不变。配对负控 `proxy-listen-address/negative_controls.py` 七项：requested-address / http-format / ws-format / ws-crlf / fixed-address / unreachable-address / oracle-commit（源码、oracle 与溯源 gate 均入 HEAD 守卫）。`tests/native_process.rs::executable_native_and_a4` 的 A4 段改为读实际端口，配对负控 `a4-requested-address`（产品回退请求地址）、`a4-readiness-address-port` 与 `readiness-address-port`（fixture 两路就绪正则丢端口）；#1071 的 `a4-listen-collision` 随预留/重试删除一并移除。运行证据 `_tmp/20261026-issue-1131/fix-r2/RESULT.md`。

## 批准记录

未批准，非已接受差异；差异本身已闭合，无需接受。本更正不增加判分豁免、不改变阶段完成状态。产品修复属 #1131，fixture 侧同步与判据更新见 #1131 fix-r2；如复核发现两侧行为仍有出入，另开新更正条目。
