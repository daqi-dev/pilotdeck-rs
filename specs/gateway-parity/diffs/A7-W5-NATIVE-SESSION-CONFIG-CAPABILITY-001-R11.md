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
`dist/src/cli/createLocalGateway.js` 的同名实现，与 R9 登记的源
`createLocalGateway.ts:1116/1129/1142` 同形），`resume/close` 不读 `projectKey`。
#2272 r2 的 40 条真实链路复跑登记于
`_tmp/20261108-issue-2272-r2/ts-probe/probe-null.out`；fix-r1 的
mcp_server_status 线路实测登记于
`_tmp/20261108-issue-2272-fix1/ts-probe/probe-null.out`。本件开单点（origin/main
`de1a7e894`）在固定基线上按 #2272 探针同形复测 12 条（场景 A：五方法 ×
`projectKey:null` / 未给，加 `delete` 空白键 × `projectKey:null` / 未给；Node
v22.22.0 / ICU 78.2）：`null` 与未给逐格同答——`resume` 回显（null 格回显
`projectKey:null`）、`close` `undefined`、`delete` `SESSION_NOT_FOUND` / 空白键
`INVALID_SESSION_KEY` / `sessionKey is required.`、`rename`/`tag`
`{"updated":true}`；原始输出
`_tmp/20261026-issue-2563/ts-probe/probe-null-r11.out`。

## Rust 行为

**差异 ② 已收敛**（#2272，即 R10「批准记录」② 处置的落地）：请求级隔离项目
准入的 filter 由 `!v.is_undefined()` 改为 `!matches!(v, JsValue::Undefined |
JsValue::Null)`（`native_process/requests.rs:212-223`，现行 `requests.rs` sha256
`8deee51240ad155bc090d6bc3cb62fd41f70053b72101e15253bc677318dc9c2`）——显式
`projectKey: null` 按「未提供」处理，回退本进程配置项目，与 handler 侧
`resolve_project`（`session_dir.rs` 的 `unwrap_or_else`）与
`fill_default_project_key`（`native_archive.rs`）的 nullish 口径统一。R9「Rust
行为」末段登记的「同一进程内对 null 的两种解释并存」这一事实基础随收敛消除。

收敛后逐格（`session_dir.rs` 入库用例钉住的现状，源 sha256
`eb1212e1200b6925c5433be7050c9553e2a128873a0a775fe7ea31f558ba5f6e`）：

- `projectKey:null` + 空白 `sessionKey`：回退配置项目后由方法各自的
  `require_session_key` 答 `INVALID_SESSION_KEY` / `sessionKey is required.`（与
  未给 `projectKey` 的空白键同答；TS 场景 A 同形）；
- `projectKey:null` + 合法 `sessionKey`：在配置项目上执行方法——`resume_session`
  回显 `{sessionKey:…}`（仍无 `projectKey` 字段，R9 登记的回显形状差异保留）、
  `close_session` `undefined`、`delete_session` `SESSION_NOT_FOUND`、
  `rename`/`tag` `{"updated":true}`（TS 场景 A 同形，唯 `resume` 回显形状除外）；
- fix-r1 补钉的共享 config 线路：`mcp_server_status` 对 `{"projectKey":null}`
  与未给同答 `{"servers":[]}`（非字符串 `projectKey` 仍被准入拒，见「机器
  判据」的用例与 #1432 登记）。

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
不动 `state/board.md`。

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
  `tests/fixtures/native-config/`）。改坏红 / 还原绿的协议跑按 #2272 的登记由
  协调者在宿主 HEAD 上执行（runner never-bypass-HEAD 拒未提交树，本件开单时
  无法代跑）。R9:121 的 null 格定向负控（把用例里的 `JsValue::Null` 改成
  `JsValue::Undefined`，变异体 `session_dir.rs` sha256 `9b3c9f81…`）自 #2272 起
  失去判别力——null 与 undefined 现同为 nullish，该变异会让用例照常通过
  （SURVIVED），已被上面的源码侧负控 `admission-null-refused` 取代。
- 绿跑（本树，HEAD `de1a7e894`，工作树干净，源 sha256 与
  `git show HEAD:…` 相同）：`cargo test --offline -p pilotdeck-gateway --lib
  native_process::methods::session_dir::tests` → `19 passed; 0 failed; 0
  ignored`，日志含 `Compiling`（非旧制品），原始输出
  `_tmp/20261026-issue-2563/rust-probe/gate-order-tests-r11.out`；
  `null_project_key_falls_back_and_non_string_stays_refused_by_admission`
  单跑 `1 passed; 0 failed`。
- 账本闸门：`python3 -I scripts/gateway-parity/check_frozen.py --base
  origin/main` 须 PASS。程序说明：本件是 `diffs/` 只追加更正，`diffs/` 条目不进
  `FROZEN.md` 哈希清单（`manifest_inventory` 不含 `diffs/`），故无需
  `--write` 重登记、报告的 `entries` 计数不变（仍是 30）；`check_frozen.py`
  的 `check_diffs` 规则要求更正件 frontmatter `original` 写基础 ID
  （`A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001`），写 `-R10` 会被该检查拒。

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
