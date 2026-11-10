---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R13
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

更正件（R 链，承接 `A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R12`；frontmatter
`original` 按账本机器规则指基础 ID，与 R9-R12 同法，承接关系由文件名与正文表达）。
R13 登记 #2703 实现单（批次3 `#1868`/`#1651`/`#2703` 随行，基线 `d15ab7719`）
——R12 修复轮四 P2 开的「thinking/style/seed 空键无正/负控」跟进单落地：三法
（`set_session_thinking` / `set_output_style` / `seed_read_state`）各补空串键正控
（无回合 + `sessionKey:""` → `INVALID_SESSION_KEY`「sessionKey is required.」
逐字——三法 TS 侧门序已由 R12 修复轮三经源码核为 sessionKey 门先于 busy 门）与
require 弱化负控（`require_session_key(params)?` → `.unwrap_or_default()` 形状，
锚随 bound 测试 `expect_err` 的消息；thinking/style 两行弱化后对空键答 Ok、死于
expect_err，seed 行弱化后答 `file_not_found`、死于同串的断码标签——锚串在
expect_err 消息与断码标签两处同字，两种死法都进 panic 载荷）。产品行为零变化
（差异 ①② 原样）；本件为测试装置收口登记，不新增判分豁免。

同批随行、一并记入机器判据的同表变化：#1868 把两条删锁变异行（config 侧 /
rename 侧）的锚从 `#1457 transcript sequences strictly increase` 换为
「#1868 the concurrent model set|rename waits at the held transcript fence」——
fix-r3 冷写租约下，删锁经请求路径的确定性可观测是并发请求被答
`SESSION_WRITER_BUSY` 而非序列重复（cb8 实跑 10/10 全 busy；issue 字面的
「A 读 N → B 读 N → A 写 N+1 → B 写 N+1」交错在租约互斥下经请求路径不可构造），
bound 测试 `concurrent_transcript_writers_keep_the_chain_loadable` 由 8 轮
`join!` 自旋改为测试自身持 fence+lease 的受控同步点；#1651 增四行（seed 门序
三联 + `PERMISSION_MODES` 放宽）并核 `gateway.rs` 的 `permissions.defaultMode`
三值白名单改走共享 `is_permission_mode`（原字面量三元组与常量逐值等，行为零
变化）。

## 基线依据

