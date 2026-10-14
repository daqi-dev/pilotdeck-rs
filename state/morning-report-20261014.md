# 晨报 · 2026-10-14 夜跑

> 02:30 开始，07:45 收工（之后没再派新活）。每一处「AI 代批，待人复核」都列在第二节，**请逐条看**；挂起的事在第三节。

## 一、结果一览

**一句话**：阶段 C（agent 循环 Rust sidecar）**51 / 53**，比第一个分数多 1；阶段 A、B、前端各做出了探针与尺子，但 **5 个 PR 卡在合并闸门「两轮评审不收敛就挂起」**，整体重构**没有跑完**。离「把前端做完、整体完成重构」还很远——夜里能自主推进的，主要是尺子、探针和阶段 C 的分数。

| 阶段 | 状态 | 关键数字 | 在哪看 |
|---|---|---|---|
| **C · agent 循环 sidecar** | 已合并，**51/53** | k=5、第 i 遍 Rust 对第 i 遍 TS 5 对全 PASS；未过：`deadline` 0/5、`deadline_during_tool` 1/5（挂起，#19） | `docs/experiments/exp005-first-score/`、`_tmp/20261014-reg2/` |
| C · 精简 S1 | **挂起**（PR #34） | 删未接线 typed reducer，**净减 1579 行**，两家评审无代码问题；闸门第 1 项被 `cancel_during_tool` 原版不稳定挡住（#35） | PR #34、#35 |
| **A · gateway** | A1 已合并；**A2、A3 挂起** | A1 透明代理连通真 `ui/server`；A2 尺子机器项全 true（TS 自比 10 对、代理一致、33 项对照）；A3 101 method schema 进 CI | PR #16（已合）、#31、#30 |
| **B · ui/server** | **B1 挂起**（PR #29） | Rust 反代零差异，路由 263/264，11 项阳性对照 | PR #29 |
| **前端** | **F1 挂起**（PR #32） | 真实 Chrome：两候选 IME/xterm/流式/滚动无差别；gzip wasm Leptos 111 KB vs Dioxus 210 KB → 暂选 Leptos | PR #32 |

**02:10 起合并进 main 的 PR（13 个）**：#6 #8 #9 #11（规则与授权）· #12（搬入旧 Rust sidecar）· #13（尺子 G03 v1 冻结）· #14 #20 #22（A/B/前端调研与计划）· #16（A1）· #17（晨报 03:40 版）· #26（第一个分数 50/53）· #27（C4：budget 修复 → 51/53）。

**一个更正**：06:45 我写「`cancel_during_tool` 回到 5/5 佐证偶发 FAIL 来自撤出的 ACK 宽限改动」——**不成立**。之后 S1 回归里 TS 自己也出现了同一个罕见结果（TS 1/15、Rust 1/15，ACK 36 vs 常态 34），这是原版不稳定，k=5 标定没撞上（#35）。C4 的合并依据（逐场景不退化）不受影响。

## 二、AI 代批，待人复核

