---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R9
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

登记 + 补齐 R7 明示未闭合的两块（端到端实测、请求级门序用例）。不改产品行为，不新增豁免。两处补齐 + 两处更正：

1. **R7 标为「属读码推断，未实测」的回退项目分支，本轮实测落在「解析不了」那一支。** 真实 CLI 形状 `createLocalGateway({ projectRoot, pilotHome })`（`pilotHome` 与 `projectRoot` 不同、`projectRoot` 未注册）下，`delete/rename/tag` 对三种 `sessionKey` 输入（缺键 / 空白 / 非字符串）的**首错全是 `PROJECT_NOT_FOUND`**（`projectRegistry.ts:50`），不是 `INVALID_SESSION_KEY`，也不是 provider 门上的 `TypeError`。R7 的两分支表已列出这一支，本条只是把它的「未实测」补上，不是推翻 R7 的结论。
2. **R7 预写的请求级期望不成立，两处。** (a) 「空白 `sessionKey` + 不可解析 `projectKey` → `INVALID_SESSION_KEY` 先于 `CAPABILITY_UNAVAILABLE`」：Rust 请求入口 `native_process/requests.rs:305-314` 在方法派发**之前**先做隔离项目准入，`projectKey` 显式存在且不是本进程配置项目 → `gateway_request_failed` + `The isolated native process serves only its configured project.`，故该组合的首错是准入错误；`resolve_project` 的 `CAPABILITY_UNAVAILABLE`（`session_dir.rs:54-56`）在该入口下**不是客户端能看到的第一个码**。(b) R7 同一路径上写的「基准答案恒为 `INVALID_SESSION_KEY`」只在守卫级成立：`projectKey: null` 这一格在请求级同样先撞准入（差异第 5 条），到不了 `require_session_key`。
3. R7 只有表达式级探针的两处，现在是真实链路实测：`resume_session` / `close_session` 在 TS 全链无 `?.trim()` 门（三种输入全部成功），`delete/rename/tag` 在项目可解析时答 `INVALID_SESSION_KEY` / 非字符串可选链 `TypeError`。
4. **`projectKey: null` 一格补齐实测。** R7 的 TS 侧读码（null 走 nullish 回退、换成 `fallbackProjectRoot` 再解析）实测成立；Rust 侧则相反：准入把 null 当「不受服务的项目」直接拒绝，五个方法全部如此，连合法 `sessionKey` 也一样。同一进程里两个层次对 null 的解释相反（见「Rust 行为」末段），本轮只登记、不修。

