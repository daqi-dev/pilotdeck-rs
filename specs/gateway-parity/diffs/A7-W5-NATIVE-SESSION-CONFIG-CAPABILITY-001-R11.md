---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R11
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

更正件（R 链，承接 `A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R10`；frontmatter
`original` 按账本机器规则指基础 ID，与 R9/R10 同法，承接关系由文件名与正文表达）。
登记差异 ②（隔离项目准入对 `projectKey: null` 的处理，R9 五条差异的第 5 条）已由
实现单 #2272 收敛：准入把显式 null 按 nullish「未提供」处理，回退配置项目。本件
更正 R10 的两处陈旧指针——R10「Rust 行为」节写「同原条目，不变」、「机器判据」节
整节复用 R9 判据，而 R9 判据自 #2272 合入（PR #2387 MERGED，`4b764a784` +
fix-r1 `47a99c577`，2026-11-08）后与现树对不上（源 sha256、null 格六格钉法、
null 格定向负控形状均已变化，逐项见「机器判据」）。纯冻结账本操作：零产码、零
测试改动，不改任何已有条目一个字节，不新增判分豁免，不改判定规则与判分语义。

## 基线依据

同原条目 `A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R9`，不变：固定基线
`cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`（`resources/PilotDeck`，只读）。TS 侧
nullish 读法与 R9 / #2272 登记一致：`createLocalGateway` 的 `delete/rename/tag`
包装读 `input.projectKey ?? fallbackProjectRoot`（编译产物
`dist/src/cli/createLocalGateway.js` 的同名实现，与 R9:31 登记的源
`createLocalGateway.ts:1117/1130/1143`（`??` 读行）同形），`resume/close` 不读
`projectKey`。
#2272 r2 的 40 条真实链路复跑登记于
`_tmp/20261109-archive-wt-2272/20261108-issue-2272-r2/ts-probe/probe-null.out`
（wt-2272 回收后的归档址，原件路径已失效）；fix-r1 的 mcp_server_status 线路
实测登记于
`_tmp/20261109-archive-wt-2272/20261108-issue-2272-fix1/ts-probe/probe-null.out`。本件开单点（origin/main
`de1a7e894`）在固定基线上按 #2272 探针同形复测 12 条（场景 A：五方法 ×
`projectKey:null` / 未给，加 `delete` 空白键 × `projectKey:null` / 未给；Node
v22.22.0 / ICU 78.2）：`null` 与未给逐格同答——`resume` 回显（null 格回显
`projectKey:null`）、`close` `undefined`、`delete` `SESSION_NOT_FOUND` / 空白键
`INVALID_SESSION_KEY` / `sessionKey is required.`、`rename`/`tag`
`{"updated":true}`；原始输出
`_tmp/20261109-2563-harvest/ts-probe/probe-null-r11.out`（主仓收货副本；原件在
worker 沙箱树 `_tmp/20261026-issue-2563/ts-probe/`、探针 import 指向其 runner
副本，树回收后以主仓副本为准）。收货复跑补溯源（#2601 评审 ds/boundary#4）：同
版本 Node v22.22.0 / ICU 78.2 在主仓冻结基线 `resources/PilotDeck`
（`cc7c4e653`）上同形复测 12 格，探针 import 直指其
`dist/src/cli/createLocalGateway.js`（sha256
`eb8ee430b7f03bee4cd28c0367d606484f066873c2ae246286ae46aae5ce37b0`），输出与
原始记录逐条一致，复跑输出
`_tmp/20261109-2563-harvest/ts-probe/probe-null-r11-repro.out`（主仓）。

## Rust 行为

**差异 ② 已收敛**（#2272，即 R10「批准记录」② 处置的落地）：请求级隔离项目
准入的 filter 由 `!v.is_undefined()` 改为 `!matches!(v, JsValue::Undefined |
JsValue::Null)`（`native_process/requests.rs:212-223`，现行 `requests.rs` sha256
`8deee51240ad155bc090d6bc3cb62fd41f70053b72101e15253bc677318dc9c2`）——显式
`projectKey: null` 按「未提供」处理，回退本进程配置项目，与 handler 侧
`resolve_project`（`session_dir.rs` 的 `unwrap_or_else`）与
`fill_default_project_key`（`native_archive.rs`）的 nullish 口径统一。R9「Rust
行为」末段登记的「同一进程内对 null 的两种解释并存」这一事实基础随收敛消除——
该事实基础的比较层是请求级准入（`requests.rs`）对 handler 侧读者
（`fill_default_project_key` / `resolve_project`），R9 原文登记的也仅这两层。
submit 侧入口（`SubmitHostServices::workspace_id`，`services.rs:1442`，现行
sha256 `fecc98b93e29bd8469d43b6bc6b716565317049fa2a390e88b148d075b21a291`；
`submit_turn` 由连接层直派不经 `Requests::request`）的 projectKey 门仍是
`!v.is_undefined()` 旧形状（显式 null 答 `projectKey must be a valid path
string.`），不在 R9 该条事实基础的比较层内，已开 #2562 跟踪（前置于 #2272
关单收口），本件不重复登记、不改其状态。