| # | 时间 | 代批了什么 | 依据 | 推翻的话要做什么 |
|---|---|---|---|---|
| 1 | 02:30 | 「前端也一并重构」理解为**改成 Rust** | 本项目方向是 TS → Rust | 改 `.42cog/intent.md` 不做什么表里那一行 |
| 2 | 02:35 | issue #10（搬旧 Rust 代码）与 #7（尺子）**并行**，偏离计划「#7 合并后再开」 | #10 只搬代码、构建、跑自带测试、握手冒烟，不依赖尺子 | 无需动作；#10 的结果不受尺子影响 |
| 4 | 02:55 | **阶段可并行**：A 现在就开工、B 开始调研，不等 C 达标（人随后授权 AI 代做全部判断） | 各阶段替换的进程不同（C = sidecar、A = gateway、B = `ui/server`），对拍互不依赖；每阶段合并前仍须自己的对拍达标 | 改回意向书原红线；停掉 A/B 的 worktree |
| 5 | 02:57 | Rust 判法的配对：**第 i 遍 Rust 对第 i 遍 TS，5 对全一致才 PASS**；另报 25 对矩阵 | 冻结规约只写了「k 遍全一致」，没写 Rust 与哪遍 TS 配 | 改 `acceptance.md`，用已存的 trace 离线重算，不用重跑 |
| 6 | 03:00 | PR #12 的评审范围收窄到**适配改动**（429 行），原样搬入部分由协调者逐字节核对 | 117 个搬入文件与旧源 `0eba113` 逐字节一致 0 差异 | 无 |
| 7 | 03:09 | 阶段 A 草稿两轮评审后剩余问题由**协调者直接改**，未开第 3 轮 | 剩余项均为具体文档改动（5 个 method 改待定、阶段门、桌面、exp004 误用） | 看 PR #14 的最后一个提交 |
| 8 | 03:09 | `skill_list/read/validate/scan`、`read_subagent_messages` 从 (a) 改**待定**（透传），(a) 类 15 → 10 | 两家评审分歧，取保守 | 改 `docs/research/005_phase-a-gateway/method-inventory.md` |
| 9 | 03:09 | 阶段 A **不验收桌面端** | 桌面壳自己起 TS gateway（`apps/desktop/src/main.ts:250-258`） | 改 005 decision #7 |
| 10 | 03:12 | 开工阶段 A 的 A1（exp003，issue #15），与阶段 C 并行 | 授权扩大后的代拍 #4 | 关 issue #15 |
| 11 | 03:20 | PR #12（旧 Rust sidecar 搬入）合并：两轮评审到无 P0/P1/P2；3 条 P3（`--version` 不标 dirty、board 状态过时、git 版本依赖）登记不修 | 闸门全过 | revert PR #12 |
| 12 | 03:21 | 阶段 B、前端草稿两轮评审后的剩余问题交原作者改一次，协调者核对，不开第 3 轮 | 两轮上限；问题均具体 | — |
| 13 | 03:32 | **尺子冻结**：`acceptance.md` / `normalization.md` **AI 代批**；FROZEN 登记全部规约 + 11 份比较器代码；新增「两侧输出路径必须一字不差」硬前提与 Rust 配对规则 | `check_ruler` 全项 true；PR #13 两家两轮无 P0/P1/P2；协调者 9 种独立变异全 FAIL | 改 `specs/parity/acceptance.md` 等并重新登记 FROZEN；trace 都在，离线重算 |
| 14 | 03:30 | PR #13 第 2 轮评审剩余 P2（锁无心跳）与 P3 由**协调者直接修** | 两轮上限 | 看 `c59c126` |
| 15 | 03:35 | A1 场景 4（断线自动重放）FAIL **归因基线 bridge**，登记原版缺陷，**阶段 B 按「默认修」处理**；阶段 A 不修（`ui/server` 不改） | exp003 补充驱动保存旧 binding 后两侧都能恢复 | 改 exp003 结论 |
| 16 | 03:56 | 3 个未过场景**分诊为 Rust 产品问题**（deadline 终态引发宿主对账失败；budget 终态给人读文本而非原因码），开 #19 修 | exp005 差异逐条 | 改 exp005 分诊 |
| 17 | 04:10 | deadline ×2 的「宿主多写 `gateway_submit_failed`」判**原版缺陷**（TS 宿主竞态 `InProcessGateway.ts:1727` 未排除 `timedOut`，与基线测试 `operation-deadline.spec.ts:159-160` 矛盾），进 `accepted-diffs.md`（AI 代批），**阶段 A 替换 gateway 时修**；Rust 侧的收尾语义差异照修；**不加人为延时** | codex 用原版 TS 宿主独立复现了竞态 | 撤 accepted-diffs 两行 |
| 18 | 04:22 | 实验编号**撞车三次**（A2、F1、B1 的计划都写了 exp006）：A2 = **exp006**、F1 = **exp007**、B1 = **exp008**，计划里的引用随各自 PR 改 | 追加式编号不能重 | — |
| 19 | 04:25 | 并行 worker **划端口段**：A2 → 287xx、B1 → 130xx / 288xx、F1 → 33xx / 289xx；绑定前先查空闲 | 多个 worker 同时起服务会撞端口 | — |
| 20 | 04:20 | F1 / B1 用**本机已装的 Chrome**（Playwright `channel: chrome`，临时用户目录在 worktree），**不下载 Playwright 浏览器** | 夜里人不在，下载新的二进制按规矩要先问人 | 早上你可以决定是否装 Playwright 自带浏览器 |
| 21 | 04:20 | 为 F1 装了 Rust 的 `wasm32-unknown-unknown` 编译目标（`rustup target add`） | codex 沙箱写不了 `~/.rustup` | `rustup target remove wasm32-unknown-unknown --toolchain 1.99` |
| 22 | 04:42 | **推翻 #17**：两家评审对「原版缺陷」归类分歧（GLM 站得住 / DeepSeek 站不住），**采纳更严的 DeepSeek**——真实 TS 流程 0/10、Rust 9/10，唯一变量是 sidecar；且上一轮「ACK 宽限」修法与 TS（到点立即 reject 在途调用）不一致。deadline ×2 **维持 FAIL、不豁免**，第 3 次（最后一次）尝试把 Rust deadline 语义对齐 TS | 错放 Rust 问题为原版缺陷 = 免检；反过来只是多花时间 | 若你认可「原版缺陷」归类，改回 #17 |
| 3 | 02:46 | 阶段 A 调研编号用 **005**（004 预留给计划 001 的模块对应表） | 计划 001 已写定 004 | 无 |
| 23 | 05:05 | **C4 拆分**：PR #27 只保留 budget 修复，deadline / 取消 ACK 宽限改动撤出；第 3 轮改动留档 `_tmp/20261014-c4/r3-worktree.patch` | deadline 三轮未修好须挂起；budget 修复独立有效 | 从 r3 补丁重开 deadline 修复 |
| 24 | 05:15 | ~~F1 选定 Leptos（依据：Dioxus IME FAIL）~~ **06:40 作废**：那是探针 bug（Playwright `dispatchEvent` 造普通 `Event`），评审拦下 | — | — |
| 24' | 06:40 | **F1 暂选 Leptos**，依据只剩 wasm 体积（同管线 gzip 111 KB vs 210 KB）；IME 等已测项两候选无差别 | 真实 Chrome 诊断两候选 4/4 PASS | 改 `docs/research/007_frontend/decision.md` |
| 25 | 06:40 | **开精简 S1**（#33）：删未接线 typed `model_event_reducer` 与 `driver.rs` 双轨 | 研究 004 膨胀候选 #3；codex 先证无产品接线 | 关 PR #34 |
| 26 | 06:45 | 组合输入时按 Enter **跟基线、不 `preventDefault`** | DeepSeek 核到基线 `useChatComposerState.ts:1772` 输入法 Enter 直接 return | 改探针断言与候选 handler |
| 27 | 07:30 | `cancel_during_tool` 判 **原版不稳定**（未改尺子，只开 #35 提请重标定）；**S1 不挑选重跑、不合并** | 三轮回归 TS 1/15、Rust 1/15 同一罕见结果 | 在 #35 定重标定方案 |

