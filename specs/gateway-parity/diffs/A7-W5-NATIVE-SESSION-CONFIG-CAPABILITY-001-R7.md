---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R7
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

更正登记（审计事实更正 + 补齐固定基线实测）。更正 -R6 的三处错误陈述（`:8`、`:33`、`:12`），并把 -R6 标为「本轮不可读」的固定基线实测补上。-**R6 正文保持原样**（只追加规则），审计以本条为准。

1. **-R6:8 与 -R6:33 对迁移前状态的描述是错的。** -R6 称 -R2 记录的是「`tools.rs` 的 `required_trim` 与 `session_dir.rs` 的门仍是 `to_utf8` + Rust `str::trim` 的旧形态」，并在三态表里写「session_dir 旧门 `to_utf8` 过滤 → 拒」。实际 -R2:29 写的是**相反**的事实：`session_dir` 的 `require_session_key` 早在 #1455 就已改为共享的 lossy `?.trim()` 实现、**放行**孤立代理项，只有 `tools.rs` 仍留着 `to_utf8` + Rust `str::trim`。因此 **#1604 没有改变 `sessionKey` 孤立代理项的判定**（迁移前后都放行）；该族唯一的行为改变是非字符串从 `INVALID_SESSION_KEY` 改答可选链 TypeError。
2. **-R6:12 说固定基线「本仓外 checkout，本轮不可读」也不成立。** 固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 就在本仓 `resources/PilotDeck`（`git rev-parse HEAD` 实测同值），本轮已用它跑了只读探针。

## TS 依据

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，路径 `resources/PilotDeck`（只读）。直接相关的两处源码：

- `src/gateway/client/InProcessGateway.ts:2037`：`if (!input.projectKey?.trim()) throw new Error("projectKey is required.");`（`closeProjectSessions`）。
- `src/gateway/client/InProcessGateway.ts:2049`：`closeSession` 与 `resumeSession`（:2026）、`deleteSession`（:2229）、`renameSession`（:2211）、`tagSession`（:2220）在**这一层**都**没有** `?.trim()` 门——但两者去向不同：`resumeSession` **原样返回 `input`**（`:2026-2028`，不转发），`closeSession` 把 `input.sessionKey` 透传给 `router.close` 再做三项清理（`:2049-2054`），`delete/rename/tag` 转发给 CLI provider 层。**端到端不等于「TS 全链不拒」**：`delete/rename/tag` 的 provider 门会拒空白 sessionKey，答同一个 `DialogGatewayError("INVALID_SESSION_KEY", "sessionKey is required.")`（`src/cli/createLocalGateway.ts:1144` / `:1118` / `:1131`；`specs/gateway-coverage/ledger.json:4111` / `:4335` / `:4406` 同锚），但该门**不是链上第一步**（见「仍未闭合」段）。**全链没有任何 `INVALID_SESSION_KEY` 拒绝的只有 `resumeSession` 与 `closeSession`。**

只读探针（Node v22.22.0 / ICU 78.2，脚本与原始输出见 `_tmp/20261107-p2sweep/ts-probe-projectkey/`）：门与 `throw` 语句**照抄基线源码行**（`InProcessGateway.ts:2037` / `:2286`），并 `import` 基线**自己的错误类** `src/gateway/dialog/errors.ts` 的 `DialogGatewayError`——所以下表是**真实抛出的错误对象**的 `name` / `code` / `message`，不是手写标签。

| 输入 | TS `!input.projectKey?.trim()`（真实抛出的对象） |
|---|---|
| 键不存在 / `undefined` / `null` | `Error` · `projectKey is required.` |
| 数字 `7` | `TypeError` · `input.projectKey?.trim is not a function` |
| 全是空格 / 只有一个 U+FEFF | `Error` · `projectKey is required.` |
| 孤立代理项 `U+D800` | **通过**（门之后的 `path.resolve` 也不抛；再往后的 router 行为本探针**未覆盖**） |
| `params` 整体为 `null` | `TypeError` · `Cannot read properties of null (reading 'projectKey')` |

