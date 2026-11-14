---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R20
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

更正件（R 链，承接 `A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R19`；frontmatter
`original` 按账本机器规则指基础 ID，与 R9-R19 同法，承接关系由文件名与正文表达）。
三件事，均不放宽既有登记的判分。其一（#2960 fix-merge-r4，MM specs3 sibling#1
P1）：更正 R15/R19 两处对 native-config 一家负控的机器判据登记——merge-main-r3
把 2692 侧 70 行表并回后名册扩到 131 行，账本未跟，登记命令在树内已不可复现
（R16 同类：登记数字与名册脱节，验收证据缺口）。其二（同轮，条2）：native-blankkey
名册新增第 16 行 `registry-skip-writer-silent`（stderr 写侧的独立杀点，MM sibling#1
b 档按 P1），R19:119 登记的 15/14/15 同步更正为 16/15/16。其三（同轮，MM src2
concurrency#1 P1）：R19:83-86 登记的差异④信号面措辞——「每次实际发生跳过的读向
stderr 发一行」按 fix-merge-r4 的锁序修法更正为「每路径/每进程一次，且在读面的
锁（进程内 chain 互斥与跨进程文件锁）全部释放后发出」。R15/R19 原字节不动，账本
只追加。

## 基线依据

本 worker 树（本机 wt-2692，fix-merge-r4 未提交工作树上实测后随轮提交）实跑：

- `python3 -I src/pilotdeck-gateway/tests/fixtures/native-config/negative_controls.py --check`
  → `#1457 131 mutation anchors OK; #2065 85 literal anchors OK; 131 bound tests
  declared`（exit 0）。表点数：`mutations` 131 行（`negative_controls.py:114` 起）、
  `EXPECTED_NAMES` 131 名（`:1424`）、`LITERAL_ANCHORS` 85 项（`:1740`）、
  `AUDITED_MODULES` 11 项（`:1872`）。夹具自家 README（`native-config/README.md:42`）
  已登记「当前 131 条变异……`#1457 131 mutation anchors OK` 对齐」「131 名」。
- `python3 -I src/pilotdeck-gateway/tests/fixtures/native-blankkey-submit/negative_controls.py --check`
  → `#2692 16 mutation anchors OK; 15 literal anchors OK; 16 bound tests declared`
  （exit 0）。第 16 行 `registry-skip-writer-silent` 只删
  `run_registry.rs` 的 `console_log_line` 写（计数器照常前进），绑定测试
  `storage::run_registry_tests::unrekeyable_skip_logs_the_stderr_signal_once_per_path`
  （re-exec 探针形：父进程持子进程 stderr 管道，断言带内跳过的那行 stderr 在场）。
- `python3 -I scripts/gateway-parity/check_frozen.py --base <merge-base>` →
  `{"status": "PASS", ..., "entries": 30}`（本件入账前后各跑一次）。

## Rust 行为

其三的信号面变更（`storage/run_registry.rs`，#2960 fix-merge-r4）：

- `read_snapshot` 内不再写 stderr——跳过计数（`snapshot.unrekeyable` 与累计
  `UNREKEYABLE_SKIPPED`）仍在锁内；stderr 行移入新函数 `report_unrekeyable`，
  由三个读面（`get` / `events` / `mutate`）在各自 guard 作用域结束后调用，
  `mutate` 的 chain 互斥与跨进程文件锁在写发生时均已释放。
- 节流：每路径/每进程一次（`UNREKEYABLE_REPORTED`，`LazyLock<Mutex<HashSet<PathBuf>>>`，
  插入成功才写、集合锁在写之前释放）。依据 #1429 先例「per-acquire stderr write
  无上限，故每 assembly 只报一次」：`append` 每事件走一次 `mutate`→读，逐读一行
  即每回合几十行、每次都持文件锁，满管道会把本进程后续 registry 读与其它进程的
  `accept`（`FILE_LOCK_TIMEOUT` 30s）一并楔住（MM src2 concurrency#1 交错序列）。
- 带语义不变：哪条记录被跳过、哪条让 load 失败仍按 R15 差异④登记分工；计数器
  （`unrekeyable_skips()`，每读递增）不变，仍是「每次读」的观测半面。

## 范围

只覆盖三处登记数字/措辞与树内现值的对齐：R15:132 的 native-config `70/60`、
R19:145-146 的「7 模块前缀表 / 70 bound tests」、R19:119 的 blankkey 15/14/15、
R19:83-86 的「每读一行」信号面措辞。不覆盖、不改动：R15 四族差异的语义与批准
状态；native-config 131 行名册本身的内容（merge-main-r3 起逐轮累积，夹具 README
为正本）；带内跳过记录在 persist 重写时被丢弃这一行为（结构性修法仍待裁决）；
native-blankkey 之外的 `--check` 输出（turn-read 27 / turn-publish 19 / turn-submit
57 与 R19 登记一致，未漂移）。不放宽判分、不新增判分豁免。

## 机器判据

- 静态核（本树实测）：native-config `--check` → `#1457 131 mutation anchors OK;
  #2065 85 literal anchors OK; 131 bound tests declared`（exit 0）；
  native-blankkey `--check` → `#2692 16 mutation anchors OK; 15 literal anchors OK;
  16 bound tests declared`（exit 0）；
- 名册等值：blankkey `EXPECTED_NAMES` 16 行（第 16 行
  `registry-skip-writer-silent`），字面锚 15 项（新增
  `(run_registry_tests.rs, '#2692 the skip's stderr signal never reached the pipe')`），
  `ROW_PINS` 补钉该行（其断言唯一，共享断言规则够不着它）；
- 行为钉：`storage::run_registry_tests::unrekeyable_skip_logs_the_stderr_signal_once_per_path`（探针断言行在场）与既有
`unrekeyable_skips_advances_in_production_on_every_read`（计数器每读递增）并立，
  两半各有杀点（`registry-skip-writer-silent` / `registry-skip-silent`）；
- 账本闸门：`check_frozen.py` 规则要求本件 frontmatter `original` 写基础 ID
  （已写 `-001`）；全量杀灭轮（需 cargo 与净树）由协调者提交后在宿主跑，杀灭
  判据以 `EXPECTED_NAMES` 16 行为分母。

## 批准记录

未批准，非已接受差异，待人复核（可否决）。2026-11-15，#2960 fix-merge-r4 worker AI
代登记。本件是登记数字与信号面措辞更正（native-config 账本与 merge-main-r3 后的
名册脱节、blankkey 写侧行并入、R19 信号面措辞随锁序修法更新），不改变任何差异族的
跳过/失败分工语义、判分规则或豁免；R15/R19 原字节不动，账本只追加。人复核否决则循
账本规则出 `-R21` 更正。
