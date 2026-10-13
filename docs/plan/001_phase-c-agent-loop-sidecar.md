---
type: plan
number: "001"
date: 2026-10-14
title: 阶段 C：agent 循环 Rust sidecar——回捞尺子、搬入旧代码、量分、对应检查
tags: [phase-c, sidecar, parity, reuse]
status: in-progress
related: [changes/arch/001_phase-c-agent-loop-sidecar, research/002_parity-boundary, research/003_prior-attempt]
---

# 阶段 C 计划

## Context

为什么这样设计见 `docs/changes/arch/001_phase-c-agent-loop-sidecar.md`。一句话：在 TS gateway 不改一行的前提下，用环境变量换上一个 Rust 写的 agent 循环 sidecar，拿复核过的尺子证明它与 TS sidecar 一致。**用户之前那次迁移已经走到 51–53/53**，这次是接着走完，不是重来。

## 现状（已确认）

| 项 | 状态 | 证据 |
|---|---|---|
| 基线 | `resources/PilotDeck` @ `cc7c4e6`，已 `pnpm install --frozen-lockfile && pnpm build` | exp001 |
| 上游对拍装置 | 能跑：53 场景、0 BLOCKED、自检 54/54；单遍约 4 分钟 | exp001 |
| 上游严格模式 | **不能直接判 Rust**：TS sidecar 自比只有 35/53 | exp001 补充 |
| 旧尺子 G03 v1 | 在旧执行现场 `build/CORE01-g03-rule-decision-independent-a10/verify_a9.py`（481 行）；驱动 `build/CORE01-astra-deadline-closure-a1/harness/formal-round.py`（137 行）、`formal-bind.mjs`（104 行）；**写死了绝对路径**（其中一份指向另一台机器的用户目录） | 调研 003 |
| 旧 Rust sidecar | `build/source-repository` @ `0eba113`；二进制由 `rust/crates/pilotdeck-transport/src/bin/pilotdeck-agent-loop-sidecar.rs` 产出；依赖极少（`sha2`、`uuid`、`chrono`、`tz-rs`、`serde_json`），**同步实现、无 tokio**；workspace 要求 edition 2024、rust-version 1.93 | 调研 003 |
| 本仓 | `src/` workspace 现有占位 crate `pilotdeck-cli`、`pilotdeck-core`；Rust 锁 1.99；CI `check` = fmt / clippy -D warnings / test；`main` 有分支保护 | PR #1 |

> 下文「旧现场」指 `~/ws/PilotDeck-rust-program-20261009/`。它是用户的自有积累：**只读，不改**。

## 目标

阶段 C 的终点：Rust sidecar 与 TS sidecar 在 53 个场景上按冻结的尺子一致（`.42cog/intent.md` 的「怎么算数」）。**本计划覆盖到「拿到第一个可信的分数 + 一份膨胀候选清单」为止**，之后的修复按场景组另开 issue。

## 执行步骤

> **为什么拆成三条 issue**：`dev-launch` 的粒度铁律（`.claude/skills/dev-launch/SKILL.md:58`）主张「能一条就一条、一条内拆多个 PR」。这里拆开不是因为工程量，而是**三条之间隔着人的闸门**：#1 结束时人批 `acceptance.md` 并冻结尺子，#2 才能开始；#2 合并后才有二进制可量。旧迁移一个 `CORE-01` 包下整个子系统、父任务收不了口（`docs/research/003_prior-attempt/`）。**只先开 #1**，#2、#3 在前一条合并后再开。

### Issue #1 · 回捞并复核尺子（≤ 1 天；超时降级，见风险 R1）

