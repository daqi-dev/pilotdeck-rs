---
id: A6-COLLATION-PUA-001-R1
original: A6-COLLATION-PUA-001
---

## 类别

不可避免（已退役）。原条目登记的是「孤代理先映射到 U+E000..E7FF 再交给 ICU4X，因而与真实 PUA 权重碰撞」这一实现限制；#132 改为原生 UTF-16 比较后该映射不复存在，本更正登记差异退役与判据替换，不改变原条目其它结论。

## 基线依据

原条目的 Node 实测仍有效：Node v22.22.0 / ICU 78.2 下 U+D800 < U+E000、U+D801 < U+E001（原条目引 `diff-backfill/oracle.json.collation`）。#132 新增的 comparator 金样 `src/pilotdeck-gateway/tests/fixtures/native-collation/oracle.json`（生成器 `generate.mts`，钉同一 Node v22.22.0 / ICU 78.2）把 62 个键——含 `\ud800`、`\ud801`、`\udfff`、``、``、``——在 14 个 locale 上两两展开为 62×62×14 = 53,816 对原始符号，碰撞对（如 `\ud800` × ``）就在其中逐对记录。

## Rust 行为

`src/pilotdeck-gateway/src/project_files.rs:667-673` 的 `node_compare_utf16` 改为调用 vendored `icu_collator` 的 `compare_utf16_preserving_surrogates`；原 `project_files.rs::node_collation_units`（PUA 映射）已随 #132 整体删除，全仓仅本账本旧条目仍提该函数名。孤代理按 ICU4C 的 implicit-weight 公式直接参与比较，不再借用 U+E000..E7FF 区间，故真实 PUA 与孤代理之间的「映射后 Equal」不再出现。

## 范围

原条目的差异范围——所有使用 `node_compare_utf16` 的排序里，真实 PUA 与相应孤代理同时出现在决定顺序的位置——整体退役。未覆盖：仍直接调用 `collator.compare_utf16(...)` 的排序点（`skills/`、`runtime/tools/registry.rs`、`runtime/plugins/contributions.rs`、`runtime/context/cache.rs` 等）不属本条目，其孤代理语义另计；snapshot 的显式宿主拒绝仍见 A6.6-SNAPSHOT-COLLATION-001，两者不可混为一谈。

## 机器判据

- `pairing::native_collation_tests::native_comparator_matches_node_all_pairs`（`src/pilotdeck-gateway/tests/fixtures/native-collation/regression.rs:146`）：53,816 对逐对比较 Rust 与 Node 金样符号，含 U+D800 × U+E000 对照；原条目的判据 `node_compare_utf16(collator, [D800], [E000]) == Equal` 因此作废，改由该矩阵断言。
- `project_files::tests::node_compare_utf16_keeps_unpaired_surrogate_weights`（`project_files/tests.rs:492`）：2,048 个孤代理两两严格递增、互不折叠。
- 负控：`tests/fixtures/native-collation/negative_controls.py` 就地变异 vendored `comparison.rs` 的 preserving 路径，要求定向用例失败。复算：`cargo nextest run -p pilotdeck-gateway -E 'test(native_comparator_matches_node_all_pairs)'`。

## 批准记录

未批准，非已接受差异。原条目 A6-COLLATION-PUA-001 本身即「无批准记录；已知未对齐（未批准）」（`specs/gateway-parity/accepted-diffs.md:161` 起）。本更正只登记该差异已由 #132 消除并替换判据，不增加任何判分豁免，也不改变其它条目的批准状态；退役结论待人复核。
