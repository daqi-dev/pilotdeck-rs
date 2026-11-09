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

修复轮二（DS 增量评审 sibling P1 仲裁，`_tmp/20261109-1872-harvest/
ts-blankkey-probe.out`）：P1 的前提（TS 对 permission 空键答
`INVALID_SESSION_KEY`）经实测+源码双证伪——真实装配（createLocalGateway.ts
的 `setPermissionMode` 回调）外层 `hasActiveTurn` 先答（probe C），空键无
回合才落回调的 sessionKey 门，`set_permission_mode` 两格均已与 TS 同形，
不改。model set 的空键卫改形：请求侧对空键照查（probe B：`""` 拼写上的回合
答 `SESSION_BUSY`），仅 owner 侧共轭项对空键跳过（probe E/G：空请求键落
port write 的 sessionKey 门）；配 set/clear/permission 三用例与三负控行。
submit 侧 null→`""` 折叠坍缩 TS 两拼法的残余格开单 #2692（前置于 #1457
收尾）。

修复轮三（DS/MM 增量评审 P2 处置）：空键的 owner 侧转写预约整体跳过（见
Rust 行为——修前折叠 owner 的并发预约可为空键请求先答 `SESSION_BUSY`，两家
两轮独立撞上）；permission 空键无回合格补用例与负控；回退支路的复查→close
非原子窗口并入 #2687 登记；thinking/style/seed 的空键门序经 TS 源码核为
sessionKey 门先于 busy 门（InProcessGateway.ts:2364-2366 / :2381-2383 /
:2423-2429），与 Rust 三法 `require_session_key` 先于 `has_active_turn`
逐法同形，零改动登记。

修复轮四（DS 增量评审两 P1，均为负控装置格、不适用修复轮上限放行——
P2-b「影响对拍结论按 P1」同旨）：①修复轮三的「owner 解析整体跳过」使
busy 门内空键卫对空键恒不可达、其负控行退化为等价变异（整表跑假 FAIL）——
改形为 owner 照旧解析下传、仅 `owner_reserved` 以空键合取跳过预约，卫保持
可达；②permission 负控行的锚在 require 弱化（返回 Ok）路径不进 panic 载荷
（cb8 r5 实跑 INVALID 与评审静态推导同判）——锚随 bound 测试 `expect_err`
的消息改。两行负控随形状换锚；P2（thinking/style/seed 空键无正/负控）开
跟进单前置登记。

修复轮五（DS-fix4 conc#1 P2-c，仅文档）：「不再能为空键先答 busy」绝对句
被他请求持有的 owner 预约证伪——限定「本请求自身」并首次登记跨请求交错格
#2710（空键实例）。

修复轮六（DS-fix5 增量复核 boundary P1 与 sibling P2 两角色独立撞上 +
codex 三谱系同撞窗口口径 P3，仅文档）：跨请求交错格的登记从空键实例扩为
通用族（非空别名在飞实例 + 两处绝对句限定 + 窗口口径自「append 全程」
对齐为「入口取得预约至 append+close 双释放」，与 Rust 行为段的
check→append→close 口径一致）；三名统一为「跨请求交错格」。

修复轮七（DS-fix6 boundary P1 与 concurrency/sibling 同族 P2、codex fix-r6
复审 P2 两谱系撞上，仅文档，同处第三次修复）：族收窄为按机制定义——以
「涉及一条 owner 侧额外条目」为准（A 的请求拼写条目对撞在 TS 同形同答
busy、非差异，此前「任一条目」过度纳入）；补镜像实例③（B 别名的 owner
预约撞 A 的请求键预约）与 close 回退支路实例④（`:411-415` 第三预约点）；
实例②③补「owner 无活跃回合」前置；clear 侧第三绝对句（「空键恒答」）与
范围段第四处（「非别名路径与 TS 同形」）补限定；窗口行号统一为 set
`:191-238` / clear `:312-353`（入口 `:191`/`:312`、owner `:219`/`:334`、
双释放 `:232-238`/`:347-353`），并注明「正常返回路径——future 取消的释放
缺口并入 #2391」。同处第 3 次修复即最后一次：若再判 P1 按「同一处修 3 次
不过挂起」转人工。

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
- 空键族（修复轮二仲裁，`_tmp/20261109-1872-harvest/ts-blankkey-probe.out`，
  同基线 cc7c4e65，cb8 · Node v22.22.1 / ICU 78.2）：`beginTurn("")` 无键校验
  接受；`""` 回合在飞时 set 与 permission 都答 `SESSION_BUSY`（:B/:C，外层
  busy 门先于回调/port 的 sessionKey 门）；null 回合对 `""` 请求答
  `INVALID_SESSION_KEY`（:E）、对 null 请求答 `SESSION_BUSY`（:F）；无空键
  回合时 set 的 `""` 请求与 `NativeSessionModelSelectionPort.write("")` 直调都答
  `INVALID_SESSION_KEY "sessionKey is required."`（:G/:H）。