## 三、挂起的事（失败 3 次 / 撞红线 / 需要人）

> **拍板用这份**：[`state/decisions-20261014.md`](decisions-20261014.md)——每件事卡在哪、选项与后果、AI 建议。下表只是索引。

> 协调者的一次失误：03:30 修 PR #13 时**在一条测试仍失败的情况下提交了**，随即发现、修好并 amend（未推送前）。

| # | 什么事 | 卡在哪 | 建议 |
|---|---|---|---|
| ~~1~~ | ~~阶段 A 草稿提出「A1–A3 可与阶段 C 并行」~~ | 人随后授权 AI 代做全部判断 → 已由代拍 #4 解决 | — |
| 2 | **deadline 竞态**（#19，阶段 C 最后 2 个场景） | codex 三轮未修好：Rust 3/5 多写 `gateway_submit_failed`、`lastAppliedSequence` 恒少 1 | 看 r3 补丁思路是否值得再试；或者由人裁定「原版缺陷」归类（代拍 #17 / #22 的分歧） |
| 3 | **B1**（PR #29） | 两轮未收敛：codex 余 3 条 P2（归一化按键名全局；ETag 强弱 / revision 头改值可 PASS；WS 转发未按 Connection 提名剥逐跳头） | 放第 3 轮（都是具体修法），或接受为已知限制；**B2 依赖它** |
| 4 | **A3**（PR #30） | 两轮未收敛：各余 1 条 P2（fixtures 为空可零例全绿 / 别名用例 4 of 18；基线 job 不是必需检查） | 很便宜，建议放第 3 轮；基线 job 加进分支保护的必需检查要你点头 |
| 5 | **A2**（PR #31） | 两轮未收敛：GLM 判 true，DeepSeek 判 false（P1：`normalization.md` 没随收紧同步） | 修文档即可，建议放第 3 轮；**A4 依赖它** |
| 6 | **F1**（PR #32） | 两轮未收敛：两家同一条 P2（`browser.mjs:55` 断言与合成 composition 口径不符，一行修法写在 PR 里） | 放第 3 轮；OS 输入法要你亲手测（见第五节） |
| 7 | **S1**（PR #34） | 闸门第 1 项：`cancel_during_tool` 4/5（原版不稳定，#35） | 先在 #35 定重标定，再按新口径判 S1 |
| 8 | **护栏越线（worker）** | F1 的 codex 夜里下载了 Caddy 2.10.2 官方二进制（SHA512 校验，只在 127.0.0.1 跑），违背「夜里不下载新二进制」的本意 | 决定 `_build/wt-f1/_build/tools/bin/caddy` 删留 |
| 9 | **护栏失误（协调者）** | A2 推送前 gitleaks 报 1 处，命令链吞了退出码，照样推了。核实是误报（RFC 6455 公开示例 nonce），已重写**功能分支**历史，main 没动 | 已装 pre-push hook 兜底；旧提交 bfb1fb4 按 SHA 仍可见（只含公开示例值） |

