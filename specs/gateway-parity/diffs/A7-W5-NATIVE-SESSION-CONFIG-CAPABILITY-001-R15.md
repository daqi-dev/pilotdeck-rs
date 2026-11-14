---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R15
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

更正件（R 链，承接 `A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R12`；frontmatter
`original` 按账本机器规则指基础 ID，与 R9-R12 同法，承接关系由文件名与正文表达）。
登记 #2692（submit 侧 sessionKey null 折叠坍缩——R12 修复轮二开单、前置于 #1457
收尾）实现单（分支 `claude/issue-2692-blankkey-fold`，round-1 提交 `e2cb40859`，
fix-r1/fix-r2 以 patch 续跑：S1 探针 + S2/S3 实现 + S4 负控/账本，worker 超时后
S2/S3 与 S4 部分在场，本件随 fix-r2 补完登记）落地后的四族新差异。

R 号口径：R13 已见于并行车道 refs（`origin/claude/issue-2068-session-dir-projectkey`）；
R14 在本树全部 refs 无命中（应在未 fetch 的车道）。fix-r1 在场代码（js_value.rs /
config.rs ×2）三处已按 R15 措辞登记，本件取 R15 与之一致、不与任何已知 ref 碰撞；
R14 缺号如实记于此，若 R14 车道后到导致撞号按账本规则出更正件。

## 基线依据

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`（`resources/PilotDeck`，只读，
本轮 worker 树实测 HEAD 即该提交），Node v22.22.0 / ICU 78.2。探针输出
`ts-deepkey-probe.out`（S1，round-1）与 `ts-prodkey-probe.out`（fix-r1）：探针
原始文件随 fix-r1 轮次目录，在本 worker 树不可达（fix-r1 证据目录未随树带过来，
见 fix-r2 RESULT 异常节），关键格以在场测试注释与断言为准。测得形状：

- **cell b–d（ts-prodkey-probe）**：`sessionKey:null` 回合经完整 submit 链存活——
  记录体 JSON null、复合键拼 `"null"`、router Map 按原始 null 值索引；null 请求
  遇 null 回合答 `SESSION_BUSY`（cell F），`"null"` 字符串请求在 null 回合下
  **成功**（cell c——router Map miss + 非空串过门），两拼法各成独立在飞回合；
- **cell e（ts-prodkey-probe）**：`sessionKey` 为 ToString 抛错的深数组时，
  `String(deep)` 在 V8 折叠为 `""`，TS 的记录**先落**、只有返回路径的
  structuredClone 才抛 `Maximum call stack size exceeded`；busy 门对不可拼键
  落 sessionKey 门（`trim is not a function` 路径）；
- **cell a1（ts-prodkey-probe）**：真服务链（`resolve_model_selection` 即
  `session_model::execute`）上 null 键回合死于模型解析，storage 侧 sessionKey
  门拒绝，TS 量得 `gateway_submit_failed` 失败对（agent_status + error +
  turn-failure userHint）；
- **折叠带（probe 机实测，Node 22.22.0）**：2621 层嵌套折叠为 `""`、2622 层
  抛 `Maximum call stack size exceeded`（`run_registry.rs` 的 `TS_KEY_FOLD_DEPTH`
  口径，栈依赖）；TS `get` 对越带记录以同一 RangeError 拒绝（`#251` 边界
  oracle 口径）。

## Rust 行为

差异 ①（**router 域键的标签碰撞与结构键控**，`js_value.rs:272-301`）：#2692 的
修法把两个键域分开——复合 run 键 `template_key_checked`（模板拼法：null →
`"null"`、缺失 → `"undefined"`、不可拼的深数组当 `Err` 回答）与 router 形 Map
（admission / sessions / fence / coordinator / busy 门预检）的原始值键
`router_key`（字符串即自身，其余类别打 `\0\0raw:…` 标签）。该实现登记两条
与 TS SameValueZero 的残余分歧：**一个等于标签的字面串**（如字符串
`"\0\0raw:null"`）与 null 值的标签键碰撞成同一键，TS 按值型分离；**容器按
结构拼写键控**（拼写相同的两个对象同一键），TS 按对象引用分离。连带格：不可
拼键（深数组）在读取面按 miss 处理并回显原始请求值（`read.rs:376-404`——
TS `get` 的 RangeError 口径见差异 ④ 带外行）。