## Rust 行为

差异 ①（#1872）：两个 wrapper
（`config.rs:189-240` set / `:310-355` clear，修复轮四后 sha256
`881bae3b65dfd4db42ef9c63f899d440a29c0aed65d099f71d7c4fa5df1d1b12`）先以
`live_transcript_owner`（`:245`）解析别名折叠到的 owner 注册键，请求键与 owner
键同时过 `has_active_turn` 门（`:269-273` / `:366-368`）——owner 有回合即
`SESSION_BUSY`，报文与 TS 精确键格逐字相同；两键同时进 replacement 协调器的
transcript-write 预约表（check→append→close 窗口不纳新回合，set `:191-238` /
clear `:312-353`）。
**空串键**（修复轮二改形，`e9ad34549` 的整门跳过卫废弃）：busy 门的请求侧
对空键照查——`""` 拼写上的回合答 `SESSION_BUSY`（probe B，与
`set_permission_mode` 同形）；仅 owner 侧共轭项以 `trimmed_string_is_non_empty
(key)` 限定，空请求键（`""` 经 `sanitize_session_id` 折到 `session` 转录的
owner）不再让 owner 先答 busy，落回 port write 的 sessionKey 门答
`INVALID_SESSION_KEY`（probe E/G；无他请求持同拼写预约时——否则先答
busy，见下跨请求交错格）。修复轮三把该排除扩到预约，修复轮四改形
（DS 增量评审 P1：整体跳过使 owner 解析对空键恒 None、busy 门内该共轭项成
死代码、其负控行退化为等价变异）：owner 照旧解析下传——busy 门内的空键卫
保持可达、其负控行不退役——仅 `owner_reserved` 以空键合取跳过 owner 侧预约
（`config.rs:205-218` / `:322-333`），**本请求自身**的折叠 owner 并发
转写预约不再能为空键先答 busy（TS 只预约请求拼写，`InProcessGateway.ts:2193`
/ `:2203`）；请求键自身的预约照旧（与 TS 同形）。跨请求交错格——凡涉及
owner 侧额外预约的跨请求冲突先答 `SESSION_BUSY`，为差异 ① 的连带残余窗口，
详见下残余窗口段（#2710）。反向镜像格——请求键为折叠
拼写（如 `session`）、owner 为空串拼写——owner 有回合仍答 busy，属差异 ①
已登记格。clear 侧 `require_session_key` 先于 busy
门，与 TS `sessionModelClear` 的 requireSessionKey→hasActiveTurn 顺序同形
（空键答 `INVALID_SESSION_KEY`——无他请求持同拼写预约时，否则入口先答
busy，见跨请求交错格）。null 请求键（as_string 为 None）整门
跳过、答 `INVALID_SESSION_KEY`；与 TS null 拼法回合的两格分歧（probe E/F
的 TS 侧）源于 submit 侧 null→`""` 折叠（`runtime/turn/submit.rs:351` +
`:1288-1290`，基线既有）坍缩两拼法，登记 #2692。
成功路径的追加固有语义不变：过 owner 的 recorder 追加后关闭 *owner* 拼写
（`close_model_selection_owner`，`:399-423`，#1457 fix-r3 的关闭目标在 #1872 的
别名门下由正控钉住；其回退支路——owner 为 append 期间才发布——修复轮起有用例
与负控，见机器判据）。非别名路径（请求键 = owner 键）行为与修前及 TS 完全
一致——无并发他请求持同拼写预约时；否则同受下述跨请求交错格影响。
**登记的残余窗口**（修法开单，不在本件）：直关分支
（`target == session_key || reserved_owner == Some(target)`）不复查
`has_active_turn`——已过 preflight、尚未 `begin_turn` 的回合仍会被 close 中止
（#2687）；回退支路的 `has_active_turn` 复查与 close 非原子，复查通过后才
入驻的回合同样会被 close 中止（同型窗口，并入 #2687 评论登记）；回退支路报
busy 时条目已落盘，重试会追加第二条 model-selection 条目
（代码注释与本件双登记，#2687 同单跟进）；跨请求交错格——差异 ① 的
Rust-only owner 侧额外预约（请求键预约与 TS 同形不能去、去 owner 侧则回修前
abort 窗口）的连带族，**按机制定义、实例非穷举**（并发判据与实测归 #2710）：
预约表为精确键匹配（`runtime/replacement.rs:154-167`），凡预约冲突涉及一条
owner 侧额外条目——持有方为 wrapper 预约（`config.rs:219-228` / `:334-343`，
owner 折叠拼写且 ≠ 持有方请求拼写、请求键非空）或 close 回退支路预约
（`:411-415`，target 拼写）——即构成 Rust-only 分歧（Rust 先答
`SESSION_BUSY`，TS 只预约请求拼写 `InProcessGateway.ts:2193` / `:2203`）；
冲突另一侧可为对方任一预约（入口请求键 `:191-197` / `:312-318`，或 owner
侧）。纯请求拼写对撞（两侧均为请求键）TS 同预约同答 busy，不属本族。已证实
例：①B 空键撞 A 的 `""` owner 预约（该拼写仅由 submit 缺省/null `sessionKey`
折叠产生，#2692）——TS 答 `INVALID_SESSION_KEY`；②A 别名 `a-b` 在飞持
owner `a/b`、B 精确 `a/b`（owner 无活跃回合）——TS B 无冲突走正常路径成功
（源码读法）；③镜像：A 精确 `a/b` 在飞持请求键 `a/b`、B 别名 `a-b` 的
owner 预约 `a/b` 撞之（owner 无活跃回合）——TS B 成功；④close 回退支路的
target 预约（TS 无任何 owner 侧预约，需实测）。②③为常规双拼写写法的并发格，
非边角。窗口＝持有方自取得相应预约（入口 `:191` / `:312`、owner `:219` /
`:334`）至 append+close 后双释放（`:232-238` / `:347-353`，正常返回路径——
future 取消的释放缺口已并入 #2391）。现有空键用例钉的是 owner 侧跳过、非
本格（#2710，前置于 #1457 收尾）。

