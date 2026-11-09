---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R12
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

更正件（R 链，承接 `A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R11`；frontmatter
`original` 按账本机器规则指基础 ID，与 R9-R11 同法，承接关系由文件名与正文表达）。
登记 #1872 / #1873 / #1874 实现单（分支 `claude/issue-1872-family`，基线
`a1719f483`，收货压为单提交 `44bf56991`——worker 原四提交 `af24560dd` +
`04784e6cd` + `db7cf305a` + `a6e9f3392` 经 squash+rebase 后不可达，树内容
逐字节等价以 final.patch 复验）落地后 config 族的两个答案格变化：

修复轮 `e9ad34549`（收货双谱系评审 DeepSeek + MiniMax 的 P2 处置）：差异 ① 补
空串键卫（见 Rust 行为）、clear 窗口/回退支路/关闭目标/双生产者字面钉的用例
与负控、`rename`/`model_set` 冷路径 broken-lock 正例；两处残余窗口登记开单
#2686（探测错误静默跳过 restore，基线既有形状）、#2687（直关分支不复查
`has_active_turn` 与 busy 后重试追加第二条），均前置于 #1457 收尾。本件为该
PR 内新增文件、未合入前随分支修订，不走 `-R13`。

- 差异 ①（别名键 busy 格）：`session_model_set` / `session_model_clear` 以别名
  拼写请求（`a-b`，owner 注册为 `a/b`——`sanitize_session_id` 把 `/` 与 `\` 折成
  `-`，一个转录文件两种拼写）且 owner 有活跃回合时，TS 成功（选择落盘、`a/b`
  会话与回合原样），Rust 答 `SESSION_BUSY`。这是对 Rust-only 回归（修前 Rust
  同样成功，但 `owner.close(owner_key)` 把 owner 的活跃回合打断——TS 无此路径，
  SessionRouter 按精确键索引，别名请求根本看不见 owner）的修法选择：收紧为
  busy，而非复刻 TS 的静默成功。
- 差异 ②（status 冷路径答案格）：转录 writer 租约被会话持有（factory 的
  restore→publish 水合窗口 / 关闭排水）时，Rust 的
  `session_record_status` 答可重试的 `SESSION_WRITER_BUSY`；存储故障（如打不开
  的 `<transcript>.lock`）以错误传播。TS 无此状态可比（router 层不 live 且无
  options 回调时 `{"recorded":false}`；有回调时照写并 `recorded:true`），且
  TS 的 `recorded:false` 在 native 进程恒不成立——冷路恒有 provider。修前 Rust
  把这两类都折成 `recorded:false` / 吞掉，#1873 分离。

## 基线依据

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`（`resources/PilotDeck`，
只读），Node v22.22.0 / ICU 78.2，原始输出
`_tmp/20261026-issue-1872/ts-probe.out`（探针 `ts-probe.mjs` 同目录；InProcessGateway
直调，SessionRouter 真实例 + 注入 port 回调记录）。关键行：

- `hasActiveTurn(a-b)` `false` 而 `hasActiveTurn(a/b)` `true`（:9）——别名键在
  TS 按精确键索引，看不见 owner 的回合；
- 别名 set 返回 `{projectKey, sessionKey:"a-b", saved:{provider:"prov",
  model:"model-x", mode:"model"}, effective:{…source:"session"}}`（:13），port
  write 以请求键 `a-b` 落盘（:14），`hasSession(a/b)` / `hasActiveTurn(a/b)` 事后
  仍 `true`（:15-16）——成功且 owner 会话与回合原样；别名 clear 同形（:18-21）；
- 精确键有回合时 TS 也答 `SESSION_BUSY` "Cannot change the model during an
  active turn."（:11）——Rust 修后的同名报文与此一致，仅作用域扩到 owner 拼写；
- `router.recordAgentStatusMessage(a-b)`（owner 不 live）→
  `{"owner":"not_live","recorded":false}`（:23）；InProcessGateway not-live 无
  options 回调 → `{"recorded":false}`，live → `{"recorded":true}`（:26-27）——
  TS 的 `recorded:false` 保留给「不 live 且无冷 provider」。

## Rust 行为