**S1 搬尺子进 `scripts/parity/`**（不新开顶层目录）
- `verify_a9.py` → `scripts/parity/g03_compare.py`；`formal-round.py` → `scripts/parity/round.py`；`formal-bind.mjs` → `scripts/parity/bind.mjs`。
- 每个文件头部注明来源路径与原文件 sha256；**所有写死的绝对路径改成参数或环境变量**：`PDRS_BASELINE`（默认 `resources/PilotDeck`）、`PDRS_OUT`（默认 `_tmp/<日期>-parity/`）。
- `trace.py` / `run.py` **不复制**，从 `$PDRS_BASELINE/tools/agent-loop-parity/` 导入（与旧尺子一样用基线原版）。
- ⚠️ **配对要改**（评审 A+B 撞车）：旧驱动只会比「native 对 X」——`formal-round.py:31-32` 的 `ts-control` 是 native 对 TS sidecar，`rust` 模式也是 native 槽对 Rust 槽；`verify_a9.py:428-429` 写死左 = native、右 = sidecar。本仓要的是 **sidecar 对 sidecar**：每一侧单独跑一遍上游 `run.py`（TS 侧用默认 sidecar，Rust 侧用 `PILOTDECK_AGENT_LOOP_SIDECAR_COMMAND` 换成 Rust），各取 **sidecar 那一侧的 trace**，交给 `g03_compare.py` 比。`g03_compare.py` 改成接收两份同类 trace；`bind.mjs` 的「unexpected」判据随之改：TS 那一遍里 TS sidecar 是**预期的**。每轮 summary 断言两侧身份：左必须是 TS sidecar，右必须是 sha256 对得上的 Rust 二进制。
- `round.py` 里写死的 Node 工具链路径（`formal-round.py:14`）一并参数化；`--expected-sha256` 只在 Rust 那一遍必填；round 名不再限定 `smoke*` / `control*`（`:34-37`）。
- 跑之前一律设 `PILOT_HOME`、`TMPDIR` 指向 `PDRS_OUT` 下——用户本机有一个在用的 `~/.pilotdeck`，还有一个 PilotDeck 正占着 18789，**都不能碰**。

**S2 写口径草稿 `specs/parity/`**（三份，S3 校准后冻结）
- `acceptance.md`：一个场景「一致」的定义 = 连跑 k 遍、G03 v1 判 PASS 的遍数 ≥ m（k、m 由 S3 的校准数据定，写进去后冻结）；全量对拍同一时刻只跑 1 个。
- `normalization.md`：工作目录路径对齐（exp001 补充里撞到的那一处）；ID 一致映射（同一原 ID 映射成同一占位符，保住引用关系）；帧 `seq` 保留不抹；**发给模型的请求体**的归一化规则（旧方案缺这一条）。
- `accepted-diffs.md`：已接受差异清单。每条写：场景、差异形状、属于哪一类（原版缺陷 / 比较器规则 / 环境 / 有意差异）、依据、谁批的。初始只放 S3 校准里找到的条目。

**S3 四项对照**（结果写进 `docs/experiments/exp004-ruler-recheck/readme.md`；`exp003` 留给阶段 A 的「真 `ui/server` 连代理」）
1. **TS sidecar 自比**：`round.py --mode ts-control` 连跑 k 遍（建议 k = 5），按 G03 v1 统计每个场景 PASS 遍数 → 定出 m。
2. **逐条规则的负向对照**：G03 v1 每放宽一处（通道拆分、E1–E12 各条），各构造一对注入了对应差异的 trace，断言是**那条规则**判它 FAIL（不是「随便哪里 FAIL」）。另加**端到端**的负向对照：在一对真实 trace 里注入差异、删掉一个场景、截断一份 trace，都必须判 FAIL。放在 `scripts/parity/controls/`，`python3 scripts/parity/controls/run_all.py` 一键跑。
   ⚠️ 评审 A 指出旧 `verify_a9.py` 的 E 规则失败只「旁报」、不进总判决（`:435`），场景靠 glob 取（`:423`）——**总判决必须同时要求：53 个场景齐全、两侧身份断言通过、所有适用的 E 规则通过**。
3. **两侧同坏**：两边都空、两边都报同样的错——必须判 FAIL。
4. **独立审查**：codex + opencode 双谱系各审一遍尺子与对照结果，记录在 `docs/reviews/001_ruler-recheck.md`（与本计划同号）。**最多两轮**；两轮后仍有 P0/P1/P2，停下交人仲裁，不再自己调。

**Issue #1 怎么算做完**
- [ ] `python3 scripts/parity/check_ruler.py` 输出一份 JSON，四项对照各一个布尔值，**全为 true**；结果与原始数据路径写进 `exp004`
- [ ] `acceptance.md`、`normalization.md` 写好后**先交人批**，批了才冻结；`FROZEN.md` 记录 `specs/parity/` 全部规约 **以及 `scripts/parity/` 下全部比较器与驱动代码**的 sha256；`python3 scripts/parity/check_frozen.py` 校验全部哈希。冻结后改任何一份都要另开 issue、由人批
- [ ] 双谱系审查收敛到无 P0/P1/P2（最多两轮，见上）
- [ ] CI `check` 绿（本 issue 不碰 Rust 代码，但 PR 照常走闸）

