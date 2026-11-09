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
（`config.rs:189-228` set / `:288-323` clear，现行 sha256
`6c342dc20f576a09b61991460b50f6583796a649d447a0ac0bb41edc67b01fa4`）先以
`live_transcript_owner`（`:233`）解析别名折叠到的 owner 注册键，请求键与 owner
键同时过 `has_active_turn` 门（`:248-256` / `:334-341`）——owner 有回合即
`SESSION_BUSY`，报文与 TS 精确键格逐字相同；两键同时进 replacement 协调器的
transcript-write 预约表（check→append→close 窗口不纳新回合，`:205-227`）。
成功路径的追加固有语义不变：过 owner 的 recorder 追加后关闭 *owner* 拼写
（`close_model_selection_owner`，`:365-390`，#1457 fix-r3 的关闭目标在 #1872 的
别名门下由正控钉住）。非别名路径（请求键 = owner 键）行为与修前及 TS 完全一致。

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

## 范围

覆盖：差异 ① 的别名 busy 格（set/clear 两法）与差异 ② 的
`session_record_status` 冷路径答案格（busy / 存储故障 / recorded 恒真），及其
机器判据（下节）。不覆盖：非别名路径（与 TS 同形，`#1457` R3 已登记的
set/clear 全链，本件零变化）；TS 别名成功格的 Rust 侧对应（Rust 在 owner 有
回合时恒 busy，owner 无回合时别名成功且关闭 owner——与 TS「成功且不关」在
关闭目标上仍有差别，属 #1457 fix-r3 已登记的关闭语义，非本件新差异）；
`rename`/`tag` 的别名格（#1457 fix-r2/r3 已登记，本件未动其答案）；
`SESSION_WRITER_BUSY` 的会话 factory 侧用法（#1860 已登记）；#1874 的三调用点
收拢（纯内部重构，答案零变化，测试与负控迁移仅随锚）。不放宽判分。

## 机器判据

以下现核于 `44bf56991`（行号为该提交上的行号，`.rs`/负控行号在 squash 前后不变）：

- 差异 ① 用例：`session_model_set_rejects_active_turn_on_alias_owner`
  （`config.rs:1373`）与 `session_model_clear_rejects_active_turn_on_alias_owner`
  （`:1404`）——recorder 注册 `al-set/session` / `al-clear/session`、
  `begin_turn` 在 owner 拼写、请求 `al-set-session` / `al-clear-session`，断
  `SESSION_BUSY` + 逐字报文；窗口用例
  `session_model_set_holds_the_owner_reservation_through_the_write`
  （`:1438`）——请求停在转录 fence 上时预约表里两拼写并在、事后双释放；正控
  `model_set_closes_the_owner_spelling_of_an_alias_request`（`:1589`）——别名
  请求成功、条目落盘、`current_handle("g7/session")` 事后 `None`（关闭目标是
  owner 拼写）、reload `restore()` 通过。
- 差异 ② 用例：`status_cold_write_reports_busy_while_a_session_holds_the_lease`
  （`session_dir.rs:743`，断 `SESSION_WRITER_BUSY` 且盘上无写入）、
  `status_cold_write_surfaces_a_broken_lock_file_as_an_error`（`:800`，断码非
  busy 且报文含 "Could not open session ledger lock"）、
  `status_reports_a_closed_writer_as_an_error`（`:897`，闭仓 writer 是错误）；
  `model_set` 的对应 busy 格 `model_set_cold_write_reports_busy_while_a_session_
  holds_the_lease`（`config.rs:1512`）；lease 分层单测
  `try_writer_lease_separates_contention_from_storage_failure`
  （`factory_tests.rs:7735`）。
- 负控（`tests/fixtures/native-config/negative_controls.py`，行号为 squash 时
  行号，rebase 并入 main 侧 #2562/#2564 行与 SERVICES 常量块后各 +6）：差异 ①
  的 `set-alias-owner-busy-removed`（`:281`）/ `clear-alias-owner-busy-removed`
  （`:291`）（busy 门反改请求键单侧）/ `set-alias-owner-reservation-skipped`
  （`:302`）（owner 侧预约跳过）；差异 ② 的 `writer-lease-folded-to-busy`
  （`:262`，存储故障折回 busy）；三段协议的 `transcript-cold-lease-skipped`(-
  `-status`/`-config`)（`:218`/`:225`/`:232`，同锚不同绑定）与三条
  `*-checked-append-reverted`（`:243`/`:248`/`:253`）。静态核
  `python3 -I negative_controls.py --check` → `#1457 55 mutation anchors OK;
  #2065 45 literal anchors OK`（本件九行 + 既有 46/38；收货 rebase 真合并
  main 侧 #2562/#2564 四行后同表为 59/49，`--check` 复核过）；九行新/迁行
  样本跑全 KILLED（改坏红，证据
  `_tmp/20261026-issue-1872/negctl-s3-sample/`）。
- 绿跑（worker 树 HEAD `1f03a0b95`，工作树干净）：`cargo test -p
  pilotdeck-gateway --lib -- native_process::` → `347 passed; 0 failed`、
  `-- runtime::session::` → `340 passed; 0 failed`；fmt --check 与 clippy
  --all-targets -D warnings 无输出。收货在 rebase 后 head `44bf56991` 于 cb8
  独立 verify 树复跑定向套件（证据 `_tmp/20261109-1872-harvest/`）。
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