沿用 R12 的两段基线：固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`
（`resources/PilotDeck`，只读）的别名/冷路径探针（`_tmp/20261026-issue-1872/
ts-probe.out`）与空键族探针（`_tmp/20261109-1872-harvest/ts-blankkey-probe.out`，
cb8 · Node v22.22.1 / ICU 78.2）。#2703 三法的相关行为格已在 R12 修复轮三经
TS 源码核对登记：thinking/style/seed 的 sessionKey 门先于 busy 门
（InProcessGateway.ts:2364-2366 / :2381-2383 / :2423-2429），无空键回合时 `""`
请求落各回调的 sessionKey 门答 `INVALID_SESSION_KEY`
"sessionKey is required."（空键族 probe :G/:H 同形）。本件零新探针。

## Rust 行为

零变化。差异 ①（别名键 busy 格）与差异 ②（status 冷路径答案格）的原登记见
R12「Rust 行为」段（sha256 锚随彼件），本批不触碰两差异的任何答案格。#1651 的
`gateway.rs` 改动是等值重构：`permissions.defaultMode` 的接受集从字面量三元组
（`"default"` / `"plan"` / `"bypassPermissions"`）改走共享
`runtime::permission::is_permission_mode`（即 `PERMISSION_MODES` 成员检查，
`null` 仍表示未设置），接受集逐值相等——一个常量两处消费，不再有第二份手抄
三值表。

## 范围

R12 登记的差异 ①② 及其覆盖/不覆盖清单继续有效（见 R12「范围」段）。本件新增
覆盖：三法空键格的正控与负控（#2703）；`PERMISSION_MODES` 常量的字面钉（放宽/
收窄都抓——收窄另有既有行 `mode-whitelist-narrowed`）；`seed_read_state` 门序
的成对钉（#1651：① 缺 sessionKey + 缺 path → `INVALID_SESSION_KEY`、② 活跃
turn + 缺 path → `INVALID_FILE_PATH`、③ 活跃 turn + 负 mtime + 有效 path →
`INVALID_FILE_MTIME`——③ 是 issue 文字绑给 ①/② 之外的必要格：① 在任何
busy-before-* 顺序下都死于 sessionKey 门、② 在 busy-before-mtime 顺序下仍死于
path 门，busy-before-mtime 只有 ③ 能杀）；空串 `mode`（`""` / 纯空白）的逐字
拒绝回归钉（无负控行）。不放宽判分。

## 机器判据

（本段为本件登记轮的增量；R12 的 69 行负控与用例清单见 R12「机器判据」段，
原字节不动、继续有效。行号为 worker 树 `issue-1868` 交付基线上的行号。）

- #2703 空键三联正控（`config.rs`）：
  `set_session_thinking_blank_key_answers_the_session_key_gate`（`:1716`）、
  `set_output_style_blank_key_answers_the_session_key_gate`（`:1741`）、
  `seed_read_state_blank_key_answers_the_session_key_gate`（`:1767`）——无
  回合 + `sessionKey:""`（seed 带有效 path+mtime、thinking/style 带合法载荷），
  断 `INVALID_SESSION_KEY` + 「sessionKey is required.」逐字。
- #2703 空键三联负控（`negative_controls.py`）：
  `thinking-blank-key-require-weakened`（`:528`）/ `style-blank-key-require-
  weakened`（`:545`）/ `seed-blank-key-require-weakened`（`:562`），锚
  `#2703 the blank key is a sessionKey error for thinking|output style|seed
  read state`，变异为各法 `require_session_key` 弱化为空默认（上下文行随法
  取唯一：thinking/style 带各自的 busy 块、seed 带 path 门行）。
- #1651 四行：`seed-path-before-session-key`（`:177`）/
  `seed-busy-before-path`（`:186`）`seed-busy-before-mtime`（`:201`）绑
  `seed_read_state_answers_gates_in_order`（`config.rs:3319`，用例 ①②③ 见
  范围段）；`permission-modes-widened`（`:152`，变异 `permission.rs` 常量
  `[&str; 3]` → `[&str; 4]` 含 `"debug"`，绑 `permission_modes_are_pinned_
  verbatim`（`config.rs:2436`）——钉字面量以 `as_slice()` 比较，数组直比会在
  放宽时编译失败、判 INVALID 而非 KILLED）。另有两枚纯回归钉无负控行：
  `set_permission_mode_blank_mode_is_rejected_verbatim`（`config.rs:2453`，
  `mode:""` / `" "` / `"\t"` 逐字拒绝）、`permission_modes_are_pinned_verbatim`
  自身即上行的 bound 测试。
- #1868 两行换锚：`transcript-fence-removed-config`（`:230`）与
  `transcript-fence-removed-rename`（`:235`）变异文本不变，锚换为
  `#1868 the concurrent model set waits at the held transcript fence` /
  `#1868 the concurrent rename waits at the held transcript fence`；bound 测试
  同名重写（`config.rs:3515`）——测试按生产同序自持该会话转录的 fence
  （`transcript_locks::acquire`）与冷写租约（`cold_write_lease`，即「在飞的
  另一个写者」），30s 窗口（同 close thread gates 的挂起界）内 intact 请求
  停于 fence（超时 → drop 租约与 fence → 完成且必须 Ok），删锁请求毫秒级以
  `SESSION_WRITER_BUSY` 在窗口内落定（判杀，证据含 busy 原文）；收尾转录级
  不变量（两条 entry、序列严格递增、fresh `restore()`）保留，`#1457 transcript
  sequences strictly increase` 前缀钉由收尾断言继续承载（该 pin 保留）。
- 静态核 `python3 -I negative_controls.py --check` → `#1457 77 mutation
  anchors OK; #2065 68 literal anchors OK`（R12 记 69/59（彼时 head），rebase
  并入 main 侧行后本批起点 70/60；+4 行（#1651）+3 行（#2703）= 77，锚
  60 +1（#1868 −1 +2）+3（#1651 门序）+1（`PERMISSION_MODES`）+3（#2703）
  = 68）。
- 样本实跑（cb8 本机即宿主）：九行（#1868 两行换锚 + #1651 四行 + #2703
  三行）经执行器正控 → 编译门 → 变异 → 恢复后正控全 KILLED。证据分两目录
  （首轮 `_tmp/20261110-issue-1868/negctl-sample/` 跑到四行全绿 +
  config 行负控 KILLED 后被宿主后台时限杀停——守卫已回卷、树净，只丢该轮
  汇总；续跑 `negctl-sample2/` 覆盖其余五行并出全量
  `summary.json`）：每行 `*-positive.log` / `*-compile.log` /
  `*-negative.log` / `*-restored.log` 逐相原始输出俱在（首轮 config 行
  独缺 restored 相，由续跑同行补齐：正控 ok 60.05s → 负控 0.07s FAILED
  于 model set 锚 → 恢复后正控 ok 60.05s）。执行器需 `src/` 逐字节等于
  HEAD，样本跑于本批的临时 WIP 提交树、跑毕即复位，交付以 final.patch
  相对 `d15ab7719` 复原。绿跑：
  `cargo test -p pilotdeck-gateway --lib -- native_process::` → `387 passed;
  0 failed`、`-- runtime::session::` → `342 passed; 0 failed`；fmt --check 与
  clippy --all-targets -- -D warnings 无输出。

## 批准记录

未批准，非已接受差异，待人复核（可否决）。2026-11-10，#2703 实现单落地后
AI 代登记（批次3 `#1868`/`#1651`/`#2703`）。本件不新增答案格差异：#2703 与
#1651/#1868 的全部变化均为测试装置与等值重构（`gateway.rs` 三值白名单改走
共享常量，接受集逐值相等）。人复核否决则循账本规则出 `-R14` 更正。
