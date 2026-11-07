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
- `src/gateway/client/InProcessGateway.ts:2049`：`closeSession` 与 `resumeSession`（:2026）、`deleteSession`（:2229）、`renameSession`（:2211）、`tagSession`（:2220）在**这一层**都**没有** `?.trim()` 门，直接把 `input` 交给下层。**但端到端不等于「TS 全链不拒」**：`deleteSession` / `renameSession` / `tagSession` 由 CLI provider 层的同名方法拒空白，答同一个 `DialogGatewayError("INVALID_SESSION_KEY", "sessionKey is required.")`（`src/cli/createLocalGateway.ts:1144` / `:1118` / `:1131`；`specs/gateway-coverage/ledger.json:4111` / `:4335` / `:4406` 同锚）。`resumeSession` 直接把 `input` 原样返回（`:2026-2028`），`closeSession` 只调 `router.close`（`:2049-2053`）——只有这两条**没有任何 `INVALID_SESSION_KEY` 拒绝**。

只读探针（Node v22.22.0 / ICU 78.2，脚本与原始输出见 `_tmp/20261107-p2sweep/ts-probe-projectkey/`）：门与 `throw` 语句**照抄基线源码行**（`InProcessGateway.ts:2037` / `:2286`），并 `import` 基线**自己的错误类** `src/gateway/dialog/errors.ts` 的 `DialogGatewayError`——所以下表是**真实抛出的错误对象**的 `name` / `code` / `message`，不是手写标签。

| 输入 | TS `!input.projectKey?.trim()`（真实抛出的对象） |
|---|---|
| 键不存在 / `undefined` / `null` | `Error` · `projectKey is required.` |
| 数字 `7` | `TypeError` · `input.projectKey?.trim is not a function` |
| 全是空格 / 只有一个 U+FEFF | `Error` · `projectKey is required.` |
| 孤立代理项 `U+D800` | **通过**（门之后的 `path.resolve` 也不抛；再往后的 router 行为本探针**未覆盖**） |
| `params` 整体为 `null` | `TypeError` · `Cannot read properties of null (reading 'projectKey')` |

同一表达式形状的 `sessionKey` 门（例如 `InProcessGateway.ts:2286` 的 `setMcpServers`；session_dir 族在 IPG 层没有此门，见上）探针结果同形状：缺键 / `null` / 空白 / U+FEFF → `DialogGatewayError`，`code: INVALID_SESSION_KEY` + `message: sessionKey is required.`；非字符串 → `TypeError: input.sessionKey?.trim is not a function`；孤立代理项**通过**。

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
  - `renameSession` / `tagSession` / `deleteSession`：IPG 只是转发（`this.options.renameSession(input)` `InProcessGateway.ts:2211-2218`、`this.options.tagSession(input)` `:2220-2227`、`await this.options.deleteSession(input)` `:2229-2239`；`deleteSession` 之前还先试 `deleteEphemeralSession` 钩子，`:2230` —— 空白键在该钩子里命中与否本探针**未测**）。转发到的 CLI provider 门**不是链上第一步**：`resolveProjectKey(input.projectKey ?? fallbackProjectRoot)` 先跑（`createLocalGateway.ts:1117` / `:1130` / `:1143`），项目解析不了就先抛 `PROJECT_NOT_FOUND`（`projectRegistry.ts:47-50`），**只有项目能解析时**才轮到 `if (!input.sessionKey?.trim()) throw new DialogGatewayError("INVALID_SESSION_KEY", "sessionKey is required.")`（`:1118` / `:1131` / `:1144`，与 `ledger.json:4111` / `:4335` / `:4406` 同锚）。所以「错误码与报文相同」**只在单字段无效（projectKey 可解析）时成立**；两个字段同时无效时，TS 先报 `PROJECT_NOT_FOUND`、Rust 先报 `INVALID_SESSION_KEY`，可观测结果不同。与 Rust 的差别是**门的层、时机与前置步骤**——不能写成「TS 不拒」，也不能写成「处处等价」。
  - `resumeSession`：`async resumeSession(input) { return input; }`（`InProcessGateway.ts:2026-2028`）——**原样返回，不校验、不 trim、不转发**，`sessionKey` 是空白还是非字符串都照样放行。
  - `closeSession`：`router.close(input.sessionKey)` 之后还执行 `interactionCoordinator.closeSession` / `permissionModes.clear` / `dialogProjectKeys.delete`（`:2049-2054`），整条链**没有任何 `INVALID_SESSION_KEY` 拒绝、也没有 `trim`**；非字符串 `sessionKey` 会原样传给 router，行为本探针**未测**。
  - **非字符串的可选链 TypeError 只对「确实走到门」的路径成立**（delete/rename/tag 的 provider 门）：`resume` 不校验、`close` 只透传，两侧在非字符串上**不同形状**，不能概括成「五个方法同形状」。

  **-R2:28 没有登记这条**——它登记的是「非字符串的错误形状与 TS 的 TypeError 不一致，待 #1457 装配回调时定夺」，不是门的位置差异。这条差异属 #1455 遗留、非 #1604 引入，已开 **#2090**（前置于 #1457 与 #1604 关单）。U+0085 一侧的翻转仓内**没有用例钉住**（BOM 一侧有），已开跟进单 **#2097**，前置于 #1604 关单。
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
$ python3 -I scripts/gateway-parity/check_frozen.py --base origin/main
{"status": "PASS", "failures": [], "entries": 30}
```

`params=null` 那一行走的是 `projectKey` 门（报文 `(reading 'projectKey')`），只是探针把它排在 `== sessionKey ==` 分节末尾；sessionKey 门的 `params=null` 形状是 `(reading 'sessionKey')`，本轮未单独出图，`-R7:30` 的「同形状」只覆盖缺键 / `null` / 空白 / U+FEFF / 非字符串 / 孤立代理项六项，不含这一项。

探针的 **stderr 另有两行** `ExperimentalWarning: Transform Types is an experimental feature…`（`--experimental-transform-types` 自带），与判据无关，上面只转录 stdout。探针脚本 `probe.mjs` 的 BOM 用例已从裸字面量改成 `"\uFEFF"` 转义（输出逐字节不变，实测 diff 为空），避免不可见字符在文件被改写时静默退化。脚本在 `_tmp/`，不属本 PR 补丁。

`path.resolve` 那行是**执行目录**的绝对化结果，随 cwd 变；上面的值是文档记录时在主仓根目录跑出来的。探针只覆盖**门表达式**与门后第一步（`path.resolve`）。TS 在这之后到底成功还是失败（`router.closeProject` / `resumeProject` 的语义、项目归属检查）本探针**没有**实测，`-R7` 也不据此下结论。

## 批准记录

未批准，非已接受差异。2026-11-07 #1604 实现轮由 AI 代登记（评审 codex 指出 -R6:8/-R6:33 的迁移前描述与 -R2:29、`origin/main` 源码相反；本条按只追加规则更正）。两处残留差异待人决定：`projectKey` 孤立代理项（对齐 / 接受，**#2083**）；`sessionKey` 门在 TS 不存在（**#2090**，前置于 #1457）。
