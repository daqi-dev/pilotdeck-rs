---
id: A4-LISTEN-PORT-LOG-001
---

## 类别

已知未收敛差异（产品缺陷）：A4 默认路径的就绪日志上报请求的监听地址而非内核实际分配端口。未批准，非已接受差异。产品修复独立开单 #1131；本条仅登记，防止 #1071 fixture 的预留端口加重试被读作该差异已闭合。

## 基线依据

固定 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，Node v22.22.0。`src/gateway/server/GatewayServer.ts:71-73` 在 `listen(server, options.port ?? 18789, host)` 后读 `server.address().port`（请求端口 0 时为内核分配端口），`url`/`wsUrl` 由该实际端口构造；`src/cli/pilotdeck.ts:252-253` 打印 `server.url` 与 `server.wsUrl`。真实组件实测（tsx 加载基线 `GatewayServer`，`port:0` 起服务，探针 `_tmp/20261026-issue-1071/fix-r3/probes/ts-gateway-port0.mts`）：`url=http://127.0.0.1:53288`、`wsUrl=ws://127.0.0.1:53288/ws`，原始输出 `_tmp/20261026-issue-1071/fix-r3/probes/ts-gateway-port0.log`。

## Rust 行为

`src/pilotdeck-gateway/src/main.rs:38-39` 先 `TcpListener::bind(listen)`，随后打印请求变量 `eprintln!("proxy listening {listen} -> …")`，不读 `listener.local_addr()`；`PILOTDECK_PROXY_LISTEN=127.0.0.1:0` 时就绪行原样上报 `127.0.0.1:0`。真实二进制实测：`proxy listening 127.0.0.1:0 -> http://127.0.0.1:1`，原始输出 `_tmp/20261026-issue-1071/fix-r3/probes/rust-a4-port0.log`。显式 `--native` 路径不受影响（`src/pilotdeck-gateway/src/native_process.rs:57-64` 读 `server.local_addr()` 后打印实际地址）。

## 范围

仅 A4 默认路径的就绪日志及据此构造地址的消费方；不涉及转发字节、鉴权或其余 A4 行为。#1071 的 `tests/fixtures/native-process/process.mts` 观察者依赖就绪地址连接子进程，fixture 以「预留具体端口 + close 后 spawn + bind 冲突有界重试」绕开该缺陷（预留的 TOCTOU 处理见 fix-r2）；产品修复（改读实际端口）在 #1131，修复后可移除该预留与重试。

## 机器判据

`native_process::tests::executable_native_and_a4` 经 fixture 预留端口验证 A4 字节转发，不宣称本差异对齐；`negative_controls.py` 的 `a4-listen-collision` 与 `readiness-address-port` 两条负控分别钉住预留/重试与就绪整行带端口地址，专属断言 `#1071 executable A4 bind collision exhausted` / `#1071 executable readiness address carries a port`（nonce receipt 判杀，其余异常 INVALID）。本差异本身暂无 Rust 侧对拍测试（两侧实测输出即证据）；#1131 修复后可去除 fixture 预留并以两侧 `:0` 实测对拍。

## 批准记录

未批准，非已接受差异。fix-r3 评审（Codex r4 P1）要求独立开产品单修复，不得以 fixture 预留端口加重试闭合该差异；产品单 #1131 已开。本条由 #1071 fix-r3 于 2026-10-31 登记。