同一表达式形状的 `sessionKey` 门（例如 `InProcessGateway.ts:2286` 的 `setMcpServers`；session_dir 族在 IPG 层没有此门，见上）探针结果同形状：缺键 / `null` / 空白 / U+FEFF → `DialogGatewayError`，`code: INVALID_SESSION_KEY` + `message: sessionKey is required.`；非 nullish 的非字符串（探针用数字 `7`）→ `TypeError: input.sessionKey?.trim is not a function`；孤立代理项**通过**。

## Rust 行为

- `sessionKey` 一族：`require_session_key` 在 #1604 前（`origin/main` 的 `session_dir.rs:69-76`）就是 `.and_then(JsValue::as_string).cloned().filter(super::config::trimmed_string_is_non_empty)`，而 `config.rs:66-68` 的 `trimmed_string_is_non_empty` 用 `String::from_utf16_lossy` + `js_trim` —— 孤立代理项**放行**。`origin/main` 的既有用例 `require_session_key_keeps_lone_surrogates_and_rejects_blank`（`session_dir.rs:790`）就断言 `is_ok() == true`。#1604 迁移后仍是放行（`trim_guard` 同样走 `from_utf16_lossy`），**行为未变**；变的是非字符串：`INVALID_SESSION_KEY` → `gateway_request_failed` + `input.sessionKey?.trim is not a function`。
- `tools.rs`：旧 `required_trimmed`（`origin/main` 的 `tools.rs:54-64`）是 `string(params, field)?` + `s.to_utf8().map(|u| u.trim().is_empty()).unwrap_or(false)`。`to_utf8()` 失败时 `unwrap_or(false)` 直接放行，所以孤立代理项迁移前后都放行。**空白集合两端都翻，不是一个字符**：旧实现按 Rust `str::trim`（`char::is_whitespace`），新实现按 `js_trim` / `js_space`（`metadata.rs:146-152`，ECMAScript WhiteSpace）——两者在 U+FEFF 与 U+0085 上方向相反，实测见机器判据：
  - **U+FEFF（BOM）**：Rust 不剥 → 旧判「非空白」**放行**；`js_space` 含 BOM → 新判空白**拒**（放行 → 拒）。
  - **U+0085（NEL）**：Rust 剥 → 旧判空白**拒**；`js_space` 不含 NEL → 新判「非空白」**放行**（拒 → 放行）。
  -R2:33 已预告这一对分叉（「JS WhiteSpace 集合与 Rust `trim` 在 U+FEFF / U+0085 上分叉」），-R6:32 只记了 U+FEFF 一侧，本条补齐另一侧。
- `close_project_sessions` 的 `projectKey`：非字符串由 `require_text` 改答可选链 TypeError；缺键/空白/非 UTF-8 仍答 `gateway_request_failed` + `projectKey is required.`（`session_dir.rs:436-441` 的显式 `to_utf8` 门）。**迁移前后一致**（`origin/main` 的 `session_dir.rs:421-426` 用 `to_utf8().ok()`）。

**残留差异（本条新登记，未批准）**：孤立代理项的 `projectKey` —— TS 通过门并继续执行；**门之后 TS 到底成功还是失败，本探针没有实测**（只证到 `path.resolve` 不抛，`router.closeProject` / `resumeProject` 的行为未覆盖），所以这里只登记**门这一层的差异**，不推断最终结果。Rust 则在门口直接答 `gateway_request_failed` / `projectKey is required.`。这是 #1604 之前就有的差异（`origin/main` 的 `to_utf8().ok()` 即如此），本 PR 未引入也未改变，但此前账本没有单独条目。要不要对齐见上文的范围与 `-R7` 批准记录。

## 范围

只更正 -R6 的三处陈述、补 `projectKey` 门的固定基线实测，并登记两条残留差异。**不新增豁免、不改判分语义、不动 -R6 与更早条目的字节。**

**仍未闭合（如实登记，不用跟进单冒充验收证据）**：

