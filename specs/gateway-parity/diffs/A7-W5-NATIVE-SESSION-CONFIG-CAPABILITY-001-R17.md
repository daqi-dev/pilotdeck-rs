---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R17
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

更正件（R 链，承接 `A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R13`；frontmatter
`original` 按账本机器规则指基础 ID，与 R9-R13 同法，承接关系由文件名与正文表达）。
登记 #2617 实现单（分支 `claude/issue-2617-projectkey-surrogate`，基线
`3ca4e1f39f`，交付提交 `243f9f3da`，fix-r1 随 PR #2983 续）落地后 submit 门
（R13「范围」点名 #2882 先探针后修的三读点之一）的答案格变化，并勘误 r1 注释里
「TS 把孤立代理串 resolve 到配置项目」的判据表述（评审 MM boundary P1 + 协调者
复核实测）：

- 差异（submit 门非 UTF-8 串键）：修前 `Services::workspace_id`
  （`services.rs`）的 `projectKey` 字符串分支是
  `as_string + to_utf8().ok()`，孤立代理项串（`{"projectKey":"\ud800"}`）被
  `gateway_submit_failed` + `projectKey must be a valid path string.` 拒绝
  （#2562 定形的类型门报文）。修后字符串分支改
  `String::from_utf16_lossy` 取真键（#2083 close 路径同法，非串类型仍走
  #2562 类型门），`serves_project` 能力门一字不动：未服务键答
  `CAPABILITY_UNAVAILABLE` + 隔离报文。
- 勘误（判据表述）：r1 注释曾依据 worker 的 resolver 级复跑
  （`probe-submit-surrogate-resolver.out` 的 `loneSurrogateResolvesLikeFallback:
  true`）把 TS 结论写成「resolve 结果与 fallback（配置项目）的 slug 完全相同」。
  该复跑脚本的 `fallbackProjectRoot = process.cwd()`（构造性恒真，只能证明
  「落到 cwd 的 slug」）；协调者 submit 级探针
  （`probe-submit-surrogate.out`，fallback 是独立临时项目）显示孤立代理串
  实际落到**进程 cwd 的项目** slug（`Users-da-ws-pilotdeck-rs`），≠ fallback
  项目 slug（`var-folders-…-fallback-project`），对照串 `"\ud800sub"` →
  `…-sub`（slug 正则 `[^A-Za-z0-9._-]+` 把代理段收「-」后经首尾修剪丢弃）。
  即：TS 对该键**不抛错、照常 resolve**（这一 r1 结论成立），但落点是 cwd 项目
  而非必然是配置项目。
- 分叉（与 #2083 close 路径已登记形状同族，非新增豁免）：两侧 identity 规则
  不同——TS 按 storage id（slug）判，孤立代理串折叠到 cwd 项目 slug → 放行；
  Rust 按原始路径恒等判，lossy 形（U+FFFD）≠ 配置项目 → `CAPABILITY_UNAVAILABLE`。
  当 native 进程 cwd == `--project` 时 TS 落到配置项目继续服务、Rust 拒绝，
  即「TS 放行 / Rust 拒」；是否按 storage-id identity 对齐属后续判定
  （#2083 人拍选项 1 采纳的同一分差家族，本件沿用同判，不扩大）。

R 号口径：R14 已在队双占（PR #2957 / #2913，另 `codex/a8-cli-integration`
#2786 的远端 ref 亦持有），R15 为 PR #2960 与 #2786 占用，R16 为 #2786 占用
（gh pr view 文件清单截断，实以 `git ls-tree` 远端 ref 为准）。本树全部 refs
对 R17 无命中，本件取 R17，不与任何已知 ref 碰撞；缺号如实记于此，若 R17 车道
后到导致撞号按账本规则出更正件（#2960 R15 先例同法）。编号取舍 AI 代批，
待人复核（任务书原指 R14，开 PR 时 R14 已双占，为免合并队列 add/add 撞
`enqueue-check-queued-same-path` 改取首个未占号）。

