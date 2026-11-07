---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R10
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

同原条目（R 链，承接 `A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R9`），类别
不变：登记 + 补齐 R7 明示未闭合的实测缺口，不改产品行为，不新增判分豁免。本
更正只变更批准状态（未批准 → 已批准（AI 预批复，AI 代批待人复核，可否决）），
不改差异内容、判定规则与判分语义，不改任何已有条目字节。

## 基线依据

同原条目 `A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R9`，不变：固定基线
`cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`（`resources/PilotDeck`，只读），
真实链路探针（`_tmp/20261107-issue-2090/ts-probe/`）与 Rust 请求级实测
（`_tmp/20261107-issue-2090/rust-probe*/`、`rust-negctl*/`）均按该条目
「TS 依据」/「Rust 行为」两节所引。

## Rust 行为

同原条目，不变。

## 范围

同原条目，不变（五条差异的覆盖与不覆盖清单、`config` 族与 `#2199` 边界、
「本轮不对齐 TS」的申明均按 R9；①② 两子项的处置见「批准记录」）。

## 机器判据

同原条目，不变：TS 探针 `probe.mjs`（47 条）/ `probe-null.mjs`（20 条）的
复算命令、cb8 上的绿跑（`rust-probe-r10/gate-order-tests.out`，源 sha256
`1ef473fa…`）与两负控（准入变异 `admission-off-mutant.out`、null 格定向
变异 `null-cell-mutant.out`）及还原核验，命令与原始输出指针全按 R9
「机器判据」节。本件为纯批准状态更正，无新增代码/测试改动，不做形式编译或
全量对拍（CLAUDE.md 三层验证：检查按改动范围选）；账本闸门复算
`python3 -I scripts/gateway-parity/check_frozen.py --base origin/main`
须 PASS。

## 批准记录

**已批准（AI 预批复：AI 代批，待人复核，可否决）**。等级如实：依据
2026-11-07 20:3x 人令「待人判断单 AI 按既往批复风格先批复并报备，可否决」
（memory「ai-pre-verdict-policy」）由 AI 预批复，**待人复核尚未发生**
（#2090 无人回复记录）；这不是 `A8.3-CH-WEIXIN-ATTACH-001` / `-002` 那级的
人批（#2203 人评 2026-11-07T06:40:15Z「按建议：①」）。

R9「批准记录」原文登记的两件待批事项，处置见 #2090 两条 coord 回执（均带
`<!-- coord -->`，以下为原文摘录）：

**回执 6707009289**（zhengdaqi，2026-11-07T12:34:41Z，标题「协调回执：两问
一并批复（AI 代批，待人复核，依据 2026-11-07 20:3x 人令；可否决）」）：

> ## ① resume/close 的 sessionKey 门：保持 vs 放开
>
> **批复**：按建议 ①（保持 Rust 门），R8 该子项按已接受差异登记——理由：TS
> 侧是静默空操作，显式报错更可纠错，且与 #1741 / #1754-② 先例一致。
>
> ## ② projectKey: null：保持全拒 vs nullish 回退对齐 TS
>
> **批复**：按建议 ①（nullish 回退对齐 TS）——理由：TS 把 null 与未给同义、
> Rust 自己对未给也回退，全拒只是自相矛盾伤合法客户端；同日 #2083 人对同族
> （projectKey 孤立代理项）即拍「选项 1 对齐 TS」，本卡探针条件已满足（两侧
> 均已实测）。

**回执 6707020393**（zhengdaqi，2026-11-07T12:35:52Z）：

> 补充（下游执行现状）：② 的实现单已开 **#2272**（fix(gateway) #2090b：
> projectKey 显式 null 按 nullish 未提供处理（准入对齐 TS）；排 #2083 同族
> 车道之后串行、#2208 合入后才开工）。

回执中的「R8」即本条目：#2208 入队前因与 #2083 车道同名 R8 件 add/add 冲突
改号为 R9，合入件为 `-R9`（回执 6707009289「下游执行现状」段记的就是这次
冲突与停手）。据此，两件待批事项的处置：

- **① `resume_session` / `close_session` 上 Rust 多出的 `sessionKey` 门
  （R9 差异第 1 条）**：保持 Rust 门，该子项登记为已接受差异（AI 代批，
  待人复核，可否决）；
- **② 隔离项目准入对 `projectKey: null` 的处理（R9 差异第 5 条）**：nullish
  回退对齐 TS，走收敛而非接受差异——实现单 #2272（OPEN，2026-11-08 00:1x
  本件开单前核），该子项不转已接受差异。

**状态**：已批准（AI 预批复，AI 代批待人复核，可否决），自回执时点
（2026-11-07T12:34:41Z）生效。原始条目
`A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R9` 随 PR #2208
（2026-11-07T14:54:16Z MERGED）合入时，其「批准记录」段文本仍写「未批准，
非已接受差异。……待批事项两件，都属行为/判分语义变更，需人批」——入队后
不再 push、不改原字节，本件为更正件：不改原始条目与 `accepted-diffs.md`
的任何字节，批准事实以 #2090 两条 coord 回执为准。

本条不放开 `resume_session` / `close_session` 的 `sessionKey` 门、不新增
判分豁免、不改判分语义。若人复核否决（部分或全部），按否决意见在 #2090
决策线上另行拍板：被否决的子项或转收敛（另开实现单）、或回到未批准登记，
届时循本账本规则再出 `-R11` 更正；`#2199` / `#2207` 两张「前置于 #2090」
的跟进单不受本件影响。
