---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R17
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

更正件（R 链，承接 `A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R16`；frontmatter
`original` 按账本机器规则指基础 ID，与 R9-R16 同法，承接关系由文件名与正文表达）。
三件事，均不放宽既有登记的判分：其一，更正 R15 差异④折叠带的**判据口径**——原实现按
`container_depth`（最深叶子节点深度）计带，而 V8 `Array.prototype.join` 的递归阈值按
**被 join 的数组个数**（与 `to_string_units` 的 frames 同口径）计；最内层为空数组时
前者比后者少计 1，带外一层的记录被静默跳过而不是像 TS 那样让 load 失败（fix-r5 评审
boundary#1）。其二，给差异④补**生产信号面**：带内跳过原为静默（唯一计数器
`#[cfg(test)]`），下次 persist 重写文件即丢数据而生产零信号（评审 boundary#3，按
CLAUDE.md 分级升 P0）。其三，登记差异①连带的新**答案形状**格：不可拼深键在 read 面的
回显经迭代无上限序列化器完整作答，而 TS 侧返回路径的 `structuredClone` 在该深度抛错
（评审 boundary#2）。fix-r6（评审 r6b）更正本件两处：带边界常量按**真实加载点**重测
（前值 2621 是探针 harness 浅栈产物，实为 2560/2561），「答案形状」段的 clone 边界
措辞按补测改写（不再主张未探的「≤2000 两侧都成」）。R15/R16 原字节不动，账本只追加。

## 基线依据

Node v22.22.0（`process.versions` 记录于探针输出首行：`node v22.22.0
"12.4.254.21-node.33" icu 78.2`），本 worker 树实测：

- fix-r5 `_tmp/20261026-issue-2692/fix-r5/node-probe/`（HEAD `eeec6ec63`）：
  `fold-band-stackdep.out` 固定调用栈前缀（0/10/50 帧）下二分搜索 `String()` 折叠
  边界，标量叶与空内层数组两种形状**逐对相同**（2621/2621、2618/2618、2604/2604）
  → V8 阈值不随叶子形状平移，`container_depth` 在空叶形状下恰好少计 1，评审#1 成立；
  `fold-band.out`（其调用形态下）`structuredClone` 自 2100 层起抛。
- fix-r6 `_tmp/20261026-issue-2692/fix-r6/node-probe/`（HEAD `d533ff9e8`，评审 r6b
  boundary#1 的结构性修法）：`fold-band-loadpoint.out` 以基线 dist 的真
  `FileRunRegistry`（constructor → initialize → withFileLock → refreshFromDisk →
  key 模板，生产调用点，非裸函数、非 atDepth 包装）实测 1999/2000/2100 全成、
  2615/2618/2621/2622/2630 全抛（两叶形逐对相同）；`fold-band-bisect.out` 同形态
  二分得**精确边界 2560 折叠、2561 抛**。→ fix-r5 的 2621 是更浅调用栈（裸 `String()`
  探针）下的值，真实加载点边界更低；「TS 能加载的形状不得转 load 失败」与「越带
  load 失败」两臂在 2560/2561 处分工。
- fix-r6 `clone-band.out`：直测 `structuredClone` 1999/2000/2100 全成（两叶形）、
  2621 起全抛，`JSON.stringify` 3000 层仍成、模板拼法与 clone 同界 → fix-r5
  「clone 自 2100 起抛」是其探针调用形态（更深栈）下的值，直测下边界在
  (2100, 2621) 未探、随调用栈前缀漂移。

## Rust 行为

- 折叠带判据（`storage/run_registry.rs`）：`container_depth` 更名重写为
  `array_nesting`——只计数组嵌套层数（每个被 join 的数组一层，对象为叶子，
  `[object Object]` 不递归），与 `to_string_units` frames 同口径；`TS_KEY_FOLD_DEPTH`
  fix-r6 按真实加载点实测改 2560（原 2621 为浅栈探针产物）。效果：2561 层起
  （空内叶与标量叶同界）记录让 load 失败（`Maximum call stack size exceeded`），
  与 TS 的 `get` 失败对齐；2001..2560 带内两形状仍走既有「跳过单条、其余可读」
  登记形状。
- 生产信号面（差异④）：`read_snapshot` 跳过臂加进程级累计计数
  `UNREKEYABLE_SKIPPED`（`AtomicUsize`，每跳过一条每读 +1），文件末尾
  `pub fn unrekeyable_skips()`（`storage/mod.rs` re-export）供生产读取；每次
  实际发生跳过的读向 stderr 发一行
  `run registry <path>: skipped N un-rekeyable record(s); the next persist rewrites the file without them`
  （计数经既有 per-snapshot `unrekeyable` 累加器驱动，紧跟记录循环之后、events 解析
  之前）。**带语义不变**：跳过 vs TS load 失败的分工仍按 R15 登记待批，本件只补可
  观测性，不改变哪条记录被跳过、哪条让 load 失败。
- 答案形状（差异①连带格）：read 面回显响应经 `pilotdeck_protocol::stringify_json`
  （迭代、无深度上限；`runtime/connection/mod.rs` 的 `send()` 与 `api_web.rs` 即全部
  线上响应面）序列化，不走 `js_value::json_stringify`（2000 层保守上限，仅服务
  provider bodies/tools/MCP，不在响应面）。3000 层深键的 miss 回显在 Rust 侧完整
  成答；TS 侧返回路径经 `structuredClone`——clone 直测 1999/2000/2100 全成（两叶
  形）、2621 起全抛，(2100, 2621) 之间未探且随调用栈前缀漂移（fix-r5 探针形态下
  2100 起抛）；机制按 ts-prodkey-probe cell e 推断，如实注明。已实测的分歧带为
  ≥2621 层的回显（clone 抛、迭代序列化器成）；≤2100 直测两侧都成。

