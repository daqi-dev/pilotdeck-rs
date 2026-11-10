---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R14
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

更正（机制指针失真，不放宽判分）：基础条目 :15「`requests.rs` 的 `describe_server` 静态列出 capabilities……过滤逻辑仅针对 snapshot 三方法，无 session config 专属开关」、:27「capabilities 静态列表（含 snapshot 过滤分支）不包含上述 7 个方法名」、-R1 :24「静态列表仍不含这 7 个方法名」，与 -R3 :29「capabilities 在 `session_model_get` 后按 oracle 顺序逐字插入 7 项」、:37「`management_tests.rs` 四处 capabilities 期望数组随能力清单同步」五处机制描述已过时：#1457 时代的 requests.rs 字面插入与手抄期望数组已被 #1343/#1352（`e8c0d739f`，2026-11-07）整体换成 `CAPABILITIES` 表 + `capabilities::advertised()` 组合与 oracle 派生期望。行为结论（7 方法已接线、随能力表广告）以 -R3 关单为准，本件不重复、不放宽判分。

## 基线依据

同基础条目「TS 依据」（固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，Node v22.22.0）；-R3 登记的 oracle `contract.capabilities` 7 项逐字清单不变。本件不新增 TS 主张，只更正 Rust 侧机制指针。

## Rust 行为

（2026-11-11 origin/main `3378fac11` 实读）

`describe_server`（`requests.rs:163`）不再静态列出或逐字插入任何能力名：能力来源是 `native_process/capabilities.rs` 的单一 `CAPABILITIES` 表，经 `capabilities::advertised()`（`capabilities.rs:231-239`）按 provider 门与 #1305 方法注册表组合（`requests.rs:179` 调用）。7 个 session config 名在表内的现行位置：`session_model_set`/`session_model_clear`（`capabilities.rs:92-93`）、`set_permission_mode`/`set_session_thinking`/`output_styles_list`/`set_output_style`（:122/:124/:125/:126）、`seed_read_state`（:145），均为 `wired: compiled_in`（无配置轴，`capabilities.rs:42-44`）——即广告不依赖任何 session config 专属开关，与基础条目 :15「无 session config 专属开关」结论一致但机制来源已换。「过滤逻辑仅针对 snapshot 三方法」已不成立：过滤链在 #1343/#1352 后泛化为按 provider 门的表级组合（runs/snapshots/memory/cron/always_on 各自的门，`capabilities.rs:47-73`）。`management_tests.rs` 的四张手抄期望数组已删，期望由 `expected_capabilities()`（:1162-1184）从 `oracle.json` 派生（详见 `A8-NATIVE-SNAPSHOT-CAPABILITY-001-R2.md` 的同源更正）。

## 范围

仅更正账本文本：基础条目 :15/:27 的「静态列出」机制句与 -R3 :29 的「逐字插入 7 项」、:37 的「四处期望数组」机器判据描述。不重复 -R3 关单与其后各轮已登记的行为与判据；不涉 7 方法自身行为、门序、`native-config` 负控；不涉其他条目（`A8-NATIVE-SNAPSHOT-CAPABILITY-001-R2.md`、`A7.8-ALWAYS-ON-DEFERRED-001-R3.md` 同轮更正各自条目，-R11 :101-102 先例：跨家刷新走各家自己的更正轮）。

## 机器判据

（当前时点真实指针）7 名的广告由宿主精确集断言钉住：`production_host_management_and_restart` / `storage_root_cli_arg_wires_snapshot_capabilities` 期望 `expected_capabilities(true)`（unix 43 项，`management_tests.rs:707`/:1806），禁用侧 40 项（:1092/:1623）；7 名均在期望内，漏报即红。`hello_reports_the_same_capabilities_as_describe_server`（:1236）钉 describe 与 `hello_ok.serverInfo` 同源。实测：`cargo test --manifest-path src/Cargo.toml native_process::management_tests` 26 passed / 1 ignored / 0 failed。账本只追加检查：`python3 -I scripts/gateway-parity/check_frozen.py --base origin/main` 须 PASS。

## 批准记录

2026-11-11，AI 代批，待人复核（#1630）；未取得持久人签记录，非已接受差异，不新增任何判分豁免。依据：协调者冻结程序任务书（`_tmp/20261111-issue-1630/`，TASKBOOK-VERSION `2cebb53054d9`）指定按真实数据复算更正与 A8 计数失真同源的机制指针。人复核应以持久决策指针登记。