收敛后逐格（`session_dir.rs` 入库用例钉住的现状，源 sha256
`eb1212e1200b6925c5433be7050c9553e2a128873a0a775fe7ea31f558ba5f6e`）：

- `projectKey:null` + 空白 `sessionKey`：回退配置项目后由方法各自的
  `require_session_key` 答 `INVALID_SESSION_KEY` / `sessionKey is required.`（与
  未给 `projectKey` 的空白键同答；TS 场景 A 的 delete 家族同形——`resume`/`close`
  的空白键格按 R9 差异 ① 由 provider 成功处理，不在本件同形主张内，比较层为
  IPG 直调）；
- `projectKey:null` + 合法 `sessionKey`：在配置项目上执行方法——`resume_session`
  回显 `{sessionKey:…}`（仍无 `projectKey` 字段，R9 登记的回显形状差异保留）、
  `close_session` `undefined`、`delete_session` `SESSION_NOT_FOUND`、
  `rename`/`tag` `{"updated":true}`（TS 场景 A 在 IPG 直调层同形，唯 `resume`
  回显形状除外；`close` 的 `undefined` 与 WS 帧层 `{"ok":true}` 包装差别按 R9
  登记保留）；
- fix-r1 补钉的共享 config 线路：`mcp_server_status` 对 `{"projectKey":null}`
  与未给同答 `{"servers":[]}`（非字符串 `projectKey` 仍被准入拒，见「机器
  判据」的用例与 #1432 登记）。该用例的文档注释（`mcp_status_tests.rs:585`
  附近）仍引回收前路径 `_tmp/20261108-issue-2272-fix1/ts-probe/probe-null.out`，
  现址为上文归档路径；注释属存量测试字节，本件零字节约束下不动，修字开
  #2614 跟进（前置于 #2563 关单收口）。

R9 五条差异的其余四条（① `resume/close` 上 Rust 多出的 `sessionKey` 门、③
不受服务项目的码与报文形状、④ Rust 回退恒为配置项目无场景 B 可测，及
`resume` 回显 / `close` `undefined` 的成功形状差别）按 R9 登记不变，本件不重复
登记、不改其登记状态。

## 范围

覆盖：差异 ② 的收敛事实与收敛后 Rust 行为（上节）、机器判据改绑（下节）。不
覆盖（同 R9 原文，不变）：`projectKey: ""` 与相对路径 `projectKey`（并入
`#2199` 一起测）、`params` 整体为 `null`（R7 已登记）、`projectKey` 孤立代理
项（#2083）、U+0085 空白集合（#2097）、`projectKey` 非字符串的 Node
`ERR_INVALID_ARG_TYPE`（R7 已登记）、`config.rs` 族门序（#2199）、
`deleteEphemeralSession` 钩子命中路径。本件零产码零测试：不动
`session_dir.rs` / `requests.rs` / `mcp_status_tests.rs` / 任何 fixture 的字节，
不动 `state/board.md`。他家条目引 #2272 收敛前 `requests.rs` 形状的（如
`A7.8-ALWAYS-ON-DEFERRED-001-R1.md:66-83` 以其合入基线 `e22b4ffb7` 为锚的
实读快照——该件已自证快照性质），其刷新走该家自身的更正程序，不随本件。

## 机器判据

以下全部现核于本件开单基点 origin/main `de1a7e894`（行号为该提交上的行号，
非 R9 开单时点的行号）：

- 入库用例：`blank_key_gate_order`（`session_dir.rs:873`，两条报文常量
  `:869-871`）与五个方法入口 `resume_session_blank_key_gate_order`（`:1032`）、
  `close_session_blank_key_gate_order`（`:1037`）、
  `delete_session_blank_key_gate_order`（`:1042`）、
  `rename_session_blank_key_gate_order`（`:1047`）、
  `tag_session_blank_key_gate_order`（`:1052`）。现行 `session_dir.rs` sha256
  `eb1212e1200b6925c5433be7050c9553e2a128873a0a775fe7ea31f558ba5f6e`——R10:34 /
  R9:110 所引 `1ef473fa…` 已随 #2272 失效，复算时须以本行为准。
- 六格钉法（#2272 后的现行形状，取代 R9「范围」节「入库用例」的旧钉法）：
  (1) 无 `projectKey` 时缺键 / 空白键 → `INVALID_SESSION_KEY`；(1b)
  `projectKey:null` + 空白键 → 同 (1)（sessionKey 门答空白键，正是收敛的直接
  证据）；(2) 显式不受服务 `projectKey` + 空白键、显式不受服务 `projectKey` +
  合法键（对照）→ 同一条准入错误；(3) `projectKey:null` + 合法键 → 按方法分叉
  断言（`resume` 回显无 `projectKey` / `close` `undefined` / `delete`
  `SESSION_NOT_FOUND` / `rename`-`tag` `updated:true`）。R9:77 旧钉法
  「`projectKey:null` 两格 → 同一条准入错误」不再成立。