差异 ②（**深键 accept-catch 无记录**，`submit.rs` 接受臂 + `config.rs:2496-2575`）：
TS 的记录先落、返回路径 structuredClone 才抛（基线 cell e）；Rust 的键构造
不能索引它拼不出的东西，所以 durable acceptance 直接以 accept-catch 形状拒收
（单事件 `result_unknown` + `Maximum call stack size exceeded` + 接受期
userHint），**无记录落盘、注册表保持完整**。修复前（round-1 前）unchecked
折叠把记录落在 `""` 键下并砖化注册表（reopen 不可读）。

差异 ③（**真服务失败的记录终态**，`config.rs:2579-2710`）：真服务链上 null 键
回合死于模型解析时，Rust 把失败对（agent_status + `gateway_submit_failed`
error）**追加进记录事件流、记录保持 accepted 态**；TS 把记录**标记为 failed**。
失败对本身与 TS 逐字同形（cell a1），差异仅在记录终态。

差异 ④（**折叠带内跳过**，`run_registry.rs:215-221` / `:700-741` / `:783-789`）：
read_snapshot 对不可重拼（拼法 Err）的记录分带处理——带内（≤ `TS_KEY_FOLD_DEPTH`
=2621）**跳过该记录**：对每次读不可见，`Snapshot::unrekeyable` 计数器保观测，
**持久化变异重写文件时把跳过的记录一并丢掉**（TS 按折叠 `""` 拼法重拼落位、
记录可达）；带外（>2621）load 失败，与 TS `get` 的 RangeError（#251 边界
oracle）同判。修复前带内外一律跳过（越带也跳、load 不再失败）。与差异 ②
合看：Rust 对深键**两端都收口**（submit 拒收 + 带内跳过），TS 两端各留一格
（记录先落 / 折叠落位）。

## 范围

覆盖：#2692 修复的四个面——submit 键计算与接受（差异 ②/③ 的 submit 侧）、
config 族 busy 门（差异 ① 的门侧）、run registry 读写与折叠带（差异 ④）、
read 面回显（差异 ① 连带格）——及其机器判据（下节）。不覆盖：`turn-submit`
姊妹面的 #2692 重锚行（submit.rs 内 `router_key` 变量重命名的连带锚更新，
行为零变化）；thinking/style/seed/permission 四法的门序与空键行为（R12 已
登记，本件零变化）；`#251` 边界 oracle 本身（本件只按其口径引用）；差异 ①
的标签碰撞/结构键控**未修**——修标签碰撞需要值型键（跳出 JsString 键型），
修结构键控需要对象身份（JS 语义外），两者登记在案待人复核裁决。不放宽判分。

## 机器判据

以下现核于 fix-r2 工作树（行号为该树行号；round-1 `e2cb40859` 上的旧锚见
Git 历史）：

- 差异 ① 用例：`router_keys_separate_raw_categories_from_string_spellings`
  （`js_value.rs:556`——类别标签表）、`template_key_spells_the_template_literal_form`
  （`js_value.rs:522`——模板拼法表，深数组 Err 格）、
  `read_session_messages_reads_the_router_domain_and_echoes_the_raw_key`
  （`read_tests.rs:941`——读取面按 router 域索引 + miss 回显原始请求值 +
  absent/undefined 丢字段）；
- 差异 ② 用例：`deep_array_session_key_submit_is_rejected_without_landing_a_record`
  （`config.rs:2506`——单事件 accept-catch 拒收 + 断言 `record.is_none()`
  无记录落盘）；
