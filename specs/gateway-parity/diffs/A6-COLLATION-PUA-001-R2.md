---
id: A6-COLLATION-PUA-001-R2
original: A6-COLLATION-PUA-001
---

## 类别

更正（计数与覆盖口径）。R1 已正确登记「PUA 映射退役」，但它引用的 `62 键 × 62 × 14 = 53,816 对` 是按声明的键数组长度写的，而该数组里 `'a\ud800b'` 出现了两次，真实唯一键只有 61 个。本更正不改 R1 的退役结论，只更正计数、给出重生成的实测依据，并补记 R1 未点名的 native-archive 边界退役。

## 基线依据

R1 引用的 Node 实测（Node v22.22.0 / ICU 78.2 下 U+D800 < U+E000、U+D801 < U+E001）不变。生成器 `src/pilotdeck-gateway/tests/fixtures/native-collation/generate.mts` 的 `keys` 数组在更正前声明 62 项、唯一 61 项；本单删去第二处 `'a\ud800b'` 后，用同一台金样机（实测 `node -v` = `v22.22.0`、`node -p 'process.versions.icu'` = `78.2`）重新生成 `oracle.json`。逐格复核：新旧 oracle 的 14 个 locale 行不变，公共键对（61×61×14 = 52,094 对）的符号**零差异**，被删的只有重复行/列本身。

选择「去重后重生成」而不是「把断言改成按唯一键数」：后者会让金样永久带着一对重复行列继续被当作 62 键使用，README 与差异件的覆盖率口径继续虚高一位；重生成后 `keys.len() == 61`、`61×61×14 = 52,094` 与数据一致，`regression.rs` 同时断言键唯一。

## Rust 行为

不涉及产品代码：比较器实现与 R1 描述一致（vendored `compare_utf16_preserving_surrogates`）。本更正涉及的是尺子侧数据与断言：`src/pilotdeck-gateway/tests/fixtures/native-collation/oracle.json` 已由上述生成器重生成（`sourceCommit` 仍为 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，`sourceSha256` 表与原文件相同），`regression.rs` 改为 `keys.len() == 61` 且新增唯一性断言，README 的计数改为 61 / 52,094。

## 范围

R1 的差异范围（`node_compare_utf16` 路径上的 PUA 碰撞）与「未覆盖的普通 `compare_utf16` 排序点」结论均不变。补充 R1 未点名的第三处退役：`src/pilotdeck-gateway/src/native_archive.rs` 原在重复 `sequence` 组含孤代理 `createdAt` 时 `return Ok(None)` 要求宿主作答，该分支已删除，排序改调 `compare_utf16_preserving_surrogates`。native-archive 边界没有独立账本条目（原记录在 A4 archive 的配对语义里），故在此点名，不另开新 ID、不改任何既有条目。transcript 与 snapshot 两处退役分别见 `A6.2-TRANSCRIPT-COLLATION-001-R1.md` 与 `A6.6-SNAPSHOT-COLLATION-001-R1.md`。

## 机器判据

- `pairing::native_collation_tests::native_comparator_matches_node_all_pairs`：52,094 对逐对比对 Rust 与 Node 金样符号；断言 61 键且唯一。该测试同时经 `crate::test_baseline::oracle(...)` 校验 `sourceCommit` 与非空 `sourceSha256` 表。
- `native_archive::tests::archive_lone_surrogate_ties_answer_natively_with_ts_bytes`：真实 TS 默认装配的 11 例（`tests/fixtures/native-archive/host-boundary.json`，sha256 钉在测试里）全部本地应答并逐字节相等。
- 负控（`tests/fixtures/native-collation/negative_controls.py`）：`archive-host-remains` 把 `Ok(None)` 边界装回 archive 必须变红；`oracle-commit-corrupt` 改坏 `oracle.json` 的 `sourceCommit` 必须让 `native_comparator_matches_node_all_pairs` 变红（绑定 `#1109 oracle baseline commit mismatch`）；`catalog-local-method-lost` 从 `dispatch::LOCAL_METHODS` 删掉 `model_catalog_list` 必须让 `matrix_pairing_has_zero_business_upstream` 变红（绑定其失败 locale 子用例名），补上该测试原先缺失的反向变异。
- ignored 重生守卫 `native_collation_regenerates_identically`（`src/pilotdeck-gateway/tests/generator_provenance.rs`）逐字节重生该金样。

## 批准记录

未批准，非已接受差异。R1 与本更正的退役/更正结论均待人复核；原条目 A6-COLLATION-PUA-001 的批准状态不变（`specs/gateway-parity/accepted-diffs.md:161` 起，「已知未对齐（未批准）」）。本更正不增加任何判分豁免。