## TS 依据

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`（`resources/PilotDeck`，只读）。真实链路：`createLocalGateway()` 返回的 IPG + 真实 `GatewayWsConnection`（`hello` 握手后发 `request` 帧），Node v22.22.0 / ICU 78.2。探针脚本 `_tmp/20261107-issue-2090/ts-probe/probe.mjs`，原始输出 `_tmp/20261107-issue-2090/ts-probe/probe.out`（47 条记录）。

场景 A：`pilotHome == projectRoot`（回退项目 = general project，可解析），WS 帧结果：

| 方法 | 缺键 `{}` | 空白 `{"sessionKey":"   "}` | 非字符串 `{"sessionKey":42}` |
|---|---|---|---|
| `resume_session` | `{}`（回显入参） | `{"sessionKey":"   "}` | `{"sessionKey":42}` |
| `close_session` | `{"ok":true}` | `{"ok":true}` | `{"ok":true}` |
| `delete_session` | `INVALID_SESSION_KEY` / `sessionKey is required.` | 同左 | `gateway_request_failed` / `input.sessionKey?.trim is not a function` |
| `rename_session` | 同 delete | 同 delete | 同 delete |
| `tag_session` | 同 delete | 同 delete | 同 delete |

成功对照（`{"sessionKey":"probe/session","projectKey":<projectRoot>}`）：`resume_session` 回显 `sessionKey` + `projectKey`；`close_session` `{"ok":true}`；`delete_session` `SESSION_NOT_FOUND` / `Session not found: probe/session`；`rename_session` / `tag_session` `{"updated":true}`。IPG 直调与 WS 帧在错误形状上同结论；分层差别有两处：裸 `TypeError` 过 `GatewayWsConnection` 后变 `gateway_request_failed`（`GatewayWsConnection.ts:245-253`），以及 `close_session` 成功时 IPG 返回 `undefined` 而 WS 帧包装成 `{"ok":true}`（`resume_session` 的成功值两侧同为回显对象）。

场景 B：`pilotHome != projectRoot`（回退项目 `fallbackProjectRoot = projectRoot`，未注册），`delete/rename/tag` × 三种输入 → 全部 `PROJECT_NOT_FOUND` / `Unknown projectKey: <projectRoot>`；再补「空白 `sessionKey` + 显式 `projectKey:"/nonexistent/pdrs-2090"`」→ 同样 `PROJECT_NOT_FOUND` / `Unknown projectKey: /nonexistent/pdrs-2090`。即 TS 侧在 provider 层**先解析项目、后判 `sessionKey`**（`createLocalGateway.ts:1117`/`:1130`/`:1143` 在 `:1118`/`:1131`/`:1144` 之前），与 R7 读码一致；R7 列为推断的「回退项目解析不了」这一支在真实 CLI 形状下确实发生。

`projectKey: null` 补测（`_tmp/20261107-issue-2090/ts-probe/probe-null.mjs`，同链路，原始输出 `probe-null.out`，20 条记录）：

| 方法 | 场景 A · 空白键 | 场景 A · 合法键 | 场景 B · 空白键 | 场景 B · 合法键 |
|---|---|---|---|---|
| `resume_session` | 成功，回显 `{"sessionKey":"   ","projectKey":null}` | 成功，回显入参 | 同 A | 同 A |
| `close_session` | `{"ok":true}` | `{"ok":true}` | 同 A | 同 A |
| `delete_session` | `INVALID_SESSION_KEY` | `SESSION_NOT_FOUND` | `PROJECT_NOT_FOUND` | `PROJECT_NOT_FOUND` |
| `rename_session` | `INVALID_SESSION_KEY` | `{"updated":true}` | `PROJECT_NOT_FOUND` | `PROJECT_NOT_FOUND` |
| `tag_session` | `INVALID_SESSION_KEY` | `{"updated":true}` | `PROJECT_NOT_FOUND` | `PROJECT_NOT_FOUND` |

即 TS 把 `null` 与缺键同等对待（`??` 是 nullish 回退，R7 `:50` 已读码，此处为实测）：项目可解析时空白键仍由 provider 的 `sessionKey` 门回答；`resume/close` 完全无视 `projectKey`。

## Rust 行为

请求入口 `Requests::request`（`native_process/requests.rs:285-320`）：`before_admission`（`:300-304`）→ 隔离项目准入（`:305-314`）→ `methods::request`（`:318`）→ `session_dir_request`（`session_dir.rs:98-117`）。方法内 `require_session_key`（`session_dir.rs:73`）在 delete `:231`、rename/tag `:300`，`resolve_project` 在其后（`:232` / `:308`）；resume `:179` / close `:214` 只过 `require_session_key`。

请求级实测（`_tmp/20261107-issue-2090/rust-probe/probe.out`，同一 fixture）：

| 方法 | 缺键 | 空白 | 非字符串 |
|---|---|---|---|
| 全部五个方法 | `INVALID_SESSION_KEY` / `sessionKey is required.` | 同左 | `gateway_request_failed` / `input.sessionKey?.trim is not a function` |

门序组合（空白 `sessionKey` + `projectKey:"/nonexistent/pdrs-2090"`）：`delete/rename/tag` 三个方法均 `gateway_request_failed` / `The isolated native process serves only its configured project.`——准入先于方法派发，故也先于 `require_session_key`。

`projectKey: null` 实测（`_tmp/20261107-issue-2090/rust-probe/null-probe.out`）：五个方法 ×（空白键 / 合法键）**十种**组合**全部** `gateway_request_failed` / `The isolated native process serves only its configured project.`，合法键也不例外——准入只放过 `undefined`（`requests.rs:305-314` 的 `.filter(|v| !v.is_undefined())`），`null` 落到「不是本进程服务的项目」那一支。

同一进程内对 `null` 的两种解释并存，这是本条登记的事实基础：准入（`requests.rs:305-314`）把 `null` 当显式项目并拒绝；`fill_default_project_key`（`native_archive.rs:44-56`，先把 nullish 换成配置项目）与 `resolve_project` 的 `unwrap_or_else`（`session_dir.rs:53`）都把 `null` 当「没给」用回退值。这个「两种口径」不是读码推断：把准入关掉（同一变异 `filter(|v| !v.is_undefined())` → `filter(|_| false)`）后重跑同一组请求，`_tmp/20261107-issue-2090/rust-negctl/null-fallback-under-mutated-admission.out` 记录到五方法 ×（空白键 / 合法键）变成——空白键全部 `INVALID_SESSION_KEY` / `sessionKey is required.`（回退到配置项目后由各自方法的 `require_session_key` 拦住，含不解析项目的 `resume` / `close`）；合法键 `resume` 回显 `{"sessionKey":"pdrs-2090-control"}`、`close` 返回 `undefined`、`delete` 答 `SESSION_NOT_FOUND`、`rename` / `tag` 答 `{"updated":true}`（`delete/rename/tag` 走到回退项目的会话查找，与场景 A 的 TS 结果同形；`resume` 只有「成功」这一点同形，回显字段仍少 `projectKey`（见本段末）；`close` 只剩前面记过的 `undefined` → `{"ok":true}` 包装差别，与准入无关）。即：**准入层是当前十格全部先报隔离项目错误的唯一原因**——`resolve_project` 与 `fill_default_project_key` 都把 null 当 nullish。关掉准入后剩下的差别不再是 null 特有的，而是本条别处已登记的那几条：`resume` / `close` 的空白键在 Rust 仍被 `require_session_key` 拒绝而 TS 成功（差异第 1 条）；Rust 的回退项目恒可解析，而 TS 场景 B 抛 `PROJECT_NOT_FOUND`（差异第 4 条）；`close` 成功值仍是 `undefined`（对 TS 的 `{ok:true}`）；`resume` 的回显只剩 `{sessionKey:…}`，TS 则原样回显含 `projectKey:null` 的入参（`null-fallback-under-mutated-admission.out` 的 `resume valid OK {sessionKey:pdrs-2090-control}` 对 `probe-null.out` 的 `{sessionKey:probe/session,projectKey:null}`；现状只在绕过准入时可见，记在此备查）。修 null 本身是对齐行为（改准入语义，影响所有方法），不在本 PR 范围。

与 TS 的差异收敛为五条：

1. `resume_session` / `close_session`：TS 三种输入全部成功（resume 回显入参，close `{"ok":true}`），Rust 三种输入全部拒绝（缺键 / 空白 → `INVALID_SESSION_KEY`；非字符串 → `gateway_request_failed` + 可选链报文）。**这两个方法上三种输入的行为全部不同**。
2. 项目可解析时的 `delete/rename/tag`：缺键 / 空白两侧同为 `INVALID_SESSION_KEY` / `sessionKey is required.`；非字符串两侧同为 `gateway_request_failed` / `input.sessionKey?.trim is not a function`。
3. 对一个**既不受本进程服务、在 TS 注册表里也解析不了**的路径（探测用 `/nonexistent/pdrs-2090`）——**这一条只在会解析项目的那三个方法（`delete/rename/tag`）上成立**：两侧都在 `sessionKey` 门之前拒绝，但码与报文不同——TS 对**未注册**的路径答 `PROJECT_NOT_FOUND` / `Unknown projectKey: …`，Rust 准入答 `gateway_request_failed` / `The isolated native process serves only its configured project.`。两点限定：① `resume_session` / `close_session` 在 TS 侧根本不读 `projectKey`（`InProcessGateway.ts:2026-2028` / `:2049-2054`），同一输入在 TS 上照常成功，因此这两格只有 Rust 侧的准入拒绝可记，没有「两侧都拒绝」可比较；② 两侧对「受服务」的定义不同（Rust：必须等于本进程配置项目；TS：注册表能解析），探测用的路径是特意选的「TS 侧未注册」路径，不能反过来推断「Rust 拒的项目在 TS 一定未注册」。这一条是全体方法共有的隔离准入形状，不是 `session_dir` 族特有；本条只登记它对本族首错顺序的影响，不重复登记准入本身。
4. 未给 `projectKey` 且回退项目解析不了时（场景 B）：TS 在 provider 层先抛 `PROJECT_NOT_FOUND`；Rust 的回退是**本进程的配置项目**（`resolve_project` 的 `unwrap_or_else`，`session_dir.rs:53`），恒被 `serves_project` 接受，所以同一输入在 `delete/rename/tag` 上先撞 `require_session_key`——**按输入分两种答案**：缺键 / 空白键 → `INVALID_SESSION_KEY` / `sessionKey is required.`；非字符串（`42`）→ `gateway_request_failed` / `input.sessionKey?.trim is not a function`（与场景 A 相同，`probe.out` 有记录）。TS 对这三种输入在场景 B 全是 `PROJECT_NOT_FOUND`，所以这一格两侧连**首错位置**都不同（TS 项目门在前，Rust sessionKey 门在前）。
5. `projectKey: null`：TS 当 nullish（回退到 `fallbackProjectRoot`），空白键的答案分两段：场景 A 回退项目可解析，落到 provider 的 `sessionKey` 门 → `INVALID_SESSION_KEY`；场景 B 回退项目解析不了，在项目解析那一层就抛 `PROJECT_NOT_FOUND`，**没有走到** `sessionKey` 门；合法键则按方法各自的结果（`delete` 场景 A `SESSION_NOT_FOUND` / 场景 B `PROJECT_NOT_FOUND`，`rename`/`tag` 场景 A `{"updated":true}` / 场景 B `PROJECT_NOT_FOUND`），`resume`/`close` 两场景都成功且不看 `projectKey`。Rust 准先把 null 当显式项目拒绝，**五个方法、连合法键都答** `gateway_request_failed` / 隔离项目报文。这一格在 R7 里只有 TS 侧描述，Rust 侧当时按守卫级推断成「恒 `INVALID_SESSION_KEY`」（见「类别」第 4 条）。

## 范围

覆盖（按实际测到的，不外推）：

- TS 真实链路，场景 A（`pilotHome == projectRoot`，回退项目可解析）：五个方法 × 三种输入；外加五个方法 × `projectKey: null` ×（空白键 / 合法键）。
- TS 真实链路，场景 B（`pilotHome != projectRoot`，回退项目未注册）：`delete/rename/tag` × 三种输入，「空白 `sessionKey` + 显式 `projectKey:"/nonexistent/pdrs-2090"`」，以及 `delete/rename/tag` × `projectKey: null` ×（空白键 / 合法键）。`resume_session` / `close_session` 不解析项目（`InProcessGateway.ts:2026-2028` / `:2049-2054`），启动形状对它们不适用，场景 B 下只实测了 `projectKey: null` 两格并确认与场景 A 同结果。
- Rust 请求级（`rust-probe/probe.out`）：一个隔离进程 fixture（配置项目可服务）× 五个方法 × 三种输入；外加 `delete/rename/tag` 的「空白键 + 显式不可解析 `projectKey`」三格。`projectKey: null` 的十格见 `rust-probe/null-probe.out`。**「不受服务项目 + 合法键」这一格不在 `probe.out` 里**——它由入库用例钉住（`gate-order-tests.out` 的绿跑与 `mutant-run-r2.out` 的红跑都是这份用例的证据）；「准入关掉后 null 走回退」的十格见 `rust-negctl/null-fallback-under-mutated-admission.out`。Rust 侧没有「回退项目解析不了」这一形状可测——它的回退恒为配置项目（差异第 4 条）。
- 「非 nullish 非字符串」以一个代表值（数字 `42`）覆盖，不是全类型枚举。
- 入库用例（`session_dir.rs`）每个方法钉六格：缺键 / 空白（无 `projectKey`）→ `INVALID_SESSION_KEY`；显式不受服务 `projectKey` + 空白键、显式不受服务 `projectKey` + 合法键（对照）、`projectKey: null` + 空白键、`projectKey: null` + 合法键 → 同一条准入错误。

不覆盖（仍在其他条目）：`projectKey: ""`（显式空串，不是 nullish）——两侧都会按 cwd 解析（TS `projectKey ?? fallback` 保留显式空串、`resolve("")` = cwd；Rust 准入 `as_string()` 得 `Some("")`、`serves_project` 同样按 cwd 解析），是否放行到 `sessionKey` 门随 fixture cwd 变化，本轮未测，并入 `#2199` 一起测；同族的**相对路径** `projectKey`（`"."` / `"foo"` 这类非空非绝对）也一样按进程 cwd 解析（Rust `resolve_process_path`、TS `resolve`），行为随 cwd 漂移，同样未测，一并并入 `#2199`；`A7.2-NATIVE-ARCHIVE-CWD-001` 只登记过 archive RPC 的同一语义并明示不扩大到其他 RPC。`params` 整体为 `null`（R7 已登记）、`projectKey` 孤立代理项（#2083）、U+0085 空白集合（#2097）、`projectKey` 非字符串的 Node `ERR_INVALID_ARG_TYPE`（R7 已登记）、`deleteEphemeralSession` 钩子命中路径（R7 已标未测，本探针的空白键未命中该钩子，原始输出里走的是 provider 门）。`list_sessions` / `close_project_sessions` 不调用 `require_session_key`（`session_dir.rs` 的四处调用点只覆盖另外五个方法），不属于本族 `sessionKey` 门的比较范围，本轮不加用例。**`config.rs` 族明示不覆盖**：该族有自己的 `require_session_key`（`config.rs:75`）与 `resolve_project`（`config.rs:143-152`），同样过 `requests.rs:305-314` 准入。逐方法事实（读码）：`require_session_key` 有 6 个调用点（`:229` `session_model_set_reserved`、`:269` `session_model_clear_reserved`、`:426` `set_permission_mode`、`:441` `set_session_thinking`、`:506` `set_output_style`、`:613` `seed_read_state`；`:70` 注释里的「7 个方法 5 个共享」已与代码不符）；`resolve_project` 只有 3 处（`:210`、`:268`、`:467`），其中**在 `sessionKey` 门之前**的只有 `session_model_set_reserved`（`:210` → `:229`）与 `session_model_clear_reserved`（`:268` → `:269`）两个方法，`output_styles_list`（`:467`）只解析项目、不判 `sessionKey`。因此本条登记的差异 3 / 5 至多对这 6 个方法里的那两处门序成立，其余要各自实测；本轮没有为 `config` 族补请求级门序用例，已开 `P2-跟进` `#2199`（前置于 `#2090`）跟踪，不在本 PR 扩大范围。

