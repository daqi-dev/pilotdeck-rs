---
id: A7-LIFECYCLE-NFKC-001-R1
original: A7-LIFECYCLE-NFKC-001
---

## 类别

差异消除（bug 修复）。原条目（`specs/gateway-parity/accepted-diffs.md` 的 `A7-LIFECYCLE-NFKC-001`）类别「未实现（Unicode normalization）」随本次实现取消：work path segment 现改用 `icu_normalizer` 做完整 NFKC，原条目所述不完整折叠已消除。

原条目里被更正的是 `- **Rust 行为**：host/modules/lifecycle.rs::path_segment 只折全角 ASCII 与 U+3000；其他字符走非 ASCII run 的 - 替换，① 这样的单字符最终成 unknown`；同时更正原机器判据句「未新增 Rust 此入口的完整相等金样，未来接入完整 normalizer 时补」。

## 基线依据

原条目引用的 TS 基线不变：`cc7c4e6` 的 `agent/turn/TurnEnvironment.ts` 的 `safeWorkPathSegment` 对 session/turn 字符串做完整 `normalize("NFKC")`，之后替换、trim、截 96 单元。新增实测对应实现提交 `3548d79087e49b0f632adf8191c02a599f742668`（位于 `origin/main` 之上），原始输出：

- `_tmp/20261026-issue-335-345/nfkc-probe.out`：真实 `buildTurnEnvironment`，Node `v22.22.0`、Unicode 17.0，逐行给出 35 个 sessionId 的归一化后工作段，包括 `ﬁle → file`、`① → 1`、`㎏ → kg`、`Ⅳ → IV`、`℡ → TEL`、`½ → 1-2`。
- `_tmp/20261026-issue-335-345/hooks-runtime-generate.out`：入库 TS 金样生成器输出 `12 env cases`（原 6 条 + 新增 6 条 NFKC 用例）。
- `src/pilotdeck-gateway/tests/fixtures/hooks-runtime/oracle.json` 的 `lifecycle.envCases` 现为 12 条，`sourceCommit` 为 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。

新增 6 条金样的工作路径段（`sessionId` / `turnId` → `PILOTDECK_WORK_DIR` 的 session/turn 段）：

| case | sessionId / turnId | TS 金样 |
| --- | --- | --- |
| `nfkc-ligatures` | `ﬁle` / `ﬂow` | `file` / `flow` |
| `nfkc-enclosed` | `①` / `⑩` | `1` / `10` |
| `nfkc-units` | `㎏` / `Ⅳ` | `kg` / `IV` |
| `nfkc-halfwidth-katakana` | `ｶﾀｶﾅ` / `AｶB` | `unknown` / `A-B` |
| `nfkc-mixed` | `ﬁ①㎏` / `℡½` | `fi1kg` / `TEL1-2` |
| `nfkc-non-ascii` | `ß㍿` / `㌀Å` | `unknown` / `unknown` |

## Rust 行为

提交 `3548d79` 的 `src/pilotdeck-gateway/src/host/modules/lifecycle.rs` 的 `path_segment` 现为：

```rust
let normalized =
    icu_normalizer::ComposingNormalizerBorrowed::new_nfkc().normalize_utf16(value.units());
```

之后沿用原 ASCII 白名单、`-` 折叠、首尾 trim、96 单元截断与 `unknown` 回退。`normalize_utf16` 保留孤立代理项的 UTF-16 语义。依赖 `icu_normalizer 2.1.1` 是仓库锁文件已有包（经 `url` → `idna` → `idna_adapter` 传递），本次只把它声明为 `pilotdeck-gateway` 直接依赖，未新增下载包。

## 范围

work 目录中 `sessionId` / `turnId` 路径段的完整 NFKC 兼容折叠，包括连字、带圈字母数字、单位符号、罗马数字、半角片假名、`℡`、`½` 等兼容字符。ASCII 白名单、非法字符折叠为 `-`、96 单元截断、空段回退 `unknown` 的规则不变；不声称所有 Unicode 码点都能保留为可读字符，非 ASCII 结果仍按白名单折叠。原条目「未对齐」的工作段差异**已消除**。

## 机器判据

- `src/pilotdeck-gateway/tests/hooks_runtime.rs` 的 `lifecycle_dispatch_matches_real_ts` 逐条回放 12 个 env case；同一用例内断言 `env_cases.len() == 12`，逐值比较 `build_turn_environment` 的 `PILOTDECK_WORK_DIR` 与 TS 金样。正向实跑见 `_tmp/20261026-issue-335-345/final-tests.log`，`nfkc_rc=0`，该用例 `ok`。
- 金样重生成守卫：`_tmp/20261026-issue-335-345/fixture-guard.log` 的 `hooks_runtime_fixtures_regenerate_identically_from_ts ... ok`（`12 env cases` 的生成输出见 `hooks-runtime-generate.out`）。
- 负控：`_tmp/20261026-issue-335-345/negative/nfkc-partial.log` 把实现换回「只折全角 ASCII + U+3000」的旧算法后，`lifecycle_dispatch_matches_real_ts` 在 `env case nfkc-ligatures` 变红，`left .../work/le/ow` vs `right .../work/file/flow`；rc=101，`negative/summary.json` 记 `nfkc-partial` killed。恢复后通过。
- 复算：`cargo test -p pilotdeck-gateway --test hooks_runtime lifecycle_dispatch_matches_real_ts`；金样重生成守卫为 `cargo test -p pilotdeck-gateway --test hooks_runtime hooks_runtime_fixtures_regenerate_identically_from_ts -- --ignored`。

## 批准记录

原条目自己的批准记录写「无……已知未对齐（未批准）」，但 `specs/gateway-parity/accepted-diffs.md` 的「人批记录 · #505」已把 `A7-LIFECYCLE-NFKC-001` 列入 2026-10-23 人批「接受为已知差异（不豁免判分）」，原话见 `state/decisions-20261023.md`「#505 补登 32 条批复」。本次完整 NFKC 落地后差异已消除，**不再是已接受差异，无需新增豁免**；上述 2026-10-23 人批的对象（差异）已不存在，不再适用，这不是撤销人批，也不构成本更正的批准。本更正本身未批准，非已接受差异。