差异 ①（#1872）：两个 wrapper
（`config.rs:189-228` set / `:296-333` clear，修复轮 `e9ad34549` 起 sha256
`41c70fb975854923d13bd9a12d74dd8fd21f81616a38a83d373f7e498de6de22`）先以
`live_transcript_owner`（`:233`）解析别名折叠到的 owner 注册键，请求键与 owner
键同时过 `has_active_turn` 门（`:255-263` / `:342-348`）——owner 有回合即
`SESSION_BUSY`，报文与 TS 精确键格逐字相同；两键同时进 replacement 协调器的
transcript-write 预约表（check→append→close 窗口不纳新回合，`:205-227`）。
**空串键例外**（修复轮 `e9ad34549` 加卫）：请求键 trim 后为空（`""` 折叠到
`session` 转录）时整个 busy 门跳过、落回 port write 的 sessionKey 门答
`INVALID_SESSION_KEY`——TS 的 `hasActiveTurn` 按精确键对空串必 false，同形。
成功路径的追加固有语义不变：过 owner 的 recorder 追加后关闭 *owner* 拼写
（`close_model_selection_owner`，`:373-397`，#1457 fix-r3 的关闭目标在 #1872 的
别名门下由正控钉住；其回退支路——owner 为 append 期间才发布——修复轮起有用例
与负控，见机器判据）。非别名路径（请求键 = owner 键）行为与修前及 TS 完全
一致。**登记的残余窗口**（修法开单，不在本件）：直关分支
（`target == session_key || reserved_owner == Some(target)`）不复查
`has_active_turn`——已过 preflight、尚未 `begin_turn` 的回合仍会被 close 中止
（#2687）；回退支路报 busy 时条目已落盘，重试会追加第二条 model-selection 条目
（代码注释与本件双登记，#2687 同单跟进）。

差异 ②（#1873；#1874 收拢）：
`session_record_status`（`session_dir.rs:448-517`，squash 时 sha256
`76bd7eab133b6dc6d9849b597e4dde7b778fe834bb2473f92117781d831333b7`；rebase
并入 main 侧 #2564 尾部用例后现行
`798e59ae84ba9e037c1437ccec85d460669c797acac90eaf33d4443a4201871f`，增量
不在 `:448-517` 段）冷路径经共享
三段协议 `transcript_locks::append_metadata_with_lease`
（`transcript_locks.rs:131-174`，sha256
`c5a2d56b91dffbbeb3da566f97b725c4452beb2478e6de3d30e1ef9b4fe4c05c`）：live
checked 追加 → 冷路取 `try_writer_lease` → 忙租约回查 live 否则
`SESSION_WRITER_BUSY`（"Session writer is still draining; retry session
recreation."，与 session factory 同码）；`try_writer_lease`（`factory.rs:251`，
sha256 `91ecb509186158289774e6098eb75a6bf0f88330ba2b0e2c86df12d7257b5b4c`）自
`Result<Option<WriterLease>, RuntimeError>`——只有互斥争用 / flock `WouldBlock`
是 `Ok(None)`，规范路径失败、`.lock` 打不开等存储故障是 `Err` 原样传播。成功
路径恒 `recorded:true`（TS 的 `recorded:false` 格在 native 进程不可达：冷路恒有
provider，此点以 InProcessGateway 原型直调探针佐证，ts-probe.out:26-27）。
第二段查表（忙租约回查）的闭仓 writer 错误同样传播（live 闭包同源，两段共用）。
存储故障格作用于**全部三个调用点**（status / rename·tag / model set·clear 冷路）：
修前 `cold_write_lease → Option` 把锁文件打不开折成 `None` → 三个调用点都答
`SESSION_WRITER_BUSY`；#1873 的 Result 化经共享 `append_metadata_with_lease`
让三处都改为以存储错误失败（R12 补记，收货修复轮 e9ad34549 为 rename 与
model_set 补了 broken-lock 正例，见机器判据）。

## 范围

