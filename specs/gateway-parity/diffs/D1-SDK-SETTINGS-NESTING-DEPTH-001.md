---
id: D1-SDK-SETTINGS-NESTING-DEPTH-001
---

## 类别

已登记差异（不修而记）：CLI `settings set` 极深嵌套 JSON 下，Rust 有意不复现 TS 的递归栈溢出。#1044 采纳人定建议 (a)：登记差异 + 守卫测试，不在 Rust 侧新增深度上限（即不采纳路线 (b)）。

## TS 依据

固定基线 cc7c4e6 的 `packages/sdk` 在 Node v22.22.0 上经真实 CLI 探针实测（`_tmp/20261026-issue-1044/depth-probe.mjs`，基线 `dist/src/cli/pilotdeck.js` + 本地 mock WebSocket 网关，与 `src/pilotdeck-cli/tests/fixtures/sdk-settings/probe.mjs` 同构，非 D0 沙箱）：

- 1,000 层（`{"nested":[[…0…]]}`，2,012 字节 argv）：exit 0，stdout `{"updated":true}`，hello+request 完整 wire。已在 193 格矩阵内（`settings-extra--set-deep-object` 等格）。
- 10,000 层（20,012 字节）：hello 后构造/序列化 request 帧时 `Maximum call stack size exceeded`，exit 1，不发 request。
- 64,000 层（128,012 字节）：同上溢出。
- 100,000 层（200,002 字节）：Linux `MAX_ARG_STRLEN`=131,072，无法经 argv 传递（实测 `Error: spawn E2BIG`）；#1029 主机表记录的该层在 macOS 可传，同样溢出。

原始输出：`_tmp/20261026-issue-1044/depth-probe.log` 与 `depth-probe-out/depth-probe.json`。溢出阈值依赖运行时栈状态，不声称精确边界，仅登记 10,000 层即溢出的事实行为。

## Rust 行为

`src/pilotdeck-cli/src/sdk/settings.rs` 的 `parse()` 使用 `pilotdeck_protocol::parse_json` 显式栈解析，`RawValue` 原样透传 wire。64,000 层（argv 可表达的最深嵌套，128,012 字节）实测：hello + `update_settings` request 完整 wire、stdout `{"updated":true}`、exit 0、磁盘快照不变。协议层另有 100,000 层小线程栈守卫 `src/pilotdeck-protocol/tests/codec.rs:196`（128 KiB 栈上 parse/stringify/clone/unwind），覆盖 argv 不可表达的更深层级。

## 范围

仅 CLI `settings set` 的 argv JSON 深度语义（Rust `settings set` 无 stdin JSON 路径）。Rust 侧不新增深度上限或字节上限；TS 基线不改。不涉及 gateway 服务端、其余 SDK 子命令（不接收 JSON set 参数）或冻结装置判分。

## 机器判据

`src/pilotdeck-cli/tests/sdk_settings_exit.rs` 的 `settings_set_accepts_extreme_nesting`：64,000 层经真实二进制全流程，断言 hello/request 全 wire 字节、输出、退出码、盘不变、无多余连接。

负控 `src/pilotdeck-gateway/tests/fixtures/sdk-settings/negative_controls.py` 新增两行：

- `settings-depth-gate`：在 `parse()` 注入 `raw.len() > 65_536` 拒绝（模拟本单不采纳的路线 (b)），绑定 lifecycle 断言 `#1044 expected gateway lifecycle settings-extra--set-extreme-nesting`。
- `settings-depth-wire`：守卫测试期望 wire 的 `{deep}` 载荷改为字面 `{}`，绑定 `#1029 hello and request wire settings-extra--set-extreme-nesting`。

两项负控在本沙箱未提交树上被 `mutating_sources` HEAD 守卫拒绝实跑；脚本已就位，提交后由协调者在宿主执行。

## 批准记录

未批准，非已接受差异，待人。#1044 由协调者指明采纳建议 (a)（登记 + 守卫）而非 (b)（Rust 侧加等价拒绝）；本登记不改行为、不改计分口径。