- 共享 config 线路：`null_project_key_falls_back_and_non_string_stays_refused_by_admission`
  （`mcp_status_tests.rs:594`）；#1432 原先钉旧行为的
  `null_and_non_string_project_keys_stay_refused_by_admission` 已由 fix-r1
  （`47a99c577`）改名并对齐 nullish 断言，R9 时点的该测试名在现树不存在。
- 负控：`admission-null-refused`
  （`src/pilotdeck-gateway/tests/fixtures/native-config/negative_controls.py:299-307`）
  ——把准入 filter 从 nullish 形状变异回 `!v.is_undefined()`（「nullish 修回
  去」，即回到 #2272 前旧行为），绑 `resume_session_blank_key_gate_order`，断言
  锚 `#2272 a null projectKey falls back like a missing one, so the sessionKey
  gate answers the blank key`（绑定测试先死在 (1b) 空白键 null 格）。锚点静态
  核：`python3 -I negative_controls.py --check` → `#1457 46 mutation anchors OK;
  #2065 36 literal anchors OK`（本件开单点实测，工作目录
  `tests/fixtures/native-config/`；静态核与负控变异跑互斥——两者都是对同一批
  源文件的锁外读，须在无并发负控的树上执行）。改坏红 / 还原绿的协议跑按
  #2272 的登记由协调者在宿主 HEAD 上执行（runner never-bypass-HEAD 拒未提交
  树，本件开单时无法代跑）；前置条件：宿主树该时段无其它 cargo / 负控共用
  （变异体是对 `requests.rs` 的就地改写，harness 的树锁与原子还原只挡走
  harness 的跑者），或改在独占副本上跑；跑完核 `requests.rs` sha256 回
  `8deee51240ad155bc090d6bc3cb62fd41f70053b72101e15253bc677318dc9c2` 且
  `git status` 干净。R9:121 的 null 格定向负控（把用例里的 `JsValue::Null`
  改成 `JsValue::Undefined`，变异体 `session_dir.rs` sha256 `9b3c9f81…`）自
  #2272 起失去判别力——null 与 undefined 现同为 nullish，该变异会让用例照常
  通过（SURVIVED）；其判别力由 `admission-null-refused` 在准入层 (1b) 格
  取代，(3) 各方法成功形状仅由绿跑正控钉住（Null→Undefined 变异与 null
  同义，定向负控自收敛起结构上不可造）。
- 绿跑（本树，HEAD `de1a7e894`，工作树干净，源 sha256 与
  `git show HEAD:…` 相同）：`cargo test --offline -p pilotdeck-gateway --lib
  native_process::methods::session_dir::tests` → `19 passed; 0 failed; 0
  ignored`，日志含 `Compiling`（非旧制品），原始输出
  `_tmp/20261109-2563-harvest/rust-probe/gate-order-tests-r11.out`（主仓收货
  副本，原件在 worker 沙箱树
  `_tmp/20261026-issue-2563/rust-probe/`，树回收后以主仓副本为准）；
  `null_project_key_falls_back_and_non_string_stays_refused_by_admission`
  单跑 `1 passed; 0 failed`。
- 账本闸门：`python3 -I scripts/gateway-parity/check_frozen.py --base
  origin/main` 须 PASS。程序说明：本件是 `diffs/` 只追加更正，`diffs/` 条目不进
  `FROZEN.md` 哈希清单（`manifest_inventory` 不含 `diffs/`），故无需
  `--write` 重登记、报告的 `entries` 计数不变（仍是 30）；`check_frozen.py`
  的 `check_diffs` 规则要求更正件 frontmatter `original` 写基础 ID
  （`A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001`），写 `-R10` 会被该检查拒。
  闸门复算读工作树当前字节且不持 ruler 租约：核闸门前确认无负控在改
  `scripts/gateway-parity/**`，任何由「hash list differs」触发的 `--write` 须
  先复核工作树与 HEAD 一致再执行（装置侧缺租约已开 #2603 跟进）。

## 批准记录

同 R10，不变：**已批准（AI 预批复：AI 代批，待人复核，可否决）**，等级、依据
（2026-11-07 20:3x 人令，memory「ai-pre-verdict-policy」）与两条 coord 回执
（6707009289 / 6707020393，2026-11-07）全按 R10「批准记录」原文，此处不重复
摘录。② 的处置按该批复走收敛而非接受差异：实现单 #2272 已由 PR #2387
（MERGED，`4b764a784` + fix-r1 `47a99c577`，2026-11-08）落地，差异 ② 随收敛
消除（非豁免、不转已接受差异）。①（`resume/close` 的 `sessionKey` 门保持
Rust 门，登记为已接受差异）不受本件影响。本件为纯账本更正，不改变 R10 的
批准状态，不新增判分豁免。R10 末段「届时循本账本规则再出 `-R11` 更正」原为
人复核否决场景的预留路径；本件的实际触发是 ② 收敛落地后 R10 判据指针陈旧
（#2563），批准语义不变。若人复核否决（部分或全部），仍循 R10 登记的否决
路径在 #2090 决策线上另行拍板。
