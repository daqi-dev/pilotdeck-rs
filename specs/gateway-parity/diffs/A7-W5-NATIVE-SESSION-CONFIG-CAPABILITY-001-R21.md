---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R21
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

更正件（R 链，承接 `A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R20`；frontmatter
`original` 按账本机器规则指基础 ID，与 R9-R20 同法，承接关系由文件名与正文表达）。
三件事，均不放宽既有登记的判分。其一（#2692 fix-r6 漏记，r7 评审 sibling#1 P1）：
fix-r6 给 native-blankkey 名册新增第 17 行 `registry-throttle-removed`（节流半面杀点）
未入账，R20 登记的 16/15/16 与树内 17/16/17 脱节（R16 同类：登记数字与名册脱节）。
其二（#2692 fix-r7，评审 concurrency#1/#3 ≡ boundary#1/#2 P1）：`report_unrekeyable`
的声明-写序恢复原子（锁内 `insert` 一步选主，写失败释放声明），探针父进程改
有界等待（先 kill+reap 再排空管道，30 s 期限）——随修法新增两行
`registry-mark-before-write`（声明释放臂的杀点）与 `registry-probe-child-stuck`
（子进程死锁时父进程须在期限内翻红而非挂死）、重锚两行 `registry-skip-writer-silent`
（写移入 `write_skip_report`）与 `registry-throttle-removed`（原子选主函数为锚），
名册终值 19/18/19。其三（r7 评审 sibling#6 / simplify#8，协调者裁定本轮不开新
follow-up）：fix-r6 在 `issue-1604-require-text` 名册补的两行 String-only 变异
（`steer-admission-string-only` / `active-turn-admission-string-only`）记豁免行
（见「范围」），不入差异账本。R15/R19/R20 原字节不动，账本只追加。

## 基线依据

本 worker 树（本机 wt-2692，fix-r7 未提交工作树上实测后随轮提交，Node v22.22.0）
实跑（2026-11-15）：

- `python3 -I src/pilotdeck-gateway/tests/fixtures/native-blankkey-submit/negative_controls.py --check`
  → `#2692 19 mutation anchors OK; 18 literal anchors OK; 19 bound tests declared`
  （exit 0）。表点数：`mutations` 19 行、`EXPECTED_NAMES` 19 名、`LITERAL_ANCHORS`
  18 项。夹具 README（`native-blankkey-submit/README.md`）同步为 19 行/18 锚。
- `python3 -I -B src/pilotdeck-gateway/tests/fixtures/native-blankkey-submit/test_literal_anchors.py`
  → 13 例 OK（含「裸字面量不入 SOURCE_TEXTS 扫描集即拒」各红侧）。
- `python3 -I src/pilotdeck-gateway/tests/fixtures/issue-1604-require-text/negative_controls.py --check`
  → `#1604 6 mutation anchors OK`（exit 0；6 = A7.2 已登记 2 行 + fix-r6 新增
  String-only 2 行 + 其余 2 行）。
- turn 三家未漂移：`turn-read: 27 mutation anchors OK; 27 bound tests declared`、
  `turn-publish: 19 mutation anchors OK; 19 bound tests declared`、
  `turn-submit: 57 mutations + 1 baseline; anchors OK`（均 exit 0，与 R20:63-64
  登记一致；fix-r7 只把三家的名册完备子句收敛进共享
  `negative_selection.require_pinned_by_roster`，红侧进各夹具
  `test_bound_test_gate.py`，7-9 例 OK）。
- `python3 -I scripts/gateway-parity/check_frozen.py --base 0f15f90955f7d891ef3025641007f66d75e0e109`
  → PASS（entries=30；本件入账前后各跑一次均 PASS——`entries` 数的是
  FROZEN.md 名册行，`--write` 登记是协调者提交后的动作，非 worker 轮内步骤；
  本件的文件名/六节/frontmatter 校验（`check_diffs`）零失败）。

## Rust 行为

fix-r7 的两处行为变更（`storage/run_registry.rs` / `run_registry_tests.rs`，
#2692 评审 concurrency#1 ≡ boundary#2、concurrency#3 ≡ boundary#1，均 P1）：