- `session_dir` 族 `sessionKey` 的**门位置/形状**（本条新登记，未批准）：TS 基线在 **IPG 层**对这五个方法都没有 `?.trim()` 门（`InProcessGateway.ts:2026`、`:2049`、`:2229`、`:2211`、`:2220`），**但端到端必须逐方法分开说**：
  - `renameSession` / `tagSession` / `deleteSession`：IPG 只是转发（`this.options.renameSession(input)` `InProcessGateway.ts:2211-2218`、`this.options.tagSession(input)` `:2220-2227`、`await this.options.deleteSession(input)` `:2229-2239`；`deleteSession` 之前还先试 `deleteEphemeralSession` 钩子，`:2230` —— 空白键在该钩子里命中与否本探针**未测**）。转发到的 CLI provider 门**不是链上第一步**：`resolveProjectKey(input.projectKey ?? fallbackProjectRoot)` 先跑（`createLocalGateway.ts:1117` / `:1130` / `:1143`），**只有项目能解析时**才轮到 `if (!input.sessionKey?.trim()) throw new DialogGatewayError("INVALID_SESSION_KEY", "sessionKey is required.")`（`:1118` / `:1131` / `:1144`，与 `ledger.json:4111` / `:4335` / `:4406` 同锚）。注意 `??` 是 **nullish 回退**，不是「必须是字符串」：`projectKey` 缺失 / `undefined` / `null` 都会换成 `fallbackProjectRoot` 再解析。

    所以「同码同报文」不能概括成「projectKey 是能解析的字符串」一条，必须按**两个字段的取值**分。下表**只覆盖 `sessionKey` 缺键 / `null` / `undefined` / 空白**——这四种在 TS 侧都短路到同一个 `INVALID_SESSION_KEY` 领域错误（探针 sessionKey 分节前三行 + `blank '  '` 行），Rust 侧都归 `TrimGuard::Missing`；非 nullish 的非字符串 `sessionKey` 正交，另列在表后。Rust 侧这一族先跑 `require_session_key`（定义在 `session_dir.rs:73-80`；**四处**调用点覆盖五个方法：resume `:179`、close `:214`、delete `:231`、rename/tag 共用 `:300`。其中只有 delete `:232` 与 rename/tag `:308` 各有 `resolve_project`，且都在守门**之后**；resume / close 方法体里没有项目解析步，门序比较对它们不适用），故这一路径上的基准答案恒为 `INVALID_SESSION_KEY` / `sessionKey is required.`。请求级用例只有 resume 一条（`resume_session_rejects_blank_session_key`，`session_dir.rs:848-862`）；delete/rename/tag 与「门序」组合都没有请求级用例（见文末 #2090）：

    | `projectKey` | TS 在 sessionKey 门之前发生什么 | 首错是否与 Rust 相同 |
    |---|---|---|
    | 缺失 / `undefined` / `null` | 按上文的 nullish 回退换成 `fallbackProjectRoot` 再解析；回退项目**能解析时**照常到达 sessionKey 门 | **按回退项目能否解析分支**：能解析 → **相同**；解析不了（显式配了一个既不是 `generalProjectKey` 也没注册的回退路径）→ 先抛 `PROJECT_NOT_FOUND`（`projectRegistry.ts:50`），**不同**。「回退项目恒可解析」属读码推断，**未实测**；启动归一化只在**类型上**保证它是字符串（TS 类型约束，不是运行时校验——JS 调用方传一个非 nullish 的非字符串时 `??` 原样保留，后面的 `path.resolve` 会先抛类型错误）——`LocalGatewayBootConfig.ts:28` 声明为 `string`、`:52` `input.fallbackProjectRoot ?? projectRoot`——所以真实调用链上不会出现 `undefined`，但会出现「未注册的字符串」 |
    | 能解析的字符串（`generalProjectKey` 或已注册项目） | 到达 sessionKey 门 | 相同（`deleteSession` 例外：它先试 `deleteEphemeralSession` 钩子，钩子命中时根本到不了门——本探针**未测**；该钩子只挂在 `createLocalGateway` 构造的 IPG 上，见下） |
    | 字符串但解析不了（未注册的应用内路径） | 先抛 `PROJECT_NOT_FOUND`（`projectRegistry.ts:50`） | 不同 |
    | 非 nullish 的非字符串（如数字 `7`） | `resolve(projectKey)`（`projectRegistry.ts:42`）由 Node 参数校验先抛 `TypeError` / `ERR_INVALID_ARG_TYPE`（实测报文 `The "paths[0]" argument must be of type string. Received type number (7)`） | 不同 |
    | 空串 `""`（不是「空白串」；真·空白 projectKey 如 `" "` 经 `resolve(" ")` 得 `cwd + "/ "`，实测 `/Users/da/ws/pilotdeck-rs/ `，几乎必然落「解析不了」行） | `""` 不是 nullish，`resolve("")` = **cwd**（实测 `/Users/da/ws/pilotdeck-rs`）；cwd 命中 `generalProjectKey` 或已注册项目时到达 sessionKey 门（相同），否则 `PROJECT_NOT_FOUND`（不同）。**两种分支本探针都未测** | **按解析结果分支，未实测** |

    **非 nullish 的非字符串 `sessionKey`（单列）**：`null` / `undefined` / 缺键**不属于这一类**——Rust 的 `trim_guard` 把它们归 `TrimGuard::Missing`（`methods/mod.rs:144`），答的仍是 `INVALID_SESSION_KEY` / `sessionKey is required.`；TS 的可选链同样短路到领域错误，与上表 `sessionKey` 那一维同结论。真正不同的是非 nullish 的非字符串（如数字 `7`）：Rust 走 `NotAString`，答 `gateway_request_failed` + `input.sessionKey?.trim is not a function`（`methods/mod.rs:175-177`；本 PR 用例 `a_non_string_session_key_throws_the_optional_chain_type_error` 在 `session_dir.rs:882-890` 断言了这两个值）。

    **分层说，别把两层混成一层**：TS 在 **provider 层**抛的是真 `TypeError`（探针 `number 7` 行——探针跑的是**照录的门表达式副本**，不是直接调用基线 provider 方法，所以它只证明该表达式的抛错形状，不证明 provider 调用链一定走到这里）。过了同一条 WebSocket 边界，`GatewayWsConnection.ts:245-255` 会把裸 `TypeError` 改写成 `code: "gateway_request_failed"` + 原 `message`；能保留 `error.code` 的是更早的三个分支——`SkillValidationError`（`:194-208`）、`SkillManagerError`（`:209-222`）、`DialogGatewayError` / 带 structured code（`:223-243`）。所以**对这条裸 TypeError 而言，传输边界上两侧同形状**：`gateway_request_failed` + 同一条报文（同一兜底语义在 `ledger.json:2147` / `:2420` / `:2552` 一族已有登记）。差别只在 provider 层：TS 内部是 `TypeError` 实例，Rust 是普通 `RequestError`。跨边界端到端等价属**读码推断，未实测**。

    `resume` / `close` 不在这条路径上，见下两条。

    **`params` 整体为 `null` / `undefined`（本条新登记，按方法分开）**：**rename / tag** 上 TS 先求值 `input.projectKey`（`createLocalGateway.ts:1130` / `:1117` 的实参），报 `TypeError: Cannot read properties of null (reading 'projectKey')`（表达式级实测，见机器判据）；Rust 的 `require_session_key` 先跑，`input_property`（`methods/mod.rs:121-126`）读 `sessionKey`，报 `…(reading 'sessionKey')`（`session_dir.rs:892-903` 钉的是这道守卫本身），**两侧键名不同**。**delete 不同（限挂了该钩子的 `createLocalGateway` 链）**：钩子是**条件调用**——`if (this.options.deleteEphemeralSession && await …)`（`InProcessGateway.ts:2230`），而 option 本身可选（`:312`）。基线有两个 IPG 构造点：`createLocalGateway.ts:1021` 挂了钩子（实现 `:1417`），`createGateway`（`Gateway.ts:45-72`）两个 delete option **都不挂**，`deleteSession(null)` 的首错是 `CAPABILITY_UNAVAILABLE`（`InProcessGateway.ts:2234-2236`）——两种都不是 `projectKey`。在 `createLocalGateway` 链上，钩子实现是 `(input) => registry.deleteEphemeralSession(input.sessionKey)`（`createLocalGateway.ts:1417`）：`input` 为 `null` 时它先读 `input.sessionKey`，所以首错键名**与 Rust 相同**（**读码推断，未实测**；端到端未经探针覆盖）。`session_dir.rs:892-893` 的注释只描述守门自身的形状，不是 TS 端到端首错的断言。

    **`resume` / `close` 的 `params=null` 单列（不在上表）**：`resumeSession` 原样 `return input`（`InProcessGateway.ts:2026-2028`），TS 这一层不回错；Rust `session_resume`（`session_dir.rs:179`）先跑守卫 → `(reading 'sessionKey')` **拒绝**——家族里**结果类别**差异最大的一格。`closeSession` 求值实参 `this.router.close(input.sessionKey)`（`:2049-2050`）时即抛 `(reading 'sessionKey')`，与 Rust `session_close`（`:214`）**同键名**。两格都**缺请求级端到端用例**：TS 半边是**表达式级实测**（见机器判据的 `resume(null)` / `close(null)` 两行，照录方法体与实参求值，不是端到端）；Rust 半边已被守卫级用例逐字钉死（`session_dir.rs:894-903` 的 `a_null_params_object_reports_the_optional_chain_read_error` 断言 `gateway_request_failed` / `Cannot read properties of null (reading 'sessionKey')`），只是没有请求级用例钉住「派发到守卫」这一步（见 #2090）。

    与 Rust 的差别是**门的层、时机与前置步骤**——不能写成「TS 不拒」，也不能写成「处处等价」。
  - `resumeSession`：`async resumeSession(input) { return input; }`（`InProcessGateway.ts:2026-2028`）——**原样返回，不校验、不 trim、不转发**，`sessionKey` 是空白还是非字符串都照样放行。Rust 侧相反：`session_resume`（`session_dir.rs:179`）先跑 `require_session_key`，空白 / 缺键答 `INVALID_SESSION_KEY`（请求级用例 `resume_session_rejects_blank_session_key`，`:848-862`）。
  - `closeSession`：`router.close(input.sessionKey)` 之后还执行 `interactionCoordinator.closeSession` / `permissionModes.clear` / `dialogProjectKeys.delete`（`:2049-2054`），整条链**没有任何 `INVALID_SESSION_KEY` 拒绝、也没有 `trim`**；非字符串 `sessionKey` 会原样传给 router，行为本探针**未测**。Rust 侧 `session_close`（`session_dir.rs:214`）同样先跑 `require_session_key`，空白 / 缺键答 `INVALID_SESSION_KEY`。
  - **非字符串的可选链 TypeError 只对「确实走到门」的路径成立**（delete/rename/tag 的 provider 门，且 `projectKey` 能解析）：`resume` 不校验、`close` 只透传，这**两个方法**上两侧不同形状，不能概括成「五个方法同形状」。

  **-R2:28 没有登记这条**——它登记的是「非字符串的错误形状与 TS 的 TypeError 不一致，待 #1457 装配回调时定夺」，不是门的位置差异。这条差异属 #1455 遗留、非 #1604 引入，已开 **#2090**（前置于 #1457 与 #1604 关单）。**门序本身也没有请求级用例**：本 PR 只加了 guard 级用例（`session_dir.rs:882-890`），请求级只有 resume 那一条（`:848-862`）；delete/rename/tag 上「空白 sessionKey + 不可解析 projectKey → `INVALID_SESSION_KEY` 先于 `CAPABILITY_UNAVAILABLE`」这一组合无人钉住，已并入 **#2090** 的范围（同样前置于 #1604 关单）。U+0085 一侧的翻转在 **gateway 的 `trim_guard` / `require_text` 这一层没有用例钉住**（BOM 一侧有）；`pilotdeck-cli` 的 `paths.rs:189-192` 钉的是它**自己那份** `js_trim` 拷贝，不穿透本门。已开跟进单 **#2097**，前置于 #1604 关单。
