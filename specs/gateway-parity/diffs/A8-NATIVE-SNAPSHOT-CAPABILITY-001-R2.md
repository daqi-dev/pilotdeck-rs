---
id: A8-NATIVE-SNAPSHOT-CAPABILITY-001-R2
original: A8-NATIVE-SNAPSHOT-CAPABILITY-001
---

## 类别

更正（计数与指针失真，真实数据复算，不放宽判分）：原条目与 -R1 的能力计数（禁用 10 项 / 启用 13 项）是 2026-11-01 原条目诞生时点的真实值，此后 2026-11-04 接线波（#1311 运行时工具 19 项 → #1319 U6 MCP 23 项 → #1321 U1 upload 30 项，#1322 U2 memory 因夹具未绑定该对 provider 不改计数）与 #1316 会话域扩表（33 项）、#1352（PR #2124）把手抄期望数组换成 oracle 派生，账本数字与负控名称/绑定口径未跟随。行为语义（缺省禁用、显式启用、入口拒绝继承环境）不变，本件只修账本文本；原条目与 -R1 原字节不动。

## 基线依据

同原条目固定基线 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，Node v22.22.0 / ICU 78.2。TS 行为不变；`oracle.json` 的 `contract` / `contract.emptyStorage` / `contract.missingStorage` 三格 capabilities 本树实测各 50 项。本件不新增 TS 主张，只更正 Rust 侧计数、机制与负控指针。

## Rust 行为

（以下为 2026-11-11 origin/main `3378fac11` 实读复算）

计数机制已变：#1352（PR #2124，`e8c0d739f`，2026-11-07）删除了 `management_tests.rs` 四张手抄期望数组，期望集合由 `expected_capabilities()`（`management_tests.rs:1162-1184`）从 `oracle.json` 的 `contract.capabilities`（50 项）实时派生：减 `capabilities::NOT_ADVERTISED`（5 项：`session_transcript_archive`、`apply_flag_settings`、`update_settings`、`reload_output_styles`、`sdk_session_config`）、减夹具未绑定 provider（`PROVIDERS_UNBOUND_IN_FIXTURE`，`management_tests.rs:1146`：`memory_list`/`memory_wipe`），快照禁用态再减三个 snapshot 名（:1172-1175）。因此：

- 启用（测试显式非空 storage / `--storage-root`）：unix **43 项**；
- 禁用（生产入口缺省）：unix **40 项**；
- Windows 另减 `background_task_stop`/`background_tasks` 两项（:1180），即 41/38。

计数史（逐 commit 用 `_tmp/20261111-issue-1630/count_arrays.py` 实测手抄数组条目数，按 禁用/启用 计）：10/13（`4a200db69`，2026-11-01，原条目诞生，当时真实）→ 20/23（`1b1c2aa3e`，2026-11-04，-R1 诞生时点；-R1 照抄 10/13，出生即陈旧）→ 27/30（`f24272160`，2026-11-05）→ 30/33（`7320d9439`，#1316，2026-11-05）→ 手抄数组删除、oracle 派生（`e8c0d739f`，2026-11-07）→ 40/43（当前）。夹具 README 的 10/13→27/30 修正是 #1440 评审 r2（`9bdff9557`，2026-11-06）做的；27/30 与 `f24272160` 的数组一致，但该 commit 出生时点的树上已因 #1316（11-05 合入）实为 30/33，即该 README 修正落地时即已陈旧。

原条目 :21 「`requests.rs::describe_server` 使用缓存和显式的三个能力名过滤」机制已不存在：#1343/#1352 后 describe 由 `capabilities::advertised()`（`capabilities.rs:231-239`）从单一 `CAPABILITIES` 表按 provider 门组合（`requests.rs:163`，调用 `:179`）；三个 snapshot 名在表内以 `wired: snapshots` 声明（`capabilities.rs:113-115`），该门函数读 `requests.snapshot_enabled` 缓存（`capabilities.rs:53-55`）——「不再次解析配置、用缓存」仍属实，「显式三能力名过滤在 requests.rs」不属实。