差异 ②（#1873；#1874 收拢）：
`session_record_status`（`session_dir.rs:448-517`，squash 时 sha256
`76bd7eab133b6dc6d9849b597e4dde7b778fe834bb2473f92117781d831333b7`；rebase
并入 main 侧 #2564 尾部用例后现行
`798e59ae84ba9e037c1437ccec85d460669c797acac90eaf33d4443a4201871f`，增量
不在 `:448-517` 段）冷路径经共享
三段协议 `transcript_locks::append_metadata_with_lease`
（`transcript_locks.rs:131-176`，sha256
`3f841068ee84c873f15412ee6925d5a96cc615dcbf66e3e9d69e9b5c45bdff76`——修复轮
二起含 `transcript_exists` 探测错误窗口的 #2686 注释锚，逻辑零变化）：live
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
set/clear 全链，本件零变化——跨请求交错格除外，该格两侧皆可非别名到达，
见上）；TS 别名成功格的 Rust 侧对应（Rust 在 owner 有
回合时恒 busy，owner 无回合时别名成功且关闭 owner——与 TS「成功且不关」在
关闭目标上仍有差别，属 #1457 fix-r3 已登记的关闭语义，非本件新差异）；
`rename`/`tag` 的别名格（#1457 fix-r2/r3 已登记；但其**存储故障格**随差异 ②
的 `try_writer_lease` Result 化一并改变，见上文补记——三调用点共用一条共享
协议，机器判据见下节）；`SESSION_WRITER_BUSY` 的会话 factory 侧用法（#1860
已登记）；#1874 的三调用点收拢（收拢本身纯内部重构、答案零变化；**例外**即
上述存储故障格，系 #1873 的分离而非收拢所致，测试与负控迁移仅随锚）。同族
四法（permission/thinking/style/seed）**不做** owner 侧 busy 门：TS SessionRouter
按精确键索引、四法同样看不见 owner 的回合；thinking/style close 请求拼写
（`config.rs` 各法自带），permission/seed 不 close（permission 的 override 存
于会话覆盖表、seed 只 `begin_maintenance`），都不会打断 owner 回合——与 TS 同
形，保持现状非漏改。permission 的空键行为经修复轮二仲裁确认与真实装配同形
（外层 busy 门先答 probe C、回调 sessionKey 门在后，`config.rs:538-546` +
createLocalGateway.ts 回调），本件零改动；修复轮三补其空键无回合格的用例。
thinking/style/seed 的空键门序经 TS 源码核为 sessionKey 门先于 busy 门
（InProcessGateway.ts:2364-2366 / :2381-2383 / :2423-2429），Rust 三法
`require_session_key` 先于 `has_active_turn`（`config.rs:560-563` / `:625-628`
/ `:732-746`）逐法同形，零改动登记。
`services.rs::record_failure`（turn 路径）仍用未检式 `record_agent_status_message`：
非请求路径，#1874 范围未覆盖，按现状豁免（TS 侧 writer 关闭语义未实测，收口
另单）。不放宽判分。