- 差异 ③ 用例：`real_services_null_session_key_turn_dies_at_model_resolution`
  （`config.rs:2592`——真服务链失败对逐字断言 + 记录 accepted 态 + 失败事件
  追加）；
- 差异 ④ 用例：`deep_session_key_record_reopens_without_bricking_the_other_records`
  （`run_registry_tests.rs:968`——带内跳过 + 深度 2622 越带 load 失败 +
  `skipped_unrekeyable` 计数）、
  `null_session_key_body_keeps_json_null_while_the_key_spells_null`
  （`run_registry_tests.rs:883`——记录体 JSON null / 键拼 `"null"` / 磁盘
  往返）；
- 差异 ① 门格用例：`session_model_set_null_key_turn_from_the_submit_chain_
  answers_the_ts_cells`（`config.rs:1971`——null 键回合四格 E/F + permission
  孪生 + 记录体 + 交叉拼法双在飞）、blank 孪生（`config.rs:2213`）、
  `"null"` 字符串孪生（`config.rs:2339`）、Grok#2 格
  `session_model_set_null_request_never_answers_for_a_busy_owner_session`
  （`config.rs:2451`——null 请求永不为 busy owner 代答）；
- 负控：`tests/fixtures/native-blankkey-submit/negative_controls.py` 十一行
  （迁移 #2391 `negative_selection` 启动契约：名册等值核验 + `refuse` rc 2 +
  `claim_dir` + `verdict_exit` + #2495 fix-r2 的 `--check` argv 机器核）——
  `submit-owner-uses-template-key` / `router-key-folds-null`（键域分离两面）/
  `template-key-checked-folds` / `acceptance-error-swallowed`（差异 ②）/
  `owner-conjunction-only`（Grok#2）/ `model-set-gate-string-only` /
  `permission-gate-string-only`（差异 ① 门）/ `registry-body-spelling` /
  `submit-raw-spelling`（记录体两行，round-1 幸存重锚）/
  `registry-skip-reverted`（差异 ④）`echo-drops-raw-key`（差异 ① 连带）。
  静态核 `python3 -I …/negative_controls.py --check` → `#2692 11 mutation
  anchors OK; 8 literal anchors OK`。连带重锚：`turn-submit/negative.py` 五行、
  `turn-read/negative.py` 两行、`turn-publish/negative.py` 一行（S2/S3 变量
  重命名的锚对齐，行为零变化）；`native-config/negative_controls.py` 四行换锚
  到 router 域门形（`--check` 70/60）；`test_negative_optimize_guards.py`
  SCRIPTS 注册 blankkey 行；CI 负控 job 补 blankkey `--check`（#1433/#2391/
  #2495 先例）；`anchor_selfcheck.py --anchors-only` errors=0。全量杀灭轮
  （需 cargo 与净树）由协调者提交后在宿主跑。
- 账本闸门：`diffs/` 只追加，`check_frozen.py` 的 `check_diffs` 规则要求本件
  frontmatter `original` 写基础 ID（已写 `-001`，非 `-R12`）。

## 批准记录

未批准，非已接受差异，待人复核（可否决）。2026-11-11，#2692 fix-r1/fix-r2
worker AI 代登记。差异 ① 的两条（标签碰撞、结构键控）是修 Rust-only 回归
（submit 侧 null→`""` 折叠坍缩 TS 两拼法）时为保持 JsString 键型而接受的
残余：值型键/对象身份键的代价高于收益，登记在案。差异 ②③ 是「不能索引
不能拼写的东西」与「事件流即终态」两个实现前提的直接后果，TS 侧对应格
有探针实测。差异 ④ 带内跳过是「保持注册表完整可读」的保守选择，代价是
带内记录不可达且重写丢弃（计数器可观测）。四族均不新增判分豁免、不改
判定规则；人复核否决则循账本规则出 `-R16` 更正。
