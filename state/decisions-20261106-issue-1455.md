# 决策卡 · #1455 config 方法族 7 条 P2 的处置方向

- 日期：2026-11-06
- 状态：**AI 代批，待人复核**（自决 · 待复核：不撤回代价低，但「不改产品码」是判断题）
- 涉及：`src/pilotdeck-gateway/src/native_process/methods/{config,session_dir}.rs`、`specs/gateway-parity/diffs/A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R2.md`
- 前置：issue #1455（源自 #1417 / #1310 闸门刷新的 7 条 P2）

## 卡在哪

7 条 P2 中 5 条（G1/G2/G4/R3/R4）问的是「Rust 该不该补一个校验门」，答案取决于 TS 基线行为，不能靠读码推断。若补错方向，会把「与 TS 对齐」改成「与 TS 分叉」，且差异账本会记反。

## 实测依据（Node v22.22.0 / ICU 78.2 / PilotDeck `cc7c4e65`）

| 问题 | TS 实测 | 处置 |
|---|---|---|
| set_permission_mode 是否校验 sessionKey | `sessionKey` 缺失 → `{"applied":true}`、`busyKeys:[null]`；非串 42 同样放行 | **不补门**，测试钉现状 |
| set_output_style 非串 name | `TypeError: input.name.trim is not a function`（裸异常，无错误码） | **不补 INVALID_OUTPUT_STYLE**，登记差异 |
| session_model_set/clear 是否需要 hasActiveTurn | `busyCalls:0` 且返回 applied；busy 语义是 `reserveTranscriptWrite` 预约 | **不加门**，账本更正 |
| lone surrogate sessionKey | `?.trim()` 放行（`{"applied":"callback"}`），Rust `to_utf8` 版会拒 | **改 Rust**：收敛 lossy 共享守卫 |
| seed_read_state / set_permission_mode 的 SESSION_BUSY | 两条报文原文见 probe3/probe4 | 门与文案与 TS 一致，**只补测试/负控** |

## 选项与后果

- A（采纳）：按 TS 实测，能对齐的改 Rust（仅 G7），TS 本身没有的门不补，差异一律写进 `-R2.md`。
  - 后果：Rust 与 TS 的 gate 顺序逐条可对拍；差异有账本可查；不动判分语义。
- B：补齐 5 个「看起来更严」的门。
  - 后果：Rust 行为偏离 TS，对拍会出现假 FAIL；等于用实现偏好改基线语义。
- C：先不改、只登记。
  - 后果：G7 的 lone-surrogate 分叉留着，两条同码守卫继续各自演化。

## 建议

采纳 A。可撤回（纯本地代码 + 账本追加，无数据/接口不可逆影响），在意向范围内（对齐 TS 基线是既有方向），判据已预注册（探针原文在 `_tmp/20261106-issue-1455/probes/`）。

## 可复用原则

**「Rust 少一道校验」不等于缺陷——先问 TS 有没有这道门；TS 没有的，写测试钉住现状并登记差异，不补门。**
能机检的落法：`specs/gateway-parity/diffs/<ID>-R<n>.md` 每条差异必须带探针原文行；负控脚本对每条既有门各有一条变异（缺失即 `--check` 报锚点漂移）。