### Issue #2 · 搬入旧 Rust sidecar（在 Issue #1 合并之后）

**S4 照搬四个 crate 进 `src/`**
- 从旧现场 `build/source-repository` @ `0eba113` 复制 `rust/crates/{pilotdeck-agent,pilotdeck-transport,pilotdeck-protocol,pilotdeck-tokenizer}` 与 `rust/vendor/tiktoken-rs`（8.4 MB，保留其 LICENSE）到 `src/`，登记进 `src/Cargo.toml` 的 workspace，搬入旧 `[workspace.dependencies]` 里阶段 C 用得上的那几条（版本照旧钉死）。
- **不搬**：`plugin/workspace/terminal-protocol`、`ui-probe`、`desktop-probe`、`pilotdeck-protocol-probe`。
- ⚠️ **workspace 继承项要一起搬**（评审 A+B 撞车）：旧 `rust/Cargo.toml` 的 `exclude = ["vendor/tiktoken-rs"]`、`[workspace.package]` 的 `rust-version` / `version` / `license-file`、以及 `[workspace.lints]`（若有），crate 里都是 `*.workspace = true` 继承的；依赖**以各 crate 自己的 manifest 为准**（旧 workspace 依赖表之外还有 `ryu-js`、`base64`、`rustc-hash` 等）。本仓 `src/Cargo.toml` 与它合并时，manifest 的改动列入 `src/PROVENANCE.md` 的「适配例外」。
- ⚠️ **sidecar 入口要接受 gateway 传来的路径参数**（评审 A，主会话核实）：基线 profile 总是传 `args: [sidecarPath]`（`resources/PilotDeck/src/cli/AgentLoopDeploymentProfile.ts:73`），而旧入口只接受空参数或 `--stdio`，否则退出码 2（旧 `rust/crates/pilotdeck-transport/src/bin/pilotdeck-agent-loop-sidecar.rs:22`）；旧执行是靠 `formal-bind.mjs` 拦截 spawn、以空参数起 Rust 才绕过去的。⇒ 在入口里接受并忽略这一个位置参数，**单独一个提交**，列入适配例外。（人已拍板：改 Rust 入口。）
- 写 `src/PROVENANCE.md`：来源仓与 commit、每个搬入文件的 sha256、适配例外清单、「此后改动走本仓提交链」。
- 占位 crate `pilotdeck-core` 删掉（无内容）；`pilotdeck-cli` 留给阶段 A。

**S5 在本仓工具链上让它过闸**
- `cd src && cargo build --release --bin pilotdeck-agent-loop-sidecar`。
- `cargo test --all`：看 `running N tests` 的合计 ≥ 684（484 + 158 + 26 + 16）；少了先查是哪个 crate 漏了。
- `cargo fmt --all --check`、`cargo clippy --all-targets -- -D warnings`：1.99 比旧的 1.93 多出来的 lint，**单独提交修**，提交说明写「lint 适配，不改行为」。

**Issue #2 怎么算做完**
- [ ] CI `check` 绿；本地测试合计 ≥ 684，全过
- [ ] `src/PROVENANCE.md` 齐全；搬入文件与旧源的 sha256 一致（lint 适配的提交除外，单独列）
- [ ] 二进制能被 TS gateway 拉起：用 `scripts/parity/round.py --scenario pure_text` 冒烟一次，trace 里有 handshake / binding / module_call 证据，且 sidecar 身份是 Rust（旧方案 `tasks.yaml:323-325` 的验收）

### Issue #3 · 量分 + 对应检查（在 Issue #2 合并之后）

**S6 用冻结的尺子量一遍**
- `round.py --mode rust` 连跑 k 遍，Rust sidecar 对 TS sidecar，按 `specs/parity/acceptance.md` 判。
- 得出**第一个可信分数**（一致场景数 / 53），写进 `docs/experiments/exp005-first-score/readme.md` 与 `state/board.md`。
- 每个不一致的场景先**分诊**再动手（旧方案 `execution.md:52`）：Rust 产品 / 原版缺陷 / 比较器规则 / fixture 或环境。