- `report_unrekeyable` 恢复单写者原子性：`claim_report_slot`（锁内
  `HashSet::insert`，插入成功即首次，`first_report_for` 形）一步完成声明-选主，
  并发首读者恰好选出一名写者；写（`write_skip_report`）在锁外，`Ok` 保留声明、
  `Err` 经 `release_report_slot` 释放，下次读重试（fix-r6 重试契约保留）。
  依据 #2301/#2558 先例（`native_process/channels_tests.rs` 探针形、`upstream.rs`
  有界等待形）。
- 探针测试父进程半面改有界等待：kill+reap 之后才读 stderr（旧序先读管道再杀，
  满管道即父进程无界挂死），stdout/stderr 各起一条排空线程，`try_wait` 30 s
  期限，超时先 `kill`+`wait` 再收排空结果，以带内断言失败（非超时挂死）收局。

## 范围

只覆盖三处：R20 登记数字与树内现值的对齐（blankkey 16/15/16 → 终值 19/18/19，
含 fix-r6 漏记的第 17 行与 fix-r7 新增/重锚的各行）、上述两处行为变更的杀点
登记、issue-1604 String-only 两行的豁免行。不覆盖、不改动：R15 四族差异的语义
与批准状态；native-config 131 行名册（R20 已对齐，未漂移）；带内跳过记录在
persist 重写时被丢弃这一行为（结构性修法仍待裁决）。豁免行（其三）：
`steer-admission-string-only` / `active-turn-admission-string-only` 两行守的是
fix-r5 迁到 router 域后的**对齐面**（回退 String-only 守卫会破坏与 TS 的一致，
变异被杀恰因 Rust 必须与 TS 同形）——它们不对应任何已登记差异族，差异账本按
定义只登记 TS/Rust 行为分歧，无族可挂；`A7.2-STEER-INPUT-001:226-237` 仍是其
同夹具已登记两行（`item-id-guard-reintroduced` /
`claim-gate-truthiness-restored`）的登记判据，本行不改变。不放宽判分、不新增
判分豁免（名册完备、锚唯一、绑定测试声明诸门照旧）。

## 机器判据

- 静态核（本树实测）：native-blankkey `--check` → `#2692 19 mutation anchors OK;
  18 literal anchors OK; 19 bound tests declared`（exit 0）；issue-1604
  `--check` → `#1604 6 mutation anchors OK`（exit 0）；
- 名册等值：blankkey `EXPECTED_NAMES` 19 行（第 17 行 `registry-throttle-removed`、
  第 18 行 `registry-mark-before-write`、第 19 行 `registry-probe-child-stuck`），
  字面锚 18 项（新增
  `(run_registry_tests.rs, '#2692 a failed report write must not mark the path as reported')`
  与 `(run_registry_tests.rs, '#2692 the stderr-line probe child did not finish
  within 30s')`）；
- 行为钉：`storage::run_registry_tests::unrekeyable_skip_logs_the_stderr_signal_once_per_path`
  （探针形：token 门 + 有界等待 + 精确计数 1）与
  `storage::run_registry_tests::unrekeyable_report_retries_after_a_failed_write`
  （`ReportWriteFailureGuard` 注入真 `Err(BrokenPipe)`，重试半面）并立，四行杀点
  各挂其上（`registry-skip-writer-silent` / `registry-throttle-removed` /
  `registry-mark-before-write` / `registry-probe-child-stuck`）；
- 账本闸门：`check_frozen.py` 规则要求本件 frontmatter `original` 写基础 ID
  （已写 `-001`）；FROZEN.md 名册行由协调者 `--write` 登记后复跑；全量杀灭轮
  （需 cargo 与净树）由协调者
  提交后在宿主跑，杀灭判据以 `EXPECTED_NAMES` 19 行为分母。

## 批准记录

未批准，非已接受差异，待人复核（可否决）。2026-11-15，#2692 fix-r7 worker AI
代登记。本件是登记数字与树内名册的对齐（fix-r6 漏记行入账、fix-r7 新增/重锚
行入账）、两处行为变更的杀点登记、issue-1604 String-only 两行的豁免行记录，
不改变任何差异族的跳过/失败分工语义、判分规则或豁免；R15/R19/R20 原字节不动，
账本只追加。人复核否决则循账本规则出 `-R22` 更正。