本轮不做「对齐 TS」：放开 `resume/close` 的 `sessionKey` 门、或把准入改成把 `null` 当 nullish，都属判分/安全语义变更，`#2090` 跟踪人批；本 PR 只补请求级用例与登记，不改 `session_dir.rs` / `requests.rs` 的任何产品逻辑。

## 机器判据

```
$ cd resources/PilotDeck && node_modules/.bin/tsx ../../_tmp/20261107-issue-2090/ts-probe/probe.mjs
# 47 条记录；节选（见 _tmp/20261107-issue-2090/ts-probe/probe.out 全文）
{"node":"v22.22.0","icu":"78.2"}
{"scenario":"A","layer":"wire","method":"resume_session","input":"blank","ok":true,"result":"{\"sessionKey\":\"   \"}"}
{"scenario":"A","layer":"wire","method":"close_session","input":"nonstring","ok":true,"result":"{\"ok\":true}"}
{"scenario":"A","layer":"wire","method":"delete_session","input":"blank","ok":false,"error":{"code":"INVALID_SESSION_KEY","message":"sessionKey is required."}}
{"scenario":"A","layer":"wire","method":"tag_session","input":"nonstring","ok":false,"error":{"code":"gateway_request_failed","message":"input.sessionKey?.trim is not a function"}}
{"scenario":"B","layer":"wire","method":"rename_session","input":"missing","ok":false,"error":{"code":"PROJECT_NOT_FOUND","message":"Unknown projectKey: /var/folders/…/pdrs-2090-B-project-JSDoxF"}}
{"scenario":"B","layer":"wire","method":"delete_session","input":"blank+unresolvable-project","ok":false,"error":{"code":"PROJECT_NOT_FOUND","message":"Unknown projectKey: /nonexistent/pdrs-2090"}}

$ cd resources/PilotDeck && node_modules/.bin/tsx ../../_tmp/20261107-issue-2090/ts-probe/probe-null.mjs
# 20 条记录；节选（见 _tmp/20261107-issue-2090/ts-probe/probe-null.out 全文）
{"scenario":"A","layer":"wire","method":"delete_session","input":"projectKey=null blank","ok":false,"error":{"code":"INVALID_SESSION_KEY","message":"sessionKey is required."}}
{"scenario":"A","layer":"wire","method":"resume_session","input":"projectKey=null blank","ok":true,"result":"{\"sessionKey\":\"   \",\"projectKey\":null}"}
{"scenario":"B","layer":"wire","method":"delete_session","input":"projectKey=null blank","ok":false,"error":{"code":"PROJECT_NOT_FOUND","message":"Unknown projectKey: /var/folders/…/pdrs-2090n-B-project-H64Kne"}}
```

