---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R9
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

登记 + 补齐 R7 明示未闭合的两块（端到端实测、请求级门序用例）。不改产品行为，不新增豁免。两处补齐 + 一处更正：

1. **R7 标为「属读码推断，未实测」的回退项目分支，本轮实测落在「解析不了」那一支。** 真实 CLI 形状 `createLocalGateway({ projectRoot, pilotHome })`（`pilotHome` 与 `projectRoot` 不同、`projectRoot` 未注册）下，`delete/rename/tag` 对三种 `sessionKey` 输入（缺键 / 空白 / 非字符串）的**首错全是 `PROJECT_NOT_FOUND`**（`projectRegistry.ts:50`），不是 `INVALID_SESSION_KEY`，也不是 provider 门上的 `TypeError`。R7 的两分支表已列出这一支，本条只是把它的「未实测」补上，不是推翻 R7 的结论。
2. **R7 预写的请求级期望「空白 `sessionKey` + 不可解析 `projectKey` → `INVALID_SESSION_KEY` 先于 `CAPABILITY_UNAVAILABLE`」不成立。** Rust 请求入口 `native_process/requests.rs:305-314` 在方法派发**之前**先做隔离项目准入：`projectKey` 显式存在且不是本进程配置项目 → `gateway_request_failed` + `The isolated native process serves only its configured project.`。因此该组合的首错是准入错误；`resolve_project` 的 `CAPABILITY_UNAVAILABLE`（`session_dir.rs:54-56`）在该入口下**不是客户端能看到的第一个码**。
3. R7 只有表达式级探针的两处，现在是真实链路实测：`resume_session` / `close_session` 在 TS 全链无 `?.trim()` 门（三种输入全部成功），`delete/rename/tag` 在项目可解析时答 `INVALID_SESSION_KEY` / 非字符串可选链 `TypeError`。

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

## Rust 行为

请求入口 `Requests::request`（`native_process/requests.rs:285-320`）：`before_admission`（`:300-304`）→ 隔离项目准入（`:305-314`）→ `methods::request`（`:318`）→ `session_dir_request`（`session_dir.rs:98-117`）。方法内 `require_session_key`（`session_dir.rs:73`）在 delete `:231`、rename/tag `:300`，`resolve_project` 在其后（`:232` / `:308`）；resume `:179` / close `:214` 只过 `require_session_key`。

请求级实测（`_tmp/20261107-issue-2090/rust-probe/probe.out`，同一 fixture）：

| 方法 | 缺键 | 空白 | 非字符串 |
|---|---|---|---|
| 全部五个方法 | `INVALID_SESSION_KEY` / `sessionKey is required.` | 同左 | `gateway_request_failed` / `input.sessionKey?.trim is not a function` |

门序组合（空白 `sessionKey` + `projectKey:"/nonexistent/pdrs-2090"`）：`delete/rename/tag` 三个方法均 `gateway_request_failed` / `The isolated native process serves only its configured project.`——准入先于方法派发，故也先于 `require_session_key`。

与 TS 的差异收敛为四条：

1. `resume_session` / `close_session`：TS 三种输入全部成功（resume 回显入参，close `{"ok":true}`），Rust 三种输入全部拒绝（缺键 / 空白 → `INVALID_SESSION_KEY`；非字符串 → `gateway_request_failed` + 可选链报文）。**这两个方法上三种输入的行为全部不同**。
2. 项目可解析时的 `delete/rename/tag`：缺键 / 空白两侧同为 `INVALID_SESSION_KEY` / `sessionKey is required.`；非字符串两侧同为 `gateway_request_failed` / `input.sessionKey?.trim is not a function`。
3. 显式 `projectKey` 不受本进程服务时：两侧都在 `sessionKey` 门之前拒绝，但码与报文不同——TS `PROJECT_NOT_FOUND` / `Unknown projectKey: …`，Rust 准入 `gateway_request_failed` / `The isolated native process serves only its configured project.`。这一条是全体方法共有的隔离准入形状，不是 `session_dir` 族特有；本条只登记它对本族首错顺序的影响，不重复登记准入本身。
4. 未给 `projectKey` 且回退项目解析不了时（场景 B）：TS 在 provider 层先抛 `PROJECT_NOT_FOUND`；Rust 的回退是**本进程的配置项目**（`resolve_project` 的 `unwrap_or_else`，`session_dir.rs:53`），恒被 `serves_project` 接受，所以同一输入在 `delete/rename/tag` 上先撞 `require_session_key`，答 `INVALID_SESSION_KEY`。这一格两侧连**首错位置**都不同（TS 项目门在前，Rust sessionKey 门在前）。