## 机器判据

以下现核于修复轮四（行号为该提交上的行号；`e9ad34549` / `44bf56991` /
`c5fba0d57` 上的旧行号见 Git 历史）：

- 差异 ① 用例：`session_model_set_rejects_active_turn_on_alias_owner`
  （`config.rs:1407`）与 `session_model_clear_rejects_active_turn_on_alias_owner`
  （`:1681`）——recorder 注册 `al-set/session` / `al-clear/session`、
  `begin_turn` 在 owner 拼写、请求 `al-set-session` / `al-clear-session`，断
  `SESSION_BUSY` + 逐字报文；空串键用例
  `session_model_set_blank_key_answers_the_session_key_gate`（`:1445`，修复轮
  新增——recorder 注册 `session` 且其上有回合，请求空串键断
  `INVALID_SESSION_KEY` "sessionKey is required."，owner 侧不先答 busy）；
  空串键六联（修复轮二/三新增）：`session_model_set_reports_busy_for_a_turn_
  held_on_the_blank_key`（`:1489`——recorder 与回合都挂 `""` 拼写，请求空串
  键断 `SESSION_BUSY` + 逐字报文，请求侧对空键照查，probe B）、
  `session_model_clear_blank_key_answers_the_session_key_gate`（`:1527`——同
  挂 `""` 回合，clear 答 `INVALID_SESSION_KEY` 且回合存活，require 先于 busy
  门）、`set_permission_mode_reports_busy_for_a_turn_held_on_the_blank_key`
  （`:1562`——`""` 回合在飞，permission 答 `SESSION_BUSY` + 逐字报文，probe C，
  仲裁后 permission 零改动即同形）、`set_permission_mode_blank_key_answers_
  the_session_key_gate`（`:1597`，修复轮三新增——无任何回合时空键落
  permission 的 sessionKey 门，断 `INVALID_SESSION_KEY` + 逐字报文）、
  `session_model_set_blank_key_ignores_a_busy_owner_reservation`（`:1627`，
  修复轮三新增——recorder 注册 `session` 且其转写预约被并发持有，请求空串
  键断 `INVALID_SESSION_KEY` 而非 owner 预约的 busy）与 clear 孪生
  `session_model_clear_blank_key_ignores_a_busy_owner_reservation`（`:1657`）；
  窗口用例
  `session_model_set_holds_the_owner_reservation_through_the_write`
  （`:1715`）与其 clear 孪生
  `session_model_clear_holds_the_owner_reservation_through_the_write`
  （`:1790`，修复轮新增）——请求停在转录 fence 上时预约表里两拼写并在、事后
  双释放；正控 `model_set_closes_the_owner_spelling_of_an_alias_request`
  （`:1970`）——别名请求成功、条目落盘、`current_handle("g7/session")` 事后
  `None`（关闭目标是 owner 拼写）、reload `restore()` 通过；回退支路双用例
  `close_model_selection_owner_reports_busy_for_an_owner_seen_by_the_append`
  （`:2050`，修复轮新增——append 期间才见 owner 且有回合：断 `SESSION_BUSY`、
  回合未被 abort、预约已释放）与 `close_model_selection_owner_closes_an_
  owner_seen_by_the_append`（`:2108`，修复轮新增——无回合则关闭该 owner 拼写）。