```
$ ssh cb8 'cd ~/ws/pdrs-2090-src/src && ulimit -n 4096 && LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8 \
    CARGO_TARGET_DIR=$HOME/ws/pdrs-2020-2035-target bash -c \
    "touch pilotdeck-gateway/src/native_process/methods/session_dir.rs && ~/.cargo/bin/cargo test --offline \
     -p pilotdeck-gateway --lib native_process::methods::session_dir::tests"'
   Compiling pilotdeck-gateway v0.1.0 (/home/clawbox-08/ws/pdrs-2090-src/src/pilotdeck-gateway)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 44.65s
running 17 tests
test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 5070 filtered out
```

全文见 `_tmp/20261107-issue-2090/rust-probe/gate-order-tests-r3.out`（当前用例体的绿跑，含 `Compiling` 行）。**命令里的 `touch` 不能省**：`rsync -a` 保留源 mtime，cargo 会沿用旧制品——本条目早先一份不带 `touch` 的绿跑（`gate-order-tests.out`）正因为此没有 `Compiling` 行，无法证明跑的是当时最新的用例体，已被本份取代。

新增请求级用例（`session_dir.rs`）：`resume_session_blank_key_gate_order`（`:952`）、`close_session_blank_key_gate_order`（`:957`）、`delete_session_blank_key_gate_order`（`:962`）、`rename_session_blank_key_gate_order`（`:967`）、`tag_session_blank_key_gate_order`（`:972`），共用 `blank_key_gate_order`（`:866`）与两条报文常量（`:862-864`）。每个方法钉六格：无 `projectKey` 时缺键 / 空白键 → `INVALID_SESSION_KEY` / `sessionKey is required.`；显式不受服务 `projectKey` + 空白键、显式不受服务 `projectKey` + 合法键、`projectKey: null` + 空白键、`projectKey: null` + 合法键 → **同一条**准入错误（后四格里两个合法键格是对照：证明这一步由准入决定而非 `sessionKey`，同时把 null 格的现状钉住）。