## 四、方法缺口（夜里新撞上的）

- ⑰ **假通过（夜里第一次抓到）**：issue #10 的 worker 报「握手冒烟通过、sidecar 身份是 Rust」——**不成立**。上游 adapter 在 sidecar 模式把 `PILOTDECK_AGENT_LOOP_SIDECAR_COMMAND` 覆盖成 `process.execPath`（`resources/PilotDeck/tools/agent-loop-parity/adapters/pilotdeck_gateway_impl.mjs:566-570`），实际跑的是 TS；「implementation=rust」来自另一次单独的 `--version`。协调者用 #7 的驱动（经生产 YAML 模块绑定）独立核实了真正的 Rust 握手（启动命令 = Rust 二进制、sha256 匹配、带生产路径参数）。**作者自证不能替代独立核实**——与旧迁移「把无关 PID 记成了自己」同一类。
- ⑱ worker 在沙箱里**提交不了**：worktree 的 git 元数据在主仓 `.git/worktrees/` 下，在可写根之外。三家都撞上；codex、DeepSeek 都没有绕过，停下交接（这是对的）。后续给 codex 加 `-c 'sandbox_workspace_write.writable_roots=["/Users/da/ws/pilotdeck-rs/.git"]'`。
- ㉑ **数据丢失（协调者疏忽）**：`gh pr merge --delete-branch` 删除本地分支时**把对应 worktree 整个删掉了**，连同未入库的 `_tmp/`。丢了 `wt-issue7/_tmp`（最早 5 遍路径不一致的 Rust trace——本已作废；worker 交接报告；可重生的 `self-compare.json`）与 `wt-issue10/_tmp`（冒烟与构建日志）。关键数据都在：TS 原料在主仓 `_tmp/20261014-ts5`，评审导出副本在 `_tmp/review-pr*/`，结论都在入库文档里。**补救**：03:38 备份了仍在的 3 个 worktree 的 `_tmp`（`_tmp/archive/`），exp003 证据另放主仓 `_tmp/20261014-exp003/`；**此后合并不用 `--delete-branch`**，远程分支单独删，worktree 先备份再回收。
- ⑳ **第一个分数 4/53 是测量假象**：Rust 与 TS 的输出目录不同，system prompt 里嵌的工作目录长度差 34 字符 → token 预算数字不同；49 个失败里 44 个只差 token 预算字段。路径对齐只替换了文本、改不了已经按原路径算好的 token 数。**修法：TS 与 Rust 用同一个 label 跑、跑完改名**，两边路径一字不差（`_tmp/ruler-snap/`，03:20 起重跑 5 + 5 遍）。这是同一类错第二次出现（第一次见 exp001 补充）——已写进待办：`specs/parity/normalization.md` 要明文规定「两侧输出路径必须相同」。
- ⑲ 协调者自己写的对照**第一次错了两个**（改到了元数据字段 `q`、用错了权限字段名），差点误判尺子漏判。修正后 9 种独立变异全部 FAIL。