- 差异 ② 用例：`status_cold_write_reports_busy_while_a_session_holds_the_lease`
  （`session_dir.rs:743`，断 `SESSION_WRITER_BUSY` 且盘上无写入）、
  `status_cold_write_surfaces_a_broken_lock_file_as_an_error`（`:800`，断码非
  busy 且报文含 "Could not open session ledger lock"）、
  `status_reports_a_closed_writer_as_an_error`（`:947`，闭仓 writer 是错误）；
  修复轮新增的另两个调用点对偶：`rename_cold_write_surfaces_a_broken_lock_
  file_as_an_error`（`session_dir.rs:851`）与
  `model_set_cold_write_surfaces_a_broken_lock_file_as_an_error`
  （`config.rs:1929`）——三调用点的存储故障格各有正例；`model_set` 的对应
  busy 格 `model_set_cold_write_reports_busy_while_a_session_holds_the_lease`
  （`config.rs:1853`）；lease 分层单测
  `try_writer_lease_separates_contention_from_storage_failure`
  （`factory_tests.rs:7735`）；双生产者字面钉
  `writer_busy_literals_stay_paired_across_producers`（`factory_tests.rs:7787`，
  修复轮新增——factory 侧 `writer_busy()` 与共享协议 `writer_busy_error()` 的
  code/message 两两相等）。
- 负控（`tests/fixtures/native-config/negative_controls.py`，行号为修复轮四
  提交上的行号）：差异 ① 的 `set-alias-owner-busy-removed`（`:287`）/
  `clear-alias-owner-busy-removed`（`:438`）（busy 门反改请求键单侧）/
  `set-busy-gate-blank-key-guard-removed`（`:306`，修复轮新增、轮二改形、
  轮四起卫重新可达——owner 照旧解析下传，卫移除则空请求键先答 busy；锚为
  用例自身的断言标签，cb8 样本 r1 的 INVALID 即锚未进 panic 载荷所致，已改）/
  `set-blank-key-busy-reverted-to-gate-skip`（`:323`，修复轮二新增——回退到
  整门跳过形状则 `""` 拼写回合答 INVALID 而非 busy，probe B 的机检）/
  `permission-blank-key-busy-skipped`
  （`:341`，修复轮二新增——被仲裁否掉的「permission 加空键卫」形状，probe C
  的机检）/ `clear-blank-key-require-weakened`（`:366`，修复轮二新增——
  require 弱化为空默认则 clear 对 `""` 回合答 busy 而非 sessionKey 门）/
  `set-blank-key-owner-reservation-restored`（`:383`，修复轮三新增、轮四随
  wrapper 改形换锚——`owner_reserved` 的空键合取还原则折叠 owner 的并发预约
  对空键先答 busy）与其 clear 孪生
  `clear-blank-key-owner-reservation-restored`（`:405`）/ `permission-blank-
  key-require-weakened`（`:431`，修复轮三新增、轮四锚改——permission 的
  require 弱化为空默认返回 Ok，bound 测试死于 expect_err，锚随其消息进
  panic 载荷（cb8 r5 实跑 INVALID 与 DS fix-r3 评审同判））/
  `set-alias-owner-reservation-skipped`（`:449`）与其
  clear 孪生 `clear-alias-owner-reservation-skipped`（`:460`，修复轮新增）/
  `close-owner-fallback-busy-skipped`（`:476`，修复轮新增——回退支路复查
  移除则 close abort 在飞回合）/ `set-closes-owner-target-reverted`（`:490`，
  修复轮新增——关闭目标折回请求拼写）；差异 ② 的 `writer-lease-folded-to-busy`
  （`:268`，存储故障折回 busy）；三段协议的 `transcript-cold-lease-skipped`(-
  `-status`/`-config`)（`:224`/`:231`/`:238`，同锚不同绑定）与三条
  `*-checked-append-reverted`（`:249`/`:254`/`:259`）。静态核
  `python3 -I negative_controls.py --check` → `#1457 69 mutation anchors OK;
  #2065 59 literal anchors OK`（本件十九行——初登记九行 + 修复轮一四行 +
  修复轮二三行 + 修复轮三三行，其中三行修复轮四随形状换锚；rebase 并入
  main 侧 #2562/#2564 行后同表为 69/59，`--check` 复核过）；
  新/迁行样本跑全 KILLED（改坏红；初登记九行的证据分布在
  `_tmp/20261026-issue-1872/negctl-s1-sample/`、`negctl-s1-sample2/`、
  `negctl-s1-sample3/`、`negctl-s2-sample/`、`negctl-s3-sample/`；修复轮
  四行中三行 KILLED 见 `_tmp/20261109-1872-harvest/negctl-fix-sample/`（第
  四行锚未命中判 INVALID 已随轮二修复）；修复轮二的五行 KILLED 见
  `_tmp/20261109-1872-harvest/negctl-fix2-sample/`（head 6186bf101，
  `cb8-verify-1872-r4.log`）；修复轮三的三行样本 2 KILLED + 1 INVALID 见
  `negctl-fix3-sample/`（head c5fba0d57，permission 行锚 Ok 路径不达断言
  标签，轮四改锚）；修复轮四的四行样本——三行换锚行加 busy 卫活性证明行
  （`set-busy-gate-blank-key-guard-removed`，卫在 owner 照旧解析后重新可达）
  ——全 KILLED 见 `negctl-fix4-sample/`，head `df757e021`）。
