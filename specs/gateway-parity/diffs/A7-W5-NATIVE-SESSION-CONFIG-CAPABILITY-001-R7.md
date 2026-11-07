---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R7
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

更正登记（审计事实更正 + 补齐固定基线实测）。更正 -R6 的两处错误陈述，并把 -R6 标为「本轮不可读」的固定基线实测补上。-**R6 正文保持原样**（只追加规则），审计以本条为准。

1. **-R6:8 与 -R6:33 对迁移前状态的描述是错的。** -R6 称 -R2 记录的是「`tools.rs` 的 `required_trim` 与 `session_dir.rs` 的门仍是 `to_utf8` + Rust `str::trim` 的旧形态」，并在三态表里写「session_dir 旧门 `to_utf8` 过滤 → 拒」。实际 -R2:29 写的是**相反**的事实：`session_dir` 的 `require_session_key` 早在 #1455 就已改为共享的 lossy `?.trim()` 实现、**放行**孤立代理项，只有 `tools.rs` 仍留着 `to_utf8` + Rust `str::trim`。因此 **#1604 没有改变 `sessionKey` 孤立代理项的判定**（迁移前后都放行）；该族唯一的行为改变是非字符串从 `INVALID_SESSION_KEY` 改答可选链 TypeError。
2. **-R6:12 说固定基线「本仓外 checkout，本轮不可读」也不成立。** 固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 就在本仓 `resources/PilotDeck`（`git rev-parse HEAD` 实测同值），本轮已用它跑了只读探针。

## TS 依据

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，路径 `resources/PilotDeck`（只读）。直接相关的两处源码：

- `src/gateway/client/InProcessGateway.ts:2037`：`if (!input.projectKey?.trim()) throw new Error("projectKey is required.");`（`closeProjectSessions`）。
- `src/gateway/client/InProcessGateway.ts:2049`：`closeSession` 与 `resumeSession`（:2026）、`deleteSession`（:2229）、`renameSession`（:2211）、`tagSession`（:2220）**都没有** `?.trim()` 门，直接把 `input` 交给下层。

只读探针（Node v22.22.0 / ICU 78.2，脚本与原始输出见 `_tmp/20261107-p2sweep/ts-probe-projectkey/`）：两个门用**点属性**写死 `input.projectKey` / `input.sessionKey`，错误文本与基线逐字一致（不参数化成 `input[field]`）。

| 输入 | TS `!input.projectKey?.trim()` |
|---|---|
| 键不存在 / `undefined` / `null` | 折叠 → `Error("projectKey is required.")` |
| 数字 `7` | `TypeError: input.projectKey?.trim is not a function` |
| 全是空格 / 只有一个 U+FEFF | 折叠 → 域名错误 |
| 孤立代理项 `U+D800` | **通过**（门之后的 `resolve(input.projectKey)` 也不抛；再往后的 router 行为本探针**未覆盖**） |
| `params` 整体为 `null` | `TypeError: Cannot read properties of null (reading 'projectKey')` |

同一表达式形状的 `sessionKey` 门（例如 `InProcessGateway.ts:2286` 的 `setMcpServers`；**session_dir 族没有此门**，见上）探针结果同形状：缺键 / `null` / 空白 / U+FEFF 折叠为 `INVALID_SESSION_KEY`，非字符串抛 `TypeError: input.sessionKey?.trim is not a function`，孤立代理项**通过**。

## Rust 行为

- `sessionKey` 一族：`require_session_key` 在 #1604 前（`origin/main` 的 `session_dir.rs:69-76`）就是 `.and_then(JsValue::as_string).cloned().filter(super::config::trimmed_string_is_non_empty)`，而 `config.rs:66-68` 的 `trimmed_string_is_non_empty` 用 `String::from_utf16_lossy` + `js_trim` —— 孤立代理项**放行**。`origin/main` 的既有用例 `require_session_key_keeps_lone_surrogates_and_rejects_blank`（`session_dir.rs:790`）就断言 `is_ok() == true`。#1604 迁移后仍是放行（`trim_guard` 同样走 `from_utf16_lossy`），**行为未变**；变的是非字符串：`INVALID_SESSION_KEY` → `gateway_request_failed` + `input.sessionKey?.trim is not a function`。
- `tools.rs`：旧 `required_trimmed`（`origin/main` 的 `tools.rs:54-64`）是 `string(params, field)?` + `s.to_utf8().map(|u| u.trim().is_empty()).unwrap_or(false)`。`to_utf8()` 失败时 `unwrap_or(false)` 直接放行，所以孤立代理项迁移前后都放行；只有 U+FEFF 从「Rust `str::trim` 不剥 → 漏判放行」改成「`js_trim` 判空白 → 域名错误」。这一列 -R6 记的是对的。
- `close_project_sessions` 的 `projectKey`：非字符串由 `require_text` 改答可选链 TypeError；之间/空白/非 UTF-8 仍答 `gateway_request_failed` + `projectKey is required.`（`session_dir.rs:436-441` 的显式 `to_utf8` 门）。**迁移前后一致**（`origin/main` 的 `session_dir.rs:421-426` 用 `to_utf8().ok()`）。