## 范围

只修账本文本：原条目 :19/:25/:29 的「10 项」「13 项」、:21 的 requests.rs 过滤机制句、:29 的「各精确集合均交叉校验为 TS contract.capabilities 子集」、:31 的负控名称与绑定；-R1 的 :24「10 项」、:26「13 项断言」、:28「13 项能力」、:30「13/10 项」「三个 `snapshot-*-filter-omitted`」「生产 10 项」。不改任何行为、测试、负控与夹具；不涉其他条目（姊妹失真由各自条目本轮更正：`A7.8-ALWAYS-ON-DEFERRED-001-R3.md`、`A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R14.md`）。

## 机器判据

（当前时点真实指针）

- `production_entry_snapshot_capabilities_are_disabled`（`management_tests.rs:1533`）：期望 `expected_capabilities(false)`（:1623），unix 40 项精确比较（原「10 项」）。
- `production_host_management_and_restart`（:626）：期望 `expected_capabilities(true)`（:707），unix 43 项，断言消息 `#1231 exact wired capability set`（原「13 项」）。
- `missing_storage_environment_disables_snapshots`（:1106）/ `empty_storage_environment_disables_snapshots`（:1111）：经 `assert_storage_disabled`（:1001）期望 `expected_capabilities(false)`（:1092），40 项。
- `storage_root_cli_arg_wires_snapshot_capabilities`（:1729）：`--storage-root` CLI 路径，期望 `expected_capabilities(true)`（:1806），43 项。
- TS 子集交叉校验 `assert_ts_capability_subset`（:1218-1228）现在只覆盖两个禁用用例（:1098）与生产入口（:1629），比较对象是 `contract.emptyStorage` / `contract.missingStorage` 而非 `contract` 本身——#1352 起 expected 从 `contract` 派生，拿 `contract` 自比恒真；启用表两侧用精确相等（强于子集），#1761 只要求禁用表在两个禁用状态各自合法。原「各精确集合均交叉校验为 TS contract.capabilities 子集」按此更正。
- 实测：`CARGO_TARGET_DIR=<树内> cargo test --manifest-path src/Cargo.toml native_process::management_tests`：26 passed / 1 ignored（`regenerates_identically`）/ 0 failed。

负控更名与重绑（`tests/fixtures/native-management/negative_controls.py`，M6 共享件，本件不改）：

- `snapshot-{list,get,restore}-provider-ignored`（:275/:278/:281）：即原条目「三个 `snapshot-*-filter-omitted`」的现行名（#1352/#2124 更名），变异把 `CAPABILITIES` 表内对应 `wired: snapshots` 改为 `wired: compiled_in`，绑定 `production_entry_snapshot_capabilities_are_disabled`，专属判杀 `#1231 production entry exact capability set`（生产 40 项，原「绑定生产 10 项」）。
- `wired-capabilities-outside-ts`（:288）：不再以 TS 子集校验判杀——#1352 起 expected 与 oracle 同源，子集比较两边同动；改由宿主用例的精确集断言（`#1231 exact wired capability set`）判杀（原「绑定 TS 子集」）。禁用态子集校验由 `empty-storage-capabilities-outside-ts` / `missing-storage-capabilities-outside-ts`（:297/:302）以各状态专属消息判杀。
- `snapshot-state-disabled/enabled`（:270/:271）、`production-*-authority-accepted`（:272-274）、`*-storage-defaulted`、`snapshot-storage-omitted`（:86）名称与绑定口径不变，仍按原条目，但绑定断言的项数随期望派生为 43/40。

## 批准记录

2026-11-11，AI 代批，待人复核（#1630）；未取得持久人签记录，非已接受差异，不新增判分豁免。依据：协调者冻结程序任务书（`_tmp/20261111-issue-1630/`，TASKBOOK-VERSION `2cebb53054d9`）指定按真实数据复算更正本条计数与指针。复核以本件登记的 commit、行号与实测命令为准，逐项可复算。
