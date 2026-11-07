---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R8
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

更正登记（关闭 -R7 所列残留差异之一：`projectKey` 孤立代理项）。#2083 决策单人 2026-11-07 07:23Z 拍板选项 1（对齐 TS，探针先行）。真实入口探针实测 TS 门后**成功**，Rust 据此删除 `close_project_sessions` 的显式 `to_utf8` 拒绝门：孤立代理项过 `?.trim()` 门后与任意未配置项目键同路。-R7 及更早条目字节不动，本条按只追加规则登记。

## TS 依据

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`（`resources/PilotDeck`，只读），Node v22.22.0 / ICU 78.2。真实入口探针（实例化基线编译产物 `InProcessGateway.closeProjectSessions` + 真实 `SessionRouter`，stub `createSession`；脚本与原始输出存 `_tmp/20261107-issue-2083/`：worker 原件 `ts-probe-close-project-sessions.mjs` / `probe.out`，本机复跑 `ts-probe-local.mjs` / `probe-local.out`，两者逐行一致）。要点原文：

- 门控对照：`projectKey` 缺失 / `"  "` → `Error "projectKey is required."`；`7` → `TypeError "input.projectKey?.trim is not a function"`。
- 孤立代理项 `"\uD800"`（close 分支与 `resume:true` 分支）→ **`{"sessionKeys":[]}` 成功**，无错误码 / 文案。
- 副作用真实：`resolve("\uD800")` 进 `pausedProjects`（同项目再 `getOrCreate` 报 `Project is being deleted.`）；resume 解除后同项目可再建会话；该项目下在册会话真被关闭并回报 `{"sessionKeys":[…]}`，与普通键（`/tmp/pd-probe-plain`）形状一致。

## Rust 行为

`src/pilotdeck-gateway/src/native_process/methods/session_dir.rs` `session_close_project`（#2083 后）：

- 删除 `to_utf8()` 拒绝门（原「不是有效 UTF-8 即答缺键域错」），`project_key_str` 改 `String::from_utf16_lossy(project_key.units())`，**仅用于** `serves_project` 已配置项目比较；原始 UTF-16 `JsString` 仍传 `resume_project` / `close_project`，会话归属按原拼写匹配（同 TS 的严格字符串相等）。
- 对齐后：`"\uD800"` 与任意未配置项目键同路——`CAPABILITY_UNAVAILABLE` + `The isolated native process serves only its configured project.`（单项目隔离进程边界，对普通未配置键早已如此，非本单引入；TS 探针用的是全量 router，非单项目隔离进程）。
- 残余、随人拍选项 1 接受：有损化 `"\u{FFFD}"` 撞上恰好含 U+FFFD 的已配置项目路径会被 serving（现实中基本不存在）。

## 范围

只关闭 -R7「批准记录」所列残留差异中的第一条（`projectKey` 孤立代理项，#2083）。-R7 登记的其余差异不动：`sessionKey` 门位置/形状（#2090，前置于 #1457 与 #1604 关单）、`session_dir` 族其余 `projectKey` 读点（#2068）、`trim_guard` 的 U+0085 侧（#2097）。不改 -R7 与更早条目字节，不新增豁免，不改判分语义。

## 机器判据

- 请求级用例 `close_project_sessions_surrogate_project_key_reaches_the_capability_gate`（`session_dir.rs`，close / resume 两臂）：孤立代理项答 `CAPABILITY_UNAVAILABLE` + 上述文案；既有 `close_project_sessions_blank_project_key_is_the_domain_error`（原 `blank_or_surrogate` 改名瘦身）保空白 / BOM 臂逐字断言不变。
- 负控 `session-dir-surrogate-rejected`（`native-config/negative_controls.py`）：把 lossy 转换变异回 `to_utf8` 拒绝门，绑定上述新用例须红；新用例断言消息入 `LITERAL_ANCHORS`（变异数 41 → 42，README 同步）。`mutating_sources` 要求源码等于 HEAD，正式负控跑由宿主在提交后执行；`anchor_selfcheck` 树自检通过（2026-11-07 实测）。

## 批准记录

已批准（走已接受路径的差异关闭）。2026-11-07 07:23Z 人拍板 #2083 选项 1（对齐 TS，探针先行）；真实入口探针实测 TS 门后成功后落地对齐。-R7 对该残留差异标的「未批准」由本条关闭；-R7 同时登记的 `sessionKey` 门差异（#2090）仍待人处理。