## 范围

覆盖（按实际测到的，不外推）：

- TS 真实链路，场景 A（`pilotHome == projectRoot`，回退项目可解析）：五个方法 × 三种输入。
- TS 真实链路，场景 B（`pilotHome != projectRoot`，回退项目未注册）：只测 `delete/rename/tag` × 三种输入，外加「空白 `sessionKey` + 显式 `projectKey:"/nonexistent/pdrs-2090"`」。`resume_session` / `close_session` 不解析项目（`InProcessGateway.ts:2026-2028` / `:2049-2054`），启动形状对它们不适用，未测。
- Rust 请求级：一个隔离进程 fixture（配置项目可服务）× 五个方法 × 三种输入，外加 `delete/rename/tag` 的门序组合。Rust 侧没有「回退项目解析不了」这一形状可测——它的回退恒为配置项目（差异第 4 条）。
- 「非 nullish 非字符串」以一个代表值（数字 `42`）覆盖，不是全类型枚举。

不覆盖（仍在其他条目）：`params` 整体为 `null`（R7 已登记）、`projectKey` 孤立代理项（#2083）、U+0085 空白集合（#2097）、`projectKey` 非字符串的 Node `ERR_INVALID_ARG_TYPE`（R7 已登记）、`deleteEphemeralSession` 钩子命中路径（R7 已标未测，本探针的空白键未命中该钩子，原始输出里走的是 provider 门）。

本轮不做「对齐 TS」：放开 `resume/close` 的 `sessionKey` 门属判分/安全语义变更，`#2090` 跟踪人批；本 PR 只补请求级用例与登记，不改 `session_dir.rs` 的任何产品逻辑。

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
```

```
$ ssh cb8 'cd ~/ws/pdrs-2090-src/src && ulimit -n 4096 && LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8 \
    CARGO_TARGET_DIR=$HOME/ws/pdrs-2020-2035-target ~/.cargo/bin/cargo test --offline \
    -p pilotdeck-gateway --lib native_process::methods::session_dir::tests'
test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 5070 filtered out
```

新增请求级用例（`session_dir.rs`）：`delete_session_blank_key_gate_order`（`:935`）、`rename_session_blank_key_gate_order`（`:940`）、`tag_session_blank_key_gate_order`（`:945`），共用 `blank_key_gate_order`（`:860`）。每条钉三件事：无 `projectKey` 时空白键 → `INVALID_SESSION_KEY` / `sessionKey is required.`；显式不可解析 `projectKey` + 空白键 → 准入错误 `gateway_request_failed` / 隔离项目报文；同项目 + 合法键 → **同一条**准入错误（对照，证明第二步由准入决定而非 `sessionKey`）。负控锚点只扫既有 rows，不引用新用例，无需新增变异。

```
$ python3 -I scripts/gateway-parity/check_frozen.py --base origin/main
{"status": "PASS", "failures": [], "entries": 31}
```

## 批准记录

未批准，非已接受差异。2026-11-07 `#2090` 车道由 AI 代登记（更正 R7 的两处推断并补齐其实测缺口）；不新增判分豁免，不改任何已有条目字节。待批事项：`resume_session` / `close_session` 上 Rust 多出的 `sessionKey` 门——保持（现状，Rust 更严）还是对齐 TS 放开，属语义变更，需人批（`#2090`）。
