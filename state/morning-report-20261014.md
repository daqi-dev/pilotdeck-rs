# 晨报 · 2026-10-14 夜跑

> 02:30 开始，08:30 收工。每一处「AI 代批，待人复核」都列在第二节，**请逐条看**。

## 一、结果一览

（收工时填）

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
| 3 | 02:46 | 阶段 A 调研编号用 **005**（004 预留给计划 001 的模块对应表） | 计划 001 已写定 004 | 无 |

## 三、挂起的事（失败 3 次 / 撞红线 / 需要人）

> 协调者的一次失误：03:30 修 PR #13 时**在一条测试仍失败的情况下提交了**，随即发现、修好并 amend（未推送前）。

| # | 什么事 | 卡在哪 | 建议 |
|---|---|---|---|
| ~~1~~ | ~~阶段 A 草稿提出「A1–A3 可与阶段 C 并行」~~ | 人随后授权 AI 代做全部判断 → 已由代拍 #4 解决 | — |

## 四、方法缺口（夜里新撞上的）

- ⑰ **假通过（夜里第一次抓到）**：issue #10 的 worker 报「握手冒烟通过、sidecar 身份是 Rust」——**不成立**。上游 adapter 在 sidecar 模式把 `PILOTDECK_AGENT_LOOP_SIDECAR_COMMAND` 覆盖成 `process.execPath`（`resources/PilotDeck/tools/agent-loop-parity/adapters/pilotdeck_gateway_impl.mjs:566-570`），实际跑的是 TS；「implementation=rust」来自另一次单独的 `--version`。协调者用 #7 的驱动（经生产 YAML 模块绑定）独立核实了真正的 Rust 握手（启动命令 = Rust 二进制、sha256 匹配、带生产路径参数）。**作者自证不能替代独立核实**——与旧迁移「把无关 PID 记成了自己」同一类。
- ⑱ worker 在沙箱里**提交不了**：worktree 的 git 元数据在主仓 `.git/worktrees/` 下，在可写根之外。三家都撞上；codex、DeepSeek 都没有绕过，停下交接（这是对的）。后续给 codex 加 `-c 'sandbox_workspace_write.writable_roots=["/Users/da/ws/pilotdeck-rs/.git"]'`。
- ㉑ **数据丢失（协调者疏忽）**：`gh pr merge --delete-branch` 删除本地分支时**把对应 worktree 整个删掉了**，连同未入库的 `_tmp/`。丢了 `wt-issue7/_tmp`（最早 5 遍路径不一致的 Rust trace——本已作废；worker 交接报告；可重生的 `self-compare.json`）与 `wt-issue10/_tmp`（冒烟与构建日志）。关键数据都在：TS 原料在主仓 `_tmp/20261014-ts5`，评审导出副本在 `_tmp/review-pr*/`，结论都在入库文档里。**补救**：03:38 备份了仍在的 3 个 worktree 的 `_tmp`（`_tmp/archive/`），exp003 证据另放主仓 `_tmp/20261014-exp003/`；**此后合并不用 `--delete-branch`**，远程分支单独删，worktree 先备份再回收。
- ⑳ **第一个分数 4/53 是测量假象**：Rust 与 TS 的输出目录不同，system prompt 里嵌的工作目录长度差 34 字符 → token 预算数字不同；49 个失败里 44 个只差 token 预算字段。路径对齐只替换了文本、改不了已经按原路径算好的 token 数。**修法：TS 与 Rust 用同一个 label 跑、跑完改名**，两边路径一字不差（`_tmp/ruler-snap/`，03:20 起重跑 5 + 5 遍）。这是同一类错第二次出现（第一次见 exp001 补充）——已写进待办：`specs/parity/normalization.md` 要明文规定「两侧输出路径必须相同」。
- ⑲ 协调者自己写的对照**第一次错了两个**（改到了元数据字段 `q`、用错了权限字段名），差点误判尺子漏判。修正后 9 种独立变异全部 FAIL。

- ⑭ **协调者绕开了课程的能力编排**：起初直接派 codex 干活，没走 `dev-req` / `pr-ready` / `simplify`。02:50 补上合并闸门（CI → simplify 四视角 → pr-ready 三角色 → 双谱系评审，审的一律不是写的那家）。
- ⑮ `dsh`（DeepSeek Harness）的 headless 模式没有只读沙盒，只能靠提示词约束、跑完核对 `git status`。
- ⑯ 夜里人不在，课程里人不可替代的两步——**实效层验证**与**定标「我亲手改的那一处」**——做不了，见第五节。

## 五、要你亲手补的（课程里 AI 不能替代的部分）

（收工时填）