判别力负控（`_tmp/20261107-issue-2090/rust-negctl/`，命令见其 `commands.txt`）：在 cb8 副本上把准入改成永不拒绝（`filter(|v| !v.is_undefined())` → `filter(|_| false)`，变异体 sha256 `0136be7d…`），`mutant-run-r2.out` 为 `12 passed; 5 failed`，五条用例全红且都是 `left: "INVALID_SESSION_KEY" right: "gateway_request_failed"`（该红跑基于 r2 版用例体：panic 落在 `session_dir.rs:924`，当前体同一断言在 `:935`；第一格断言形态未变，机制结论不受影响）。**这份负控的证明范围要写清**：用例是顺序断言的，准入一关就停在第一步（「空白键 + 不受服务项目」），所以它直接证明的是第一步有判别力；第二、三步（合法键对照、null 格）没有被这一步单独证明——null 格的定向证据是 `null-fallback-under-mutated-admission.out`（同一变异下 null 的答案从准入错误变成 `INVALID_SESSION_KEY` / 方法各自的成功值），合法键对照只有读码支持：准入关掉后 `delete/rename/tag` 会走 `resolve_project`、不再答准入错误，`resume/close` 根本不过项目解析、会直接成功——两者都会让该格断言变红，但没有单独跑这条定向变异。还原后 `requests.rs` sha256 回到 `cdfa0b99…`，`restore-r2.out` 为 `17 passed`。还原后必须先 `touch` 源文件再跑：`rsync -a` 保留源 mtime，cargo 会认为制品仍是新的（第一次还原跑出 `Finished in 0.08s`，仍是变异体二进制，5 红）。

```
$ python3 -I scripts/gateway-parity/check_frozen.py --base origin/main
{"status": "PASS", "failures": [], "entries": 30}
```

## 批准记录

未批准，非已接受差异。2026-11-07 `#2090` 车道由 AI 代登记（更正 R7 的请求级推断并补齐其实测缺口）；不新增判分豁免，不改任何已有条目字节。待批事项两件，都属行为/判分语义变更，需人批（`#2090`）：① `resume_session` / `close_session` 上 Rust 多出的 `sessionKey` 门——保持（现状，Rust 更严）还是对齐 TS 放开；② 隔离项目准入对 `projectKey: null` 的处理——保持（现状，五个方法都拒）还是按 nullish 回退对齐 TS（并与 `resolve_project` / `fill_default_project_key` 的口径统一）。