- `session_dir` 族其余 `projectKey` 读点（`resolve_project` / `session_new`）仍是旧形状 → **#2068**。
- 上面那条 `projectKey` 孤立代理项的残留差异要「对齐还是接受」需要人批 → **#2083**（决策单，前置于 #1604 关单）。

## 机器判据

```
$ ( cd resources/PilotDeck && git rev-parse HEAD )        # 子 shell：不污染后续命令的 cwd
cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85
$ node --experimental-transform-types _tmp/20261107-p2sweep/ts-probe-projectkey/probe.mjs
# 在主仓根目录执行（探针 import 基线自己的 errors.ts，参数属性语法要 transform-types）；Node v22.22.0 / icu 78.2
node v22.22.0 icu 78.2
== projectKey ==
["missing","THROW","Error/Error","{\"message\":\"projectKey is required.\"}"]
["undefined","THROW","Error/Error","{\"message\":\"projectKey is required.\"}"]
["null","THROW","Error/Error","{\"message\":\"projectKey is required.\"}"]
["number 7","THROW","TypeError/TypeError","{\"message\":\"input.projectKey?.trim is not a function\"}"]
["blank '  '","THROW","Error/Error","{\"message\":\"projectKey is required.\"}"]
["BOM only","THROW","Error/Error","{\"message\":\"projectKey is required.\"}"]
["lone surrogate","PASS","{\"projectKey\":\"\\ud800\"}"]
["ok","PASS","{\"projectKey\":\"/tmp/x\"}"]
== sessionKey ==
["missing","THROW","DialogGatewayError/DialogGatewayError","{\"code\":\"INVALID_SESSION_KEY\",\"message\":\"sessionKey is required.\"}"]
["undefined","THROW","DialogGatewayError/DialogGatewayError","{\"code\":\"INVALID_SESSION_KEY\",\"message\":\"sessionKey is required.\"}"]
["null","THROW","DialogGatewayError/DialogGatewayError","{\"code\":\"INVALID_SESSION_KEY\",\"message\":\"sessionKey is required.\"}"]
["number 7","THROW","TypeError/TypeError","{\"message\":\"input.sessionKey?.trim is not a function\"}"]
["blank '  '","THROW","DialogGatewayError/DialogGatewayError","{\"code\":\"INVALID_SESSION_KEY\",\"message\":\"sessionKey is required.\"}"]
["BOM only","THROW","DialogGatewayError/DialogGatewayError","{\"code\":\"INVALID_SESSION_KEY\",\"message\":\"sessionKey is required.\"}"]
["lone surrogate","PASS","{\"sessionKey\":\"\\ud800\"}"]
["ok","PASS","{\"sessionKey\":\"/tmp/x\"}"]
["params=null","THROW","TypeError/TypeError","{\"message\":\"Cannot read properties of null (reading 'projectKey')\"}"]
["path.resolve(projectKey)","RETURN","/Users/da/ws/pilotdeck-rs/\ud800"]
$ node -e 'const nel="\u0085", bom="\ufeff"; console.log(JSON.stringify({nel_blank: nel.trim()==="", bom_blank: bom.trim()==="", node: process.version}))'
{"nel_blank":false,"bom_blank":true,"node":"v22.22.0"}
$ node -e 'const p=require("node:path"); for (const v of [7,null,""," ","/nope/x"]) { try { console.log(JSON.stringify([String(v),"RETURN",p.resolve(v)])) } catch(e) { console.log(JSON.stringify([String(v),"THROW",e.name,e.code,e.message])) } }'   # \u4e3b\u4ed3\u6839\u76ee\u5f55
["7","THROW","TypeError","ERR_INVALID_ARG_TYPE","The \"paths[0]\" argument must be of type string. Received type number (7)"]
["null","THROW","TypeError","ERR_INVALID_ARG_TYPE","The \"paths[0]\" argument must be of type string. Received null"]
["","RETURN","/Users/da/ws/pilotdeck-rs"]
[" ","RETURN","/Users/da/ws/pilotdeck-rs/ "]
["/nope/x","RETURN","/nope/x"]
$ ssh mbp2 'mkdir -p /tmp/pdrs-nel && cat > /tmp/pdrs-nel/n.rs <<"EOF"
fn main() {
    for (name, s) in [("NEL", "\u{0085}"), ("BOM", "\u{feff}"), ("SP", " ")] {
        println!("{name} trim_is_empty={} is_whitespace={}", s.trim().is_empty(), s.chars().next().map(|c| c.is_whitespace()).unwrap_or(false));
    }
}
EOF
cd /tmp/pdrs-nel && rustc -O -o n n.rs && ./n && rustc --version'   # 本机不起 cargo/rustc，实测在 mbp2
NEL trim_is_empty=true is_whitespace=true
BOM trim_is_empty=false is_whitespace=false
SP trim_is_empty=true is_whitespace=true
rustc 1.99.0 (b940084d7 2026-09-28)
$ git show origin/main:src/pilotdeck-gateway/src/native_process/methods/session_dir.rs | sed -n '69,76p;790,798p'
（.filter(super::config::trimmed_string_is_non_empty) + 断言 is_ok()==true 的既有用例）
$ git show origin/main:src/pilotdeck-gateway/src/native_process/methods/tools.rs | sed -n '54,64p'
（`s.to_utf8().map(|u| u.trim().is_empty()).unwrap_or(false)`）
$ grep -n "input.projectKey ?? fallbackProjectRoot" resources/PilotDeck/src/cli/createLocalGateway.ts | head -4
1004:          const resolvedProjectKey = await dialog.projects.resolveProjectKey(input.projectKey ?? fallbackProjectRoot);
1117:      const projectKey = await dialog.projects.resolveProjectKey(input.projectKey ?? fallbackProjectRoot);
1130:      const projectKey = await dialog.projects.resolveProjectKey(input.projectKey ?? fallbackProjectRoot);
1143:      const projectKey = await dialog.projects.resolveProjectKey(input.projectKey ?? fallbackProjectRoot);
（`:1117` / `:1130` / `:1143` 是 rename / tag / delete 三处；`:1004` 是 native-archive 授权器，正文不引）
$ sed -n '41,60p' resources/PilotDeck/src/gateway/dialog/projectRegistry.ts
（`const requestedProjectKey = resolve(projectKey);` @:42 + 两处 `PROJECT_NOT_FOUND` @:50 / :57）
$ sed -n '882,890p' src/pilotdeck-gateway/src/native_process/methods/session_dir.rs
（a_non_string_session_key_throws_the_optional_chain_type_error：断言 `gateway_request_failed` + `input.sessionKey?.trim is not a function`）
$ sed -n '219,253p' resources/PilotDeck/src/gateway/server/GatewayWsConnection.ts
（`:219` `error instanceof DialogGatewayError || hasStructuredErrorCode(error)` 分支保留 `error.code`；`:245-253` 兜底 `code: "gateway_request_failed"` + `error.message`）
$ grep -n "fallbackProjectRoot" resources/PilotDeck/src/cli/LocalGatewayBootConfig.ts
16:  fallbackProjectRoot?: string;
28:  fallbackProjectRoot: string;
52:    fallbackProjectRoot: input.fallbackProjectRoot ?? projectRoot,
$ sed -n '2230p' resources/PilotDeck/src/gateway/client/InProcessGateway.ts; sed -n '1417p' resources/PilotDeck/src/cli/createLocalGateway.ts
    if (this.options.deleteEphemeralSession && await this.options.deleteEphemeralSession(input)) {
    deleteEphemeralSession: (input) => registry.deleteEphemeralSession(input.sessionKey),
$ sed -n '2228,2236p' resources/PilotDeck/src/gateway/client/InProcessGateway.ts   # 钩子是「条件调用」；无 deleteSession option 时 CAPABILITY_UNAVAILABLE

  async deleteSession(input: { sessionKey: string; projectKey?: string }): Promise<void> {
    if (this.options.deleteEphemeralSession && await this.options.deleteEphemeralSession(input)) {
      this.dialogProjectKeys.delete(input.sessionKey);
      return;
    }
    if (!this.options.deleteSession) {
      throw new DialogGatewayError("CAPABILITY_UNAVAILABLE", "delete_session is unavailable.");
    }
$ grep -n "deleteEphemeralSession?" resources/PilotDeck/src/gateway/client/InProcessGateway.ts
312:  deleteEphemeralSession?: (input: { sessionKey: string; projectKey?: string }) => Promise<boolean>;
$ grep -rn "new InProcessGateway" resources/PilotDeck/src/
resources/PilotDeck/src/cli/createLocalGateway.ts:1021:  const gateway = new InProcessGateway(router, {
resources/PilotDeck/src/gateway/Gateway.ts:58:  return new InProcessGateway(router, {
$ sed -n '2026,2028p;2049,2054p' resources/PilotDeck/src/gateway/client/InProcessGateway.ts
  async resumeSession(input: { sessionKey: string }): Promise<{ sessionKey: string }> {
    return input;
  }
  async closeSession(input: { sessionKey: string; reason?: string }): Promise<void> {
    await this.router.close(input.sessionKey);
    this.interactionCoordinator.closeSession(input.sessionKey, input.reason ?? "session_closed");
    this.permissionModes.clear(input.sessionKey);
    this.dialogProjectKeys.delete(input.sessionKey);
  }
$ sed -n '892,905p' src/pilotdeck-gateway/src/native_process/methods/session_dir.rs
（a_null_params_object_reports_the_optional_chain_read_error：断言 `gateway_request_failed` + `Cannot read properties of null (reading 'sessionKey')`）
$ sed -n '230,232p;300,308p' src/pilotdeck-gateway/src/native_process/methods/session_dir.rs
（delete：`require_session_key` → `resolve_project`；rename/tag：`require_session_key` → active-turn 检查 → `resolve_project`）
$ node -e 'const gate=(input)=>{ const pk = input.projectKey ?? "/fb"; return pk; }; try { gate(null) } catch(e) { console.log(JSON.stringify(["null", e.name, e.message])) } const g2=(params)=>{ if (!params.sessionKey?.trim()) throw new Error("x") }; try { g2(null) } catch(e) { console.log(JSON.stringify(["null-sessionKey-gate", e.name, e.message])) } const g3=(input)=>input.sessionKey?.trim(); try { g3(null) } catch(e) { console.log(JSON.stringify(["delete-hook-shape", e.name, e.message])) }'
["null","TypeError","Cannot read properties of null (reading 'projectKey')"]
["null-sessionKey-gate","TypeError","Cannot read properties of null (reading 'sessionKey')"]
["delete-hook-shape","TypeError","Cannot read properties of null (reading 'sessionKey')"]
$ node -e 'const resume=(input)=>{ return input; }; try { console.log(JSON.stringify(["resume(null)","RETURN",resume(null)])) } catch(e) { console.log(JSON.stringify(["resume(null)","THROW",e.name,e.message])) } const closeArg=(input)=>input.sessionKey; try { console.log(JSON.stringify(["close(null)","RETURN",String(closeArg(null))])) } catch(e) { console.log(JSON.stringify(["close(null)","THROW",e.name,e.message])) } console.log(process.version)'
["resume(null)","RETURN",null]
["close(null)","THROW","TypeError","Cannot read properties of null (reading 'sessionKey')"]
v22.22.0
$ python3 -I scripts/gateway-parity/check_frozen.py --base origin/main
{"status": "PASS", "failures": [], "entries": 30}
```