- ⑭ **协调者绕开了课程的能力编排**：起初直接派 codex 干活，没走 `dev-req` / `pr-ready` / `simplify`。02:50 补上合并闸门（CI → simplify 四视角 → pr-ready 三角色 → 双谱系评审，审的一律不是写的那家）。
- ⑮ `dsh`（DeepSeek Harness）的 headless 模式没有只读沙盒，只能靠提示词约束、跑完核对 `git status`。
- ⑯ 夜里人不在，课程里人不可替代的两步——**实效层验证**与**定标「我亲手改的那一处」**——做不了，见第五节。

- ㉒ **两轮闸门遇到两家分歧（一家 true 一家 false）没有仲裁规则**：今晚一律按「不收敛」挂起，结果 A/B/前端的 5 个 PR 全停。建议人定：分歧时引第三家（codex）裁决，还是维持挂起；或者区分「代码 P2」与「文档 / 口径 P2」。
- ㉓ **协调者自己的证据也要过评审**：F1 那次「Dioxus IME FAIL → 选 Leptos」是我在沙箱外补跑得出的，评审一眼看出是探针 bug。补跑不等于独立核实。
- ㉔ **k=5 标定抓不住低频原版不稳定**：`cancel_during_tool` 约 7% 的频率下，5 遍全中常态的概率约 0.7。课程的「k 遍全一致」没有说 k 怎么按频率选；精简类改动会被它随机挡住。
- ㉕ **红线要按类别写，别按举例写**：「夜里不下载新二进制（Playwright 浏览器）」被 worker 读成只禁 Playwright，于是下了 Caddy。
- ㉖ **「推送前 gitleaks」要靠机器，不能靠人记**：已改成 pre-push hook。
- ㉗ **codex 沙箱 05 点起拒绝 loopback listen**（之前可以）：所有对拍 / 浏览器门改由协调者在沙箱外跑，worker 只能交「BLOCKED」。worker 交 BLOCKED 而不是伪造 PASS，是对的。

## 五、要你亲手补的（课程里 AI 不能替代的部分）

1. **实效层验证（每阶段亲手用一次）**
   - 阶段 C：用 `PILOTDECK_AGENT_LOOP_TRANSPORT=stdio` + `PILOTDECK_AGENT_LOOP_SIDECAR_COMMAND=<main 编出的 sidecar>` 起你自己的 PilotDeck，**真实模型**跑一次日常对话 + 一次工具调用 + 一次中途取消，看有没有体感上的不同（尺子只用 mock 模型）。
   - 阶段 A1：浏览器经 Rust 透明代理连 gateway，点几下会话列表、发一条消息（exp003 的命令在 readme 里，用高位端口，别碰你自己的 18789）。
   - 前端 F1：在**真实输入法**（中文拼音）下，两个探针各打一句话、按回车，看组合期间会不会误发——这是浏览器自动化做不到的。
2. **定标「我亲手改的那一处」**：从第二节挑一条你最不认同的代拍（我猜是 #17/#22 的 deadline 归类、#24' 的 Leptos，或 #27 的原版不稳定判定），亲手改掉，看后续流程是否跟着正确调整。
3. **要你拍板的**：第三节第 2–8 行；以及 `.42cog/intent.md` 里阶段 C 的分母要不要因 #35 改成「53 − 原版不稳定 1」。

## 附：夜间时间线（追记原文，按时间）

