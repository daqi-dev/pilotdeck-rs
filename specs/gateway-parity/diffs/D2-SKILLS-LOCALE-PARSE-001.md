---
id: D2-SKILLS-LOCALE-PARSE-001
---

## 类别

有意改进（登记，不收敛）：CLI skills collator 的 locale 解析器刻意比 gateway 简化
（`src/pilotdeck-cli/src/skills.rs:192-198` 现场注释自述），#2140 把两处已知分歧
按表格登记入账。本条只登记差异，不收敛 CLI 行为（见「范围」）。

## 基线依据

真实 Node 实测（钉版宿主 Node v22.22.0 / ICU 78.2，与金样宿主同 ICU；本轮在
cb8 复测，`node -p 'process.version + " " + process.versions.icu'` → `v22.22.0
78.2`，原始输出 `_tmp/20261109-issue-2140-2170/s1/node-pin.txt`）。子进程隔离环境
`LANG=LC_ALL=<值>` 下 `new Intl.Collator().resolvedOptions().locale`，探针脚本与
逐行原始输出见 `_tmp/20261109-issue-2140-2170/s1/node_locale_probe.mjs` /
`node-locale-probe.txt`：

- `de_DE@euro` → `de-DE-x-lvariant-euro`（四对 `coté`/`côte` 符号 `[-1,1,-1,1]`）
- `fr_XX_YY` 与 `fr_XX_YY.UTF-8` → `fr-XX-x-lvariant-yy`（符号同上）
- 对照行（与两侧均一致）：`C`→`en-US`、`de_DE.UTF-8`→`de-DE`、
  `fr_CA@collation=eor`→`fr-CA`；既有金样 `src/pilotdeck-gateway/tests/fixtures/node-locale/oracle.json`
  的 `en_US@modifier`→`en-US-modifier`、`en_US@foo`→`en-US-x-lvariant-foo`、
  `tr_TR_invalid!!!`→`tr-TR`。

gateway 正本按 `src/pilotdeck-gateway/src/node_locale.rs` 实跑核对、CLI 侧按
`skills.rs` 同一预处理串的 `Locale::parse` 结果核对（一次性探针测试，原始输出
`_tmp/20261109-issue-2140-2170/s1/rust-locale-probe.txt`，已从树中移除）。

## Rust 行为

同批 LANG 值下三个实现的 resolved locale（原件 = 实测 Node）：

| LANG | Node 实测 | gateway `node_locale.rs` | CLI `skills.rs` |
|---|---|---|---|
| `de_DE@euro` | `de-DE-x-lvariant-euro` | `de-DE-x-lvariant-euro`（与 Node 一致） | `de-DE`（在第一个 `@` 截断，丢 modifier） |
| `fr_XX_YY(.UTF-8)` | `fr-XX-x-lvariant-yy` | `fr-XX` | `en-US`（解析失败整体回落） |
| `en_US@modifier` | `en-US-modifier` | `en-US-modifier`（与 Node 一致） | `en-US` |
| `tr_TR_invalid!!!` | `tr-TR` | `tr-TR`（与 Node 一致） | `en-US` |
| `C` / `fr_CA@collation=eor` / `de_DE.UTF-8` | `en-US` / `fr-CA` / `de-DE` | 同左 | 同左 |

分歧归两类：①POSIX `@modifier`（非 `key=value` 关键字）：gateway 无 `=` 时升为
variant（`euro` 4 字节走 `x-lvariant` 兜底），CLI 连同其后整段丢弃；②畸形 tag：
gateway 保留 ICU 合法前缀，CLI 整体回落 `en-US`。如实登记：`fr_XX_YY` 行 Node
实测为 `fr-XX-x-lvariant-yy`，gateway 给 `fr-XX`（尾部 `x-lvariant-yy` 被丢）——
该行不在 node-locale/oracle.json 覆盖内，gateway 与 Node 在此未入账行上同样有差，
一并登记，不在本条收敛。

## 范围

仅 CLI skills collator 的 LANG/LC_* → locale 选择（`pilotdeck-cli` 内
`collator()`，服务 skills 目录名排序）；gateway 侧 `node_locale.rs` 行为为正本、
不改。四对探针对在所有分歧行上两侧符号一致（Node 实测 variant 不改 collation
tailoring），故已记录的排序行为无差；差异只出现在 resolved locale 字符串本身，
任何以 resolved locale 逐字对拍的场景会把两侧分开。本条不收敛 CLI 行为：收敛需
改 CLI 产品代码并复核 #132 冻结探针语境，前置负控虽已就位
（`src/pilotdeck-cli/tests/fixtures/skills-fr-ca-probe/negative_controls.py`），
仍留待后续单。

## 机器判据

- gateway 正本：`project_files::tests::node_locale_contract` 系回放
  `node-locale/oracle.json`（含 `en_US@modifier` / `en_US@foo` 行）。
- CLI 回放：`skills::tests::fr_ca_secondary_matches_recorded_node_probe` 回放
  `src/pilotdeck-cli/tests/fixtures/skills-fr-ca-probe.json`（6 行 LANG，不含
  分歧行——分歧行证据只在本轮 _tmp 实测原件）。
- 负控：`src/pilotdeck-cli/tests/fixtures/skills-fr-ca-probe/negative_controls.py`
  五条变异（`fr-ca-repair-dropped` / `fr-ca-region-widened` / `fr-ca-script-lost` /
  `fr-ca-script-widened` / `probe-resolved-rewritten`，中间两条对应 gateway 侧
  native-collation 的 french-script-fallback-lost / french-script-widened 同款双向，
  第五条绑定 #2170 的 resolved 自审）各绑专属断言消息；提交后宿主执行，任一非
  KILLED 非零退出。

## 批准记录

未批准，非已接受差异。#2140 只登记不收敛；若后续要收敛 CLI 解析，须按
`diffs/README.md` 另开条目并补人批。
