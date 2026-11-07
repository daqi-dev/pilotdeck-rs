---
id: D1-SDK-SIGNAL-RUNID-LITERAL-001
---

## 类别

已批准差异 · 动态字段。`sdk-signals` 96 格矩阵中 `run--unknown-cleanup` /
`resume--unknown-cleanup` 两格的 stdout 差异只有一个字段：
`recovery.runId`（每轮新 UUID）。产品行为（status / 退出码 / 连接 / 监听器 /
文件树）两侧完全一致；两格在登记的比较命令下继续 FAIL，作为**已登记的
预期红格**存在，不修改比较器、不修改金样、不修改代码，96 格分母与逐字节
严格度不变。

## TS 依据

固定基线 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，Node v22.22.0，
`src/pilotdeck-cli/tests/fixtures/sdk-signals/source.json` 的 96 格真实 TS CLI
观测。金样 stdout 保存**采集那一次**的固定 UUID：`run--unknown-cleanup` 的
`{"status":"result_unknown","recovery":{"sessionId":"d1-session","runId":"7a0c08c8-6b0f-4baa-a434-88a1169d8cc0"}}`
与 `resume--unknown-cleanup` 的
`"runId":"c19fd263-25a7-4427-87c5-f1637668e022"`。装置比较器
`src/pilotdeck-cli/tests/common/signal_replay.rs:1199` 发布的
`comparison.json` 归一化说明明文
`UUID bytes stay literal`（只归一化 TS SQLite 前缀与完整业务前缀的
Node v22.22.0 尾段）——即**故意不归一化 UUID，按字面比较**。

## Rust 行为

`src/pilotdeck-cli/src/sdk/turn.rs:257`
`let run_id = uuid::Uuid::new_v4().to_string();` 每轮生成新 UUID，经
`TextOutput`（`src/pilotdeck-cli/src/sdk/output.rs:203-239`）写入
`result_unknown` 终态行的 `recovery.runId` 字段（stdout surface）。因此
同一份代码连跑两次都与冻结金样的固定 UUID 不同，两格以
`differing surfaces: ["stdout"]` FAIL，红格数恒为 2、失败面恒为 stdout 单面。
与 TS 行为无产品语义差异：TS 同样每轮生成新 UUID，只是金样冻结了采集那
一次的值。

## 范围

**只覆盖这 2 格（`run--unknown-cleanup` / `resume--unknown-cleanup`）的
`recovery.runId` 一个字段**：该两格的其余比较面仍按既有规则严格比较——
`compare_surfaces`（`signal_replay.rs:1045`）实际比较的是 status /
naturalExit / cleanupExit / aborts / connections / activeAfterExit /
initial / final / milestones / stdout / stderr / connection closures /
listener removal；`wire` 只随 `actual.json` 落盘记录、**不参与比较**。
其余 94 格适用的既有比较规则不变（含已登记例外：31 格
`D1-SDK-WS-CLOSE-ORDER-001` 序不敏感、8 格
`D1-SDK-SIGNAL-UNHANDLED-REJECTION-001` stderr、4 格
`D1-SDK-WS-CLOSE-REASON-001` close reason）。不动比较器
（`tests/common/signal_replay.rs`）、不动金样（`source.json` /
`oracle.json`）、不动产品代码；`comparison.json` 的 `acceptedDifferences`
清单（`signal_replay.rs:1200-1202`）按 #2165 裁定**不更新、不含本条**，
不得把它当作已批差异全集。两格在 96 格验收下继续 FAIL 属本条登记的预期
结果，判据见「机器判据」：失败恰为这 2 格、且两格失败面均为 stdout 单面，
只数红格不算数。
#2165 已核：该差异不是 #1260 引入（#1260 分支与 `origin/main` 的
merge-base `c42027366` 直接跑同样 2 failed、`differing surfaces: ["stdout"]`
完全相同）。若未来把 `recovery.runId` 归一化（改比较器语义），须按冻结
装置批准程序另走独立批准，本条不构成授权。

## 机器判据

验收命令与全部运行前置的**正本**是
`src/pilotdeck-cli/tests/fixtures/sdk-signals/README.md`「Rust 回放与产品
负控」节的登记命令块（必须恰好执行 96 个测试，0 个测试不构成验收；
含 `scripts/cargo-slots.sh`、`PDRS_SIGNAL_RESULTS` 绝对证据路径、整批独占
28999 mock 端口车道与 `/tmp/pdrs-parity.lock` 目录租约的跨套件串行前置）；
本条不再内联命令副本，以免与正本漂移。**判据钉身份与失败面，不只钉数量**：
失败恰为 `run--unknown-cleanup` / `resume--unknown-cleanup` 两格，两格
`differing surfaces` 均仅 `["stdout"]`，其余 94 格 `ok`——若失败面含
stderr / abort / connections 等其它面，即超出本条范围、须另开条目。#1260
修复后全量实跑：`test result: FAILED. 94 passed; 2 failed; 0 ignored;
0 measured; 118 filtered out`，失败恰为本条两格、surface 均为
`["stdout"]`；原始 `test result` 行、逐格输出与实跑 HEAD 见
`_tmp/20261107-issue-1260/matrix/`（mbp2，HEAD
`14393b5f715a64f721b4ffd1d962db02c89d2daa`、dirty 0，见该目录
`HEADER.txt`；持 `/tmp/pdrs-parity.lock` 目录租约）。本条为登记件，无新增
机器判据；本地金样守卫 `signal_oracle_matches_recorded_source_and_local_files`
与 `approved_stderr_match_domain_is_exact` 继续钉住其余严格比较范围。

## 批准记录

已批准（已接受差异）：郑达奇 2026-11-07T11:45:19Z 于 #2165 评论原话
「2」（#2165#issuecomment-6706514345），即决策单选项 2「登记为已批
差异」——不动比较器、不动金样、不改代码，只新增本差异登记条目；撤回 =
删本条目（差异账本只追加，更正另开 `<ID>-R<n>`）。前置于 #1260 关单
（本条合入后该前置即清）。同类先例（按字面比较动态字段的登记口径）：
`D1-SDK-WS-CLOSE-ORDER-001`（31 格 UNSTABLE 仅序不敏感，其余严格）、
`D1-SDK-SIGNAL-UNHANDLED-REJECTION-001`（八个完整 ID 的 stderr 单字段）。