### 05:00 追记
- **回归（二进制 8e7326cf，k=5 第 i 对第 i）**：50/53 五对全 PASS。budget 场景 5/5（已修）；deadline 4/5、deadline_during_tool 1/5（C4 在修）；**cancel_during_tool 1 次 FAIL（第 4 遍，E10/E11/通道）**——rust-r4 对五遍 TS 全 FAIL、TS 互比全 PASS，判为 Rust 侧偶发竞态（ACK 36 vs 34），不是尺子问题。C4 合并后重跑时专门看此场景，若再现就开 issue。原件 `_tmp/20261014-reg1/`、`_tmp/20261014-score3/`。
- **流程失误（已纠正）**：A2 推送前 gitleaks 报 1 处，但命令链用 `| tail` 吞了退出码，照常推送并开了 PR #31。核实为误报（`driver.mts` 里 RFC 6455 §1.3 公开示例 nonce），已加 `gitleaks:allow`、重写**功能分支**历史并强推（main 未动）；旧提交 bfb1fb4 在 GitHub 按 SHA 仍可见，内容仅公开示例值。之后 gitleaks 一律单独检查 `$?` 再推。→ 方法缺口：课件「推送前 gitleaks」没有给出可机器强制的写法（应放进 pre-push hook 而非靠人记）。

### 05:30 追记
- **挂起 · B1（PR #29）**：两轮双谱系评审未收敛（第 2 轮 GLM 无 P2；codex 仍 3 条 P2：归一化按键名全局、ETag 强弱/revision 头改值可 PASS、WS 转发未按 Connection 提名剥逐跳头）→ 按闸门不合并，PR 打「挂起」标签并留评论。B1 的核心结论（反代零差异、路由 263/264）不受影响，但比较器严格度未过闸，B2 不能依赖它。**要人决定**：允许第 3 轮，还是接受这 3 条为已知限制。
- **挂起 · C4 的 deadline 竞态（issue #19）**：codex 三轮未修好（第 3 轮冻结比较器下 deadline 2/5、deadline_during_tool 2/5，Rust 仍 3/5 多写 gateway_submit_failed，lastAppliedSequence 恒少 1）。第 3 轮改动留档 `_tmp/20261014-c4/r3-worktree.patch`。**AI 代拍 #23**：PR #27 拆分——只保留 budget 修复，撤出 deadline/取消 ACK 宽限改动（回归显示 e868d6a 让 `cancel_during_tool` 新出现 1/5 FAIL，嫌疑在此），codex 在拆。accepted-diffs #1 #2 的依据改按「Rust 产品问题」更正（追加行，待批）。
- **F1（PR #32）**：协调者在沙箱外用本机 Chrome（临时 profile）补跑浏览器门：Leptos 全 PASS；Dioxus 组合输入期间 Enter 仍发送 → FAIL。~~AI 代拍 #24：选定 Leptos~~ **（05:40 作废，见下）**。F1 不按完整验收关闭（OS 输入法、37 例黑盒、跨壳 storage/BFCache、HMR 未测）。
- **护栏越线（worker）**：F1 的 codex 夜里从 Caddy 官方 GitHub release 下载了 `caddy_2.10.2_mac_arm64`（SHA512 校验后解压到 worktree `_build/tools/bin`，只在 127.0.0.1 上跑过）。违背「夜里不下载新二进制」的本意（任务里只点名了 Playwright 浏览器，没写全）。之后不再使用；文件留在 `_build/wt-f1/_build/tools/bin/caddy` 待人决定删留。→ 方法缺口：红线按「类别」写（任何预编译二进制），别按举例写。
- **新护栏**：本地装 `.git/hooks/pre-push`，gitleaks 不过即拒推（所有 worktree 共用）。

### 05:40 追记
- **更正 · 我自己的错误结论**：F1 评审（DeepSeek P1、GLM P1）指出 Dioxus 的「IME 失败」是探针 bug。协调者实测确认：Playwright `locator.dispatchEvent('compositionstart')` 在 Chrome 中构造普通 `Event`，Dioxus 只接收 `CompositionEvent` 实例 → handler 不触发。**AI 代拍 #24 作废**，PR #32 已留更正评论；codex 第 2 轮改用真 `CompositionEvent` 重测。→ 方法缺口：协调者自己补跑的证据也必须过双谱系评审再进取舍——这次正是评审拦住了我的错判。
- **挂起 · A3（PR #30）**：第 2 轮两家无 P0/P1，各余 1 条 P2（fixtures 为空可零例全绿/别名用例只覆盖 4 of 18；基线 job 不是必需检查）→ 按闸门挂起。两条都便宜，**要人决定**是否放第 3 轮。