## 范围

只覆盖评审 fix-r5 五条（boundary#1/#2/#3、sibling#3、P3#4/#5）与 fix-r6 六条
（boundary#1-5、sibling#1-2）对应的判据口径更正、信号面、答案形状登记与负控装置
维护。不覆盖、不改动：R15 差异④「跳过 vs load 失败」的分工语义与批准状态（仍是
登记待批，人复核可否决）；带内跳过记录在 persist 重写时被丢弃这一行为本身（信号面
只让丢弃可见，改丢不丢属结构性修法，待裁决）；`replacement.rs` pending/reservations
字符串域折叠（sibling#1，已由 A7.8-INTERACTION-NONSTRING-OWNER-001-R1 登记，结构性
修法挂 #1723）；`assembly/steer.rs` 生产者事件键（sibling#2，生产不可达：
AsyncHookRegistry 唯一生产入口的 sessionKey 先过 `require_text`）；fix-r6 评审的
P3 各条（sibling#3-8、boundary#6-8，含 `MAX_ARRAY_DEPTH < TS_KEY_FOLD_DEPTH` 的
编译期断言——现值 2000 < 2560 成立）。不放宽判分、不新增判分豁免；两个序列化器
不合并（回显面继续用迭代器，provider 面继续用保守上限）。

## 机器判据

- 静态核（fix-r6 本树实测，HEAD `d533ff9e8`）：
  `python3 -I src/pilotdeck-gateway/tests/fixtures/native-blankkey-submit/negative_controls.py --check`
  → `#2692 15 mutation anchors OK; 14 literal anchors OK; 15 bound tests declared`
  （exit 0）。名册 13→15 行（fix-r6 新增 `registry-band-leaf-depth`：带判据变异回
  旧叶深口径、空内叶带外第一层被跳过、绑 reopen 测试的「past V8's fold band and
  must fail the load」；`echo-capped-serializer`：`MAX_ARRAY_DEPTH` 抬到 5000 使
  两口径坍缩、绑深键回显测试的「the capped provider-body serializer refuses the
  same value」）；字面锚 13→14（新增 read_tests.rs 的两口径 pin；band-leaf-depth
  行复用既有 pin）；`--check` 另核 15 个绑定测试名仍在声明文件里
  （`bind_module_file` + `declares_test`，评审 boundary#5）。turn-read / turn-publish
  的 `--check` 补 `before != after` 拒绝与 `declares_test`（含 `scenario!` 宏形态），
  输出 `27 mutation anchors OK; 27 bound tests declared` / `19 + 19`。
- 行为钉（三条测试）：
  `storage::run_registry_tests::deep_session_key_record_reopens_without_bricking_the_other_records`
  补空叶形状（fix-r6：2001/2560 跳过、2561 load 失败，两形状共用真实加载点边界，
  空内叶 2561 是叶深口径的判别腿）；
  `storage::run_registry_tests::unrekeyable_skips_advances_in_production_on_every_read`
  （累计计数器每读递增）；
  `runtime::turn::read::tests::deep_session_key_echo_survives_the_response_serializer`
  （3000 层回显完整序列化 + 有上限序列化器拒绝同一值，钉两口径分工）。
  fix-r6 两条新变异（`registry-band-leaf-depth` / `echo-capped-serializer`）本树
  已逐条「应用 → `cargo test --no-run` → 点名测试变红于绑定断言 → 恢复」实测；
  全量杀灭轮（需 cargo 与净树）由协调者在宿主跑，杀灭判据以 `EXPECTED_NAMES`
  15 行为分母。
- CI 接线（评审 sibling#3）：`negative-controls` job 的「绑定漂移自检」步补四行
  `--check`——native-config、turn-submit（已有 `--check`，先前未接线）与 turn-read、
  turn-publish（本轮新增只读 `--check`：锚点唯一性 + 名册无重复，零 cargo，同
  #1433/#2391/#2495 先例；fix-r6 起另核 no-op 变异与绑定测试名）。
- 账本闸门：`check_frozen.py` 的 `check_diffs` 规则要求本件 frontmatter `original`
  写基础 ID（已写 `-001`）；R15/R16 原字节不动。

## 批准记录

未批准，非已接受差异，待人复核（可否决）。2026-11-12，#2692 fix-r5 worker AI
代登记。本件更正 R15 差异④的带判据口径（叶子深度 → join 帧数口径，空叶形状差 1 已
实测钉死）、为该差异补生产信号面（计数器 + stderr 行，带语义与批准状态不变）、并新
登记差异①连带的深键回显答案形状格（TS 侧机制按 cell e 推断、确切带未探，如实注明）。
同日 fix-r6 worker 更正两处：带边界常量按真实加载点（constructor → initialize →
withFileLock → refreshFromDisk → key 模板）实测从 2621 改 2560（浅栈探针产物），
「答案形状」段 clone 边界措辞按 1999/2000/2100 补测改写（不再主张未探的
「≤2000 两侧都成」）。人复核否决则循账本规则出 `-R18` 更正。