`params=null` 那一行走的是 `projectKey` 门（报文 `(reading 'projectKey')`），只是探针把它排在 `== sessionKey ==` 分节末尾；sessionKey 门与 delete 钩子形状的 `params=null` 见上面 `node -e` 段的 `null-sessionKey-gate` / `delete-hook-shape` 两行，`resume` / `close` 两个形状见紧接的 `resume(null)` / `close(null)` 两行——都是**表达式级**实测（照录表达式，不是端到端）。`resume` 那行照录的是 `resumeSession` 的整个方法体（`InProcessGateway.ts:2026-2028`），`close` 那行照录的是 `closeSession` 求值实参那一处裸读（`:2050` 的 `input.sessionKey`），不含其后的 `router.close` 与三项清理。`-R7:30` 的「同形状」只覆盖缺键 / `null` / 空白 / U+FEFF / 非字符串 / 孤立代理项六项，不含 `params=null`。

探针的 **stderr 另有两行** `ExperimentalWarning: Transform Types is an experimental feature…`（`--experimental-transform-types` 自带），与判据无关，上面只转录 stdout。探针脚本 `probe.mjs` 的 BOM 用例已从裸字面量改成 `"\uFEFF"` 转义（输出逐字节不变，实测 diff 为空），避免不可见字符在文件被改写时静默退化。脚本在 `_tmp/`，不属本 PR 补丁。

`path.resolve` 那行是**执行目录**的绝对化结果，随 cwd 变；上面的值是文档记录时在主仓根目录跑出来的。探针只覆盖**门表达式**与门后第一步（`path.resolve`）。TS 在这之后到底成功还是失败（`router.closeProject` / `resumeProject` 的语义、项目归属检查）本探针**没有**实测，`-R7` 也不据此下结论。

## 批准记录

未批准，非已接受差异。2026-11-07 #1604 实现轮由 AI 代登记（评审 codex 指出 -R6:8/-R6:33 的迁移前描述与 -R2:29、`origin/main` 源码相反；本条按只追加规则更正）。两处残留差异待人决定：`projectKey` 孤立代理项（对齐 / 接受，**#2083**）；`sessionKey` 门在 TS 不存在（**#2090**，前置于 #1457）。