### 06:05 追记
- **挂起 · A2（PR #31）**：第 2 轮 GLM independent_review=true；DeepSeek=false（P1：`normalization.md` 口径没随收紧同步、与 compare.py 实现相反；P2：startedAt/completedAt 仍按单 marker 抹、simplify/pr-ready 表缺）。机器项全 true。→ 按闸门不合并、不冻结。**阶段 A 至此卡在闸门**：A2/A3 都挂起，A4 依赖 A2 不能开。
- **C4 拆分（PR #27，22fd3ef）**：transport 三文件与 ports.rs/cancellation.rs 已逐字节恢复为 main，turn.rs 只留 budget 修复；697 测试过。新二进制 `e53d772e…` 全量 k=5 回归在跑（`_tmp/20261014-reg2/`）。codex 沙箱 05 点起拒绝 loopback listen，冒烟由协调者在沙箱外跑。
- **F1 第 2 轮**：codex 已改用真 `CompositionEvent` + isComposing/keyCode229 路径、data-seq 对账、滚动前置断言；readme/decision 已撤回「Dioxus IME FAIL」，暂选 Leptos 的依据退回体积（AI 代批待复核）。离线 DOM 两候选 PASS；Chrome 门待回归结束后由协调者跑（避免 CPU 争用扰动 deadline 类时序场景）。
- → 方法缺口：两轮闸门 + 两家意见分歧时（一家 true 一家 false），课程没有仲裁规则；今晚一律按「不收敛」处理，导致 A 阶段整体停住。建议人定：分歧时引第三家（codex）裁决，还是维持挂起。

### 06:45 追记
- **合并 · C4（PR #27 → main e68b8c4）**：拆分后两家评审（GLM、DeepSeek）均无 P0/P1/P2，CI 绿。**全量回归（二进制 e53d772e，k=5 第 i 对第 i）：51/53 五对全 PASS**（main 为 50）；budget 5/5；`cancel_during_tool` 回到 5/5（佐证上一版的偶发 FAIL 来自撤出的 ACK 宽限改动）；剩 `deadline` 0/5、`deadline_during_tool` 1/5。原件 `_tmp/20261014-reg2/`、`_tmp/20261014-score4/`。issue #19 被 PR 自动关闭后已**重新打开并标「挂起」**。
- **阶段 C 当前分数：51/53**（分母 53 − 原版不稳定 0 = 53）。
- **F1 r2（真实 Chrome，协调者跑）**：原样 `browser.mjs` 两候选同在 `:55` FAIL——组合内真实 Enter 两边都没发送，但合成 composition 不吞默认动作、浏览器插入换行；诊断副本（只把该断言改为记录值，未入库）两候选 4/4 PASS。→ IME 维度两候选无差别，**暂选 Leptos 的依据只剩 wasm 体积（AI 代拍 #24'，待人复核）**。第 2 轮评审在跑。→ 待人定的产品语义：组合内 Enter 是否要求前端 `preventDefault`。

### 06:50 追记
- **挂起 · F1（PR #32）**：第 2 轮两家均无 P0/P1，**同一条 P2**：`browser.mjs:55` 断言与合成 composition 口径不符（一行修法已写在 PR 评论）。按闸门挂起。DeepSeek 核到基线 `useChatComposerState.ts:1772` 输入法 Enter 不 preventDefault → 前面「待人定的产品语义」有了依据：**跟基线，不补 preventDefault**（AI 建议，待复核）。
- **新开 · 精简 S1（issue #33，`_build/wt-slim1`）**：删未接线 typed `model_event_reducer` 与 `driver.rs` 双轨（AI 代拍 #25）。codex 先证无产品接线，否则停；对拍由协调者跑。