**残留差异（本条新登记，未批准）**：孤立代理项的 `projectKey` —— TS 通过门并继续执行；**门之后 TS 到底成功还是失败，本探针没有实测**（只证到 `path.resolve` 不抛，`router.closeProject` / `resumeProject` 的行为未覆盖），所以这里只登记**门这一层的差异**，不推断最终结果。Rust 则在门口直接答 `gateway_request_failed` / `projectKey is required.`。这是 #1604 之前就有的差异（`origin/main` 的 `to_utf8().ok()` 即如此），本 PR 未引入也未改变，但此前账本没有单独条目。要不要对齐见上文的范围与 `-R7` 批准记录。

## 范围

只更正 -R6 的两处陈述、补 `projectKey` 门的固定基线实测，并登记两条残留差异。**不新增豁免、不改判分语义、不动 -R6 与更早条目的字节。**

**仍未闭合（如实登记，不用跟进单冒充验收证据）**：

- `session_dir` 族 `sessionKey` 的**门是否应当存在**（本条新登记，未批准）：TS 基线在 `resumeSession` / `closeSession` / `deleteSession` / `renameSession` / `tagSession` 上**根本没有** `?.trim()` 门（`InProcessGateway.ts:2026`、`:2049`、`:2229`、`:2211`、`:2220`，都是直接把 `input` 交给下层），而 Rust 侧 `require_session_key` 会先答 `INVALID_SESSION_KEY` / 可选链 TypeError。**-R2:28 没有登记这条**——它登记的是「非字符串的错误形状与 TS 的 TypeError 不一致，待 #1457 装配回调时定夺」，不是「门本身在 TS 不存在」。这条差异属 #1455 遗留、非 #1604 引入，已开 **#2090**（P1-跟进，前置于 #1457 与 #1604 关单）。
- `session_dir` 族其余 `projectKey` 读点（`resolve_project` / `session_new`）仍是旧形状 → **#2068**。
- 上面那条 `projectKey` 孤立代理项的残留差异要「对齐还是接受」需要人批 → **#2083**（决策单，前置于 #1604 关单）。

## 机器判据

```
$ ( cd resources/PilotDeck && git rev-parse HEAD )        # 子 shell：不污染后续命令的 cwd
cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85
$ node _tmp/20261107-p2sweep/ts-probe-projectkey/probe.mjs        # 主仓根目录执行；Node v22.22.0 / icu 78.2
node v22.22.0 icu 78.2
== projectKey ==
missing          ["FOLDED","Error: projectKey is required."]
undefined        ["FOLDED","Error: projectKey is required."]
null             ["FOLDED","Error: projectKey is required."]
number 7         ["THROW","TypeError: input.projectKey?.trim is not a function"]
blank '  '       ["FOLDED","Error: projectKey is required."]
BOM only         ["FOLDED","Error: projectKey is required."]
lone surrogate   ["PASS","\"\\ud800\""]
ok               ["PASS","\"/tmp/x\""]
== sessionKey ==
missing          ["FOLDED","DialogGatewayError: INVALID_SESSION_KEY"]
undefined        ["FOLDED","DialogGatewayError: INVALID_SESSION_KEY"]
null             ["FOLDED","DialogGatewayError: INVALID_SESSION_KEY"]
number 7         ["THROW","TypeError: input.sessionKey?.trim is not a function"]
blank '  '       ["FOLDED","DialogGatewayError: INVALID_SESSION_KEY"]
BOM only         ["FOLDED","DialogGatewayError: INVALID_SESSION_KEY"]
lone surrogate   ["PASS","\"\\ud800\""]
ok               ["PASS","\"/tmp/x\""]
params=null      ["THROW","TypeError: Cannot read properties of null (reading 'projectKey')"]
path.resolve(projectKey) "/Users/da/ws/pilotdeck-rs/_tmp/20261107-p2sweep/ts-probe-projectkey/\ud800"
$ git show origin/main:src/pilotdeck-gateway/src/native_process/methods/session_dir.rs | sed -n '69,76p;790,798p'
（.filter(super::config::trimmed_string_is_non_empty) + 断言 is_ok()==true 的既有用例）
$ python3 -I scripts/gateway-parity/check_frozen.py --base origin/main
{"status": "PASS", "failures": [], "entries": 30}
```

探针只覆盖**门表达式**与门后第一步（`path.resolve`）。TS 在这之后到底成功还是失败（`router.closeProject` / `resumeProject` 的语义、项目归属检查）本探针**没有**实测，`-R7` 也不据此下结论。

## 批准记录

未批准，非已接受差异。2026-11-07 #1604 实现轮由 AI 代登记（评审 codex 指出 -R6:8/-R6:33 的迁移前描述与 -R2:29、`origin/main` 源码相反；本条按只追加规则更正）。两处残留差异待人决定：`projectKey` 孤立代理项（对齐 / 接受，**#2083**）；`sessionKey` 门在 TS 不存在（**#2090**，前置于 #1457）。