## TS 依据

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`（`resources/PilotDeck`，
只读），Node v22.22.0 / ICU 78.2。submit 链
`workspaceIdResolver(input.projectKey)` →
`resolveProjectStorageId(requestedProjectKey ?? fallbackProjectRoot, pilotHome)`
（`InProcessGateway.ts:954`、`createLocalGateway.ts:1026-1028`）。

- 协调者 submit 级探针（主仓
  `_tmp/20261111-issue-2617-probe/probe-submit-surrogate.out`）：
  `lone-surrogate`（`"\ud800"`）→ ok，`"Users-da-ws-pilotdeck-rs"`
  （**进程 cwd 的 slug**）；`lone-surrogate-fallback`（`"\ud800sub"`）→
  `"Users-da-ws-pilotdeck-rs-sub"`；`ascii-control`（显式 fallback 临时项目）→
  `"var-folders-…-T-p2617-probe-0TzGDg-fallback-project"`（与 cwd slug 不同，
  证明孤立代理串不落 fallback 项目）。
- resolver 级只读复跑（主仓
  `_tmp/20261111-2617-evidence/probe-submit-surrogate-resolver.mjs` /
  `.out`，worker cb8，同一 `paths.ts` resolver 链）：
  `"\ud800"` 照常 resolve、无异常（`resolved` = cwd slug；其
  `loneSurrogateResolvesLikeFallback:true` 因脚本 `fallbackProjectRoot =
  process.cwd()` 为构造恒真，仅证「落到 cwd slug」，不证「落到配置项目」——
  本件勘误的原始依据）。
- TS 侧不抛错：`path.resolve` 收任意串（R13 :11-:13 已登记同族：空白 /
  BOM-only / 孤立代理项串都是真键）。

## Rust 行为

修前（基线 `3ca4e1f39f`）：`services.rs` `workspace_id` 的 projectKey
字符串分支 `as_string + to_utf8().ok()`，非 UTF-8 串键（孤立代理项）答
`gateway_submit_failed` + `projectKey must be a valid path string.`（#2562
nullish 读与类型门不动：null/缺键回落配置项目，非串类型照旧类型错）。

修后（`243f9f3da`，fix-r1 后 `services.rs:1535`）：字符串分支
`String::from_utf16_lossy(project.units())`（`session_dir.rs` #2083 同一
转换），随后既有 `Options::serves_project` 原始路径恒等判定不动：未服务键
（含孤立代理项的 lossy 形）答 `CAPABILITY_UNAVAILABLE` + `The isolated
native process serves only its configured project.`。残余（residual，#2083
已登记族）：键的 lossy 形命中配置项目（真实路径含 U+FFFD）时被服务，答该项目
workspace id。

## 范围

覆盖：submit 路径 `Services::workspace_id` 的 `projectKey` 字符串分支一格
（非 UTF-8 串键），及其机器判据（下节）。#2882 三读点中本件只关
`services.rs` submit 门；`module_host.rs` `public_skills_create_input` 与
`config.rs` `seed_read_state` 两读点仍在 #2882 待办。**同键两族两判（登记
在案，不改）**：`management.rs` `admit_management_project` /
`snapshot_restore` 的 `targetProjectKey` 门与 `assembly/memory.rs` 的
`serves_project` 仍是 strict `to_utf8`（R8:28 / R13 已登记的 deferred，
#2562 任务书 P3 记录在案）——残留键（lossy 形命中配置项目）在 submit /
close / config 侧被服务、在这些门仍被拒。不放宽判分，不改
#2562/#2272 已钉的 nullish 回落格与 #2083 已登记的入口层叠。

## 机器判据

行号为 fix-r1 提交上的行号（交付提交 `243f9f3da` 之后）：

- 用例（`management_tests.rs`）：
  `submit_workspace_id_surrogate_project_key_reaches_the_capability_gate`
  （`:2591`）两格——拒绝格：`JsString::from_utf16([0xd800])` 键断
  `error.code == "CAPABILITY_UNAVAILABLE"` + 隔离报文逐字（含
  「not the type error」前置断言钉住非 #2562 类型错）；放行格：residual
  键（配置路径 + `-pd-` + U+FFFD 位为 0xD800）断 `Ok(Some(workspace id))`，
  且 served id 逐字等于
  `SnapshotProjects::new(home, cwd=空).workspace_id(residual)`（fix-r1
  评审 P2 处置：放行格有鉴别力，不只是一段合法 UTF-8 字符串）。
- 负控（`tests/fixtures/native-config/negative_controls.py` 新增两行）：
  `submit-surrogate-project-key-rejected`（lossy 转换变异回
  `to_utf8().ok()` 拒绝门，绑上述用例 code 断言）与
  `submit-serve-gate-removed`（能力门 `if false &&` 失效，绑
  「not the type error」前置断言）。`--check` →
  `#1457 82 mutation anchors OK; #2065 69 literal anchors OK`；正式全表
  82/82 KILLED、0 SURVIVED（提交后本机分轮跑完，证据
  `_build/wt-2617/_tmp/20261111-issue-2617/negctl-full-local*.log` 与
  `negctl-full-r5-a*.log`；变异体 2 的杀死载荷 `Some([97,56,45,109,97,105,110])`
  = setup 注册项目 id，证能力门是唯一拒点）。
- 回归：`submit_workspace_id` 过滤 2/2（#2562 格）、`native_process::`
  模块 391/391、gateway lib 全量 5371 passed（worker cb8 原始输出
  `cargo-test-no-run.out` / `gateway-lib-full-postfix.out`，2 例环境性
  flake 隔离复跑过）。
- 账本闸门：`diffs/` 只追加（本件为新增文件，R13 及更早字节未动），
  `check_frozen.py --base origin/main` PASS（输出 30 entries，该计数指冻结
  正文件的条目；`--print-ledger` 合并视图已含本件）。

## 批准记录

未批准，非已接受差异，待人复核（可否决）。2026-11-11，#2617 实现单落地后
AI 代登记（同轮更正 r1 注释的「落到配置项目」表述，评审 DS-sibling P1 /
MM-boundary P1 的处置；R17 编号取舍见「类别」R 号口径，AI 代批待人复核）。
差异本体为对齐性修复（非 UTF-8 串键不再吃 submit 特有的类型错、改走与
#2083 同族的能力门判定），不新增豁免、不改判分语义、不动 #2562
nullish/类型门。人复核若否决（如裁定 submit 门应按 storage-id identity
对齐 TS、或 cwd 落点应另行登记），循账本规则出 `-R18` 更正。