覆盖：差异 ① 的别名 busy 格（set/clear 两法）与差异 ② 的
`session_record_status` 冷路径答案格（busy / 存储故障 / recorded 恒真），及其
机器判据（下节）。不覆盖：非别名路径（与 TS 同形，`#1457` R3 已登记的
set/clear 全链，本件零变化）；TS 别名成功格的 Rust 侧对应（Rust 在 owner 有
回合时恒 busy，owner 无回合时别名成功且关闭 owner——与 TS「成功且不关」在
关闭目标上仍有差别，属 #1457 fix-r3 已登记的关闭语义，非本件新差异）；
`rename`/`tag` 的别名格（#1457 fix-r2/r3 已登记；但其**存储故障格**随差异 ②
的 `try_writer_lease` Result 化一并改变，见上文补记——三调用点共用一条共享
协议，机器判据见下节）；`SESSION_WRITER_BUSY` 的会话 factory 侧用法（#1860
已登记）；#1874 的三调用点收拢（收拢本身纯内部重构、答案零变化；**例外**即
上述存储故障格，系 #1873 的分离而非收拢所致，测试与负控迁移仅随锚）。同族
四法（permission/thinking/style/seed）**不做** owner 侧 busy 门：TS SessionRouter
按精确键索引、四法同样看不见 owner 的回合，且它们 close 的是请求拼写
（`config.rs` 各法自带），不会打断 owner 回合——与 TS 同形，保持现状非漏改。
`services.rs::record_failure`（turn 路径）仍用未检式 `record_agent_status_message`：
非请求路径，#1874 范围未覆盖，按现状豁免（TS 侧 writer 关闭语义未实测，收口
另单）。不放宽判分。

## 机器判据

以下现核于修复轮 `e9ad34549`（行号为该提交上的行号；`44bf56991` 上的旧行号见
Git 历史）：

- 差异 ① 用例：`session_model_set_rejects_active_turn_on_alias_owner`
  （`config.rs:1381`）与 `session_model_clear_rejects_active_turn_on_alias_owner`
  （`:1457`）——recorder 注册 `al-set/session` / `al-clear/session`、
  `begin_turn` 在 owner 拼写、请求 `al-set-session` / `al-clear-session`，断
  `SESSION_BUSY` + 逐字报文；空串键用例
  `session_model_set_blank_key_answers_the_session_key_gate`（`:1418`，修复轮
  新增）——recorder 注册 `session` 且其上有回合，请求空串键断
  `INVALID_SESSION_KEY` "sessionKey is required."（busy 门不先答）；窗口用例
  `session_model_set_holds_the_owner_reservation_through_the_write`
  （`:1491`）与其 clear 孪生
  `session_model_clear_holds_the_owner_reservation_through_the_write`
  （`:1566`，修复轮新增）——请求停在转录 fence 上时预约表里两拼写并在、事后
  双释放；正控 `model_set_closes_the_owner_spelling_of_an_alias_request`
  （`:1746`）——别名请求成功、条目落盘、`current_handle("g7/session")` 事后
  `None`（关闭目标是 owner 拼写）、reload `restore()` 通过；回退支路双用例
  `close_model_selection_owner_reports_busy_for_an_owner_seen_by_the_append`
  （`:1826`，修复轮新增——append 期间才见 owner 且有回合：断 `SESSION_BUSY`、
  回合未被 abort、预约已释放）与 `close_model_selection_owner_closes_an_
  owner_seen_by_the_append`（`:1884`，修复轮新增——无回合则关闭该 owner 拼写）。
- 差异 ② 用例：`status_cold_write_reports_busy_while_a_session_holds_the_lease`
  （`session_dir.rs:743`，断 `SESSION_WRITER_BUSY` 且盘上无写入）、
  `status_cold_write_surfaces_a_broken_lock_file_as_an_error`（`:800`，断码非
  busy 且报文含 "Could not open session ledger lock"）、
  `status_reports_a_closed_writer_as_an_error`（`:947`，闭仓 writer 是错误）；
  修复轮新增的另两个调用点对偶：`rename_cold_write_surfaces_a_broken_lock_
  file_as_an_error`（`session_dir.rs:851`）与
  `model_set_cold_write_surfaces_a_broken_lock_file_as_an_error`
  （`config.rs:1705`）——三调用点的存储故障格各有正例；`model_set` 的对应
  busy 格 `model_set_cold_write_reports_busy_while_a_session_holds_the_lease`
  （`config.rs:1629`）；lease 分层单测
  `try_writer_lease_separates_contention_from_storage_failure`
  （`factory_tests.rs:7735`）；双生产者字面钉
  `writer_busy_literals_stay_paired_across_producers`（`factory_tests.rs:7787`，
  修复轮新增——factory 侧 `writer_busy()` 与共享协议 `writer_busy_error()` 的
  code/message 两两相等）。