- 绿跑（worker 树 HEAD `1f03a0b95`，工作树干净）：`cargo test -p
  pilotdeck-gateway --lib -- native_process::` → `347 passed; 0 failed`、
  `-- runtime::session::` → `340 passed; 0 failed`；fmt --check 与 clippy
  --all-targets -D warnings 无输出。收货复跑：rebase 后 head `85e9dc55a` 于
  cb8 独立 verify 树定向套件 `351 passed; 0 failed` / `340 passed; 0 failed`；
  修复轮 head `e9ad34549` 同树复跑 `357 passed; 0 failed`（+6：空串键、clear
  窗口、回退双用例、set/rename broken-lock 对偶）/ `341 passed; 0 failed`
  （+1：字面钉），fmt/clippy 绿、负控 63/53（原始输出
  `_tmp/20261109-1872-harvest/cb8-verify-1872.log` 与
  `cb8-verify-1872-r3.log`）；修复轮二 head `6186bf101` 同树复跑
  `native_process::` **360** passed（+3：空串键三联）/ `runtime::session::`
  **341** passed，负控 `--check` 66/56、五行有界样本全 KILLED（原始输出
  `cb8-verify-1872-r4.log`，样本目录 `negctl-fix2-sample/`；fmt/clippy 由
  修复轮三的头复跑覆盖，r4 的这两条命令形参错不采信）；修复轮三 head
  `c5fba0d57` 同树复跑 fmt --check 与 clippy --all-targets -D warnings 绿、
  `native_process::` **363** passed（+3：空键 owner 预约双联 + permission
  空键门）/ `runtime::session::` **341** passed，负控 `--check` 69/59、三行
  样本 2 KILLED + 1 INVALID——permission 行锚在 require 弱化（Ok）路径不进
  panic 载荷，DS fix-r3 评审与实跑同判、修复轮四改锚（原始输出
  `cb8-verify-1872-r5.log`，样本目录 `negctl-fix3-sample/`）；修复轮四 head
  `df757e021` 同树复跑 fmt --check 与 clippy --all-targets -D warnings 绿、
  `native_process::` **363** passed / `runtime::session::` **341** passed，负控
  `--check` 69/59、四行有界样本全 KILLED——三行换锚行（含 permission 改锚行）
  加 `set-busy-gate-blank-key-guard-removed`（卫在 owner 照旧解析后重新可达的
  活性证明，即 DS fix-r3 sibling#1 修复的实测）（原始输出
  `cb8-verify-1872-r6.log`，样本目录 `negctl-fix4-sample/`）。
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
