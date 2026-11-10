---
id: D2-SKILLS-LOCALE-PARSE-001-R1
original: D2-SKILLS-LOCALE-PARSE-001
---

## 类别

同原条目 `D2-SKILLS-LOCALE-PARSE-001`（有意改进 · 登记不收敛）。本更正只同步
#2170 扩容后的负控名册事实（四条 → 七条变异），不改变差异类别、登记范围与
「不收敛 CLI 行为」的结论。

## 基线依据

同原条目：真实 Node 实测（宿主 Node v22.22.0 / ICU 78.2 钉版，
`_tmp/20261109-issue-2140-2170/s1/node-pin.txt` 与
`node-locale-probe.txt` 原始输出）。#2170 未新增分歧行基线：三条新变异
（`probe-resolved-rewritten` / `probe-language-emptied` /
`probe-resolved-prefix-poisoned`）钉的是 #2170 在
`skills.rs` 探针测试里新增的 resolved 自审与空语言守卫本身，金样仍是
`src/pilotdeck-cli/tests/fixtures/skills-fr-ca-probe.json` 的原 6 行 LANG
（无新增行、无新金样）。

## Rust 行为

同原条目：CLI `skills.rs` 的 LANG/LC_* → locale 选择行为不变。#2170 在
`skills::tests::fr_ca_secondary_matches_recorded_node_probe` 中为每行
fixture 增加两道前置自审（resolved 首子标签须等于该行语言子标签；语言子标签
不得为空），产品代码 `collator()` 不受影响。

## 范围

仍只覆盖 CLI skills collator 的 locale 选择；原条目「范围」节的全部结论与
分歧行登记不变。本更正仅将「机器判据」中负控名册从四条改为七条：新增
`probe-resolved-rewritten`（改写 Cyrl 行 `"resolved": "fr-Cyrl-CA"` →
`"en-US"`，自审必红，绑 `#2170 fr_Cyrl_CA.UTF-8: resolved`）、
`probe-language-emptied`（改写 fr_CA 行 `"lang"` → `"_CA.UTF-8"`，空语言
守卫必红，绑 `#2170 _CA.UTF-8: probe row needs a language subtag`）与
`probe-resolved-prefix-poisoned`（fix-r6：改写 fr_FR 行 `"resolved"` →
`"frr-FR"`，钉住自审的首子标签相等判据——退回前缀式则该行 SURVIVED，绑
`#2170 fr_FR.UTF-8: resolved`），均经 `include_str!` 重编译生效，提交后宿主
执行。

## 机器判据

负控正本 `src/pilotdeck-cli/tests/fixtures/skills-fr-ca-probe/negative_controls.py`
现为七条变异（`fr-ca-repair-dropped` / `fr-ca-region-widened` /
`fr-ca-script-lost` / `fr-ca-script-widened` / `probe-resolved-rewritten` /
`probe-language-emptied` / `probe-resolved-prefix-poisoned`；中间两条对应
gateway 侧 native-collation 的 french-script-fallback-lost /
french-script-widened 同款双向，后三条绑定 #2170 的自审守卫），名册由静态
`EXPECTED_NAMES` 钉死（漂移 rc=2）；提交后宿主执行，任一非 KILLED 非零退出。
CLI 回放测试与金样不变（同原条目）。

## 批准记录

未单独报批：本更正为名册事实性修订（#2170 给两道新自审守卫补配对变异的直接
后果，防止按旧「四条变异」字面名册产生审计假 FAIL），不改变原条目的登记
范围、判分语义与豁免；七条名册由 `EXPECTED_NAMES` 静态钉死并经双谱系评审
（deepseek + anthropic）。AI 代批，待人复核。