- 负控（`tests/fixtures/native-config/negative_controls.py`，行号为 `e9ad34549`
  上的行号）：差异 ① 的 `set-alias-owner-busy-removed`（`:287`）/
  `clear-alias-owner-busy-removed`（`:316`）（busy 门反改请求键单侧）/
  `set-busy-gate-blank-key-guard-removed`（`:304`，修复轮新增——空串键卫移除
  则空键先答 busy）/ `set-alias-owner-reservation-skipped`（`:327`）与其
  clear 孪生 `clear-alias-owner-reservation-skipped`（`:338`，修复轮新增）/
  `close-owner-fallback-busy-skipped`（`:354`，修复轮新增——回退支路复查
  移除则 close abort 在飞回合）/ `set-closes-owner-target-reverted`（`:368`，
  修复轮新增——关闭目标折回请求拼写）；差异 ② 的 `writer-lease-folded-to-busy`
  （`:268`，存储故障折回 busy）；三段协议的 `transcript-cold-lease-skipped`(-
  `-status`/`-config`)（`:224`/`:231`/`:238`，同锚不同绑定）与三条
  `*-checked-append-reverted`（`:249`/`:254`/`:259`）。静态核
  `python3 -I negative_controls.py --check` → `#1457 63 mutation anchors OK;
  #2065 53 literal anchors OK`（本件十三行——初登记九行 + 修复轮四行——加既有
  46/36；rebase 并入 main 侧 #2562/#2564 四行后同表为 63/53，`--check` 复核过）；
  新/迁行样本跑全 KILLED（改坏红；初登记九行的证据分布在
  `_tmp/20261026-issue-1872/negctl-s1-sample/`、`negctl-s1-sample2/`、
  `negctl-s1-sample3/`、`negctl-s2-sample/`、`negctl-s3-sample/`——s3 的九行
  含三行迁移的 `transcript-live-bypass-*`，新增九行分布在 s1/s2 各样本；修复轮
  四行的 KILLED 样本留待宿主全量负控，锚唯一性已经 `--check` 复核）。
- 绿跑（worker 树 HEAD `1f03a0b95`，工作树干净）：`cargo test -p
  pilotdeck-gateway --lib -- native_process::` → `347 passed; 0 failed`、
  `-- runtime::session::` → `340 passed; 0 failed`；fmt --check 与 clippy
  --all-targets -D warnings 无输出。收货复跑：rebase 后 head `85e9dc55a` 于
  cb8 独立 verify 树定向套件 `351 passed; 0 failed` / `340 passed; 0 failed`；
  修复轮 head `e9ad34549` 同树复跑 `357 passed; 0 failed`（+6：空串键、clear
  窗口、回退双用例、set/rename broken-lock 对偶）/ `341 passed; 0 failed`
  （+1：字面钉），fmt/clippy 绿、负控 63/53（原始输出
  `_tmp/20261109-1872-harvest/cb8-verify-1872.log` 与
  `cb8-verify-1872-r3.log`）。
- 账本闸门：`diffs/` 只追加，`check_frozen.py` 的 `check_diffs` 规则要求本件
  frontmatter `original` 写基础 ID（已写 `-001`，非 `-R11`）。

## 批准记录

未批准，非已接受差异，待人复核（可否决）。2026-11-09，#1872 / #1873 / #1874
实现单落地后 AI 代登记。差异 ① 是修 Rust-only 回归（别名请求打断 owner 活跃
回合）时的有意收紧：在「busy」与「复刻 TS 静默成功但需自行保证不打断」之间选
了前者（owner 侧预约 + 双键 busy 门可证；复刻成功则须在 check→append→close
窗口内保住 owner 回合不被打断，代价更高且 TS 侧该格本就不关闭 owner）。差异
② 是把两类非 busy 事实从 busy/成功答案里分离的纠错，`recorded:false` 的不可
达以 TS 侧无对应状态佐证。两差异均不新增判分豁免、不改判定规则；人复核否决
则循账本规则出 `-R13` 更正（如裁定差异 ① 应复刻 TS 成功格）。