**S7 对应检查**（人 2026-10-14 要求加入）
- 把搬入的每个 Rust 模块（按文件）对应到 TS 源头：`resources/PilotDeck/src/agent/loop/`、`src/agent/modules/`、`src/agent/sub/` 等，记 `TS 路径:行号范围`。
- 产出一张表：Rust 文件 · 行数 · 对应 TS 文件 · TS 行数 · 比值 · 判断（对得上 / 部分 / **对不上 = 可能膨胀**）。落 `docs/research/004_rust-ts-module-mapping/`。
- 对不上的模块**只登记、不删**；每个登记一条后续 issue，删之前必须用尺子证明删了不掉分。

**Issue #3 怎么算做完**
- [ ] 第一个分数与 k 遍原始数据路径已记录；每个非一致场景都有分诊结论
- [ ] 对应表覆盖搬入的全部 `.rs` 产品文件；「对不上」的都开了后续 issue
- [ ] CI `check` 绿

### Issue #4 起 · 按场景组修

来源：S6 的分诊结果 + 旧 `CORE.6` 的 10 个 open 项 + S7 的膨胀候选。**一组场景一条 issue**，每条在开头列出要让哪几个场景从 FAIL 变 PASS，开工后不加。

## 验收（端到端怎么测）

```bash
# 尺子（Issue #1 之后可用）
python3 scripts/parity/controls/run_all.py
python3 scripts/parity/check_ruler.py                              # 四项对照，输出 JSON
python3 scripts/parity/check_frozen.py                             # 冻结文件哈希
python3 scripts/parity/round.py --side ts   --label ts-r1           # TS sidecar 跑一遍
python3 scripts/parity/round.py --side ts   --label ts-r2
python3 scripts/parity/g03_compare.py ts-r1 ts-r2                   # TS 自比
# Rust sidecar（Issue #2 之后可用）
python3 scripts/parity/round.py --side rust --label rust-r1 --rust-binary src/target/release/pilotdeck-agent-loop-sidecar --expected-sha256 <sha>
python3 scripts/parity/g03_compare.py ts-r1 rust-r1
```

最后一道是人：用原版 UI 实际跑一遍（TS gateway 配 Rust sidecar），这一步不交给 AI。

## 风险与权衡

| # | 风险 | 应对 |
|---|---|---|
| R1 | G03 v1 在 `cc7c4e6` 上调不平，复核本身变成无限 MUST（旧执行在尺子上花了约一天半） | Issue #1 限 1 天；超时**停下交人决定**是否降级——降级不是自动的（评审 A+B：降级后「四项全过」永远打不了勾，降级分支要人另定验收与出分方式） |
| R2 | 环境漂移：旧尺子跑在另一台机器、Python 版本不同（本机 3.14，上游 3.12） | S1 里把 Node / Python 版本写进每轮的 provenance；S3 自比就是在检这一条 |
| R3 | 照搬会把旧执行没收口的问题一起带进来（`CORE.6` 10 open） | 不在 Issue #2 里修；进 S6 分诊，再按场景组开 issue |
| R4 | 体量：旧 Rust 产品代码约 2.7 万行，是 TS 对应职责的 1.5–2 倍 | S7 对应检查；尺子到位之后再精简 |
| R5 | 负载造成假差异 | 全量对拍同一时刻只跑 1 个（`CLAUDE.md` §五） |
| R6 | 误碰用户本机正在用的 PilotDeck 与数据 | 一律设 `PILOT_HOME` / `TMPDIR`；端口用 0 或高位；**绝不 kill 用户的进程** |

## 范围外

阶段 A（18789 gateway）与阶段 B（`ui/server`）；性能；发版；把对拍接进 GitHub CI（单遍约 4 分钟且要构建基线，先只在本地跑）。

## 人已拍板（评审后新增，2026-10-14 均按建议）

| # | 需要拍板的 | 选项 | 建议 → 决定 |
|---|---|---|---|
| 1 | Rust sidecar 怎么接进 TS gateway | 改 Rust 入口、接受并忽略路径参数（TS 零改动） · 沿用旧执行的 spawn 拦截（`bind.mjs`） | 改 Rust 入口：真实部署也走这条路，拦截只适合测试 → ☑ **改 Rust 入口** |
| 2 | `specs/parity/README.md` 第三节的阈值程序与分母 | 见该文件 | ☑ 按该文件，已冻结 |

## 相关

`docs/changes/arch/001_phase-c-agent-loop-sidecar.md` · `docs/research/002_parity-boundary/` · `docs/research/003_prior-attempt/` · `docs/experiments/exp001-baseline-parity-run/` · `docs/experiments/exp002-connection-state/`
