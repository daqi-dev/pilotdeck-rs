---
type: research
number: "008"
date: 2026-11-06
title: A7.2-pr6b 四格矩阵一次性账本（#590）
tags: [phase-a, session-domain, four-cell, native-witness, parity]
status: 一次性账本已记录；四格分别报告，无后续四格回归闸门
related: [research/005_phase-a-gateway, research/008_phase-a-session-domain/must-know, research/008_phase-a-session-domain/decision, research/008_phase-a-session-domain/gap, research/009_a19-host-expectations/decision, experiments/exp006-gateway-parity-ruler, plan/005_phase-a-session-domain]
---

# 四格矩阵一次性账本（#590 / A7.2-pr6b）

> ## 一次性账本
>
> **这是一次性账本，不是常设闸门。** 按 #1301 选项 B（人批，正本 `state/decisions-20261102-a-stage-scope.md`「#1301 → B」节），
> A6 的完成口径降为「#590 一次性四格账本」；
> **之后没有四格回归闸门**。原拟的常设闸门 host suite **#1314 / #1315 已按 #1301 B 以 `not planned` 关单**
> （`gh issue view 1314`：`CLOSED / NOT_PLANNED`），A7 的「50 项 PASS/FAIL」改由 #1317 的人工对账表承担，同样无回归闸门
> ——分母以更晚的 `state/decisions-20261105-1313-rescope.md` 为准：**47 项装配 + 3 项 always-on 暂缓（如实报 `CAPABILITY_UNAVAILABLE`）**，
> 即 47/50 + 3，暂缓的三项不算漏项；
> 日后若要恢复常设闸门，须另开单人批。
> 计划 005 的原文按 #1301 B 的约定**不回写**（它还是 `specs/gateway-coverage/ledger.json` 的钉哈希输入，改一个字节就 `input_drift`），
> 所以 A6.9「四格矩阵相同 fixture 下语义一致」、A7 的 `--suite session` / `--suite host` 等旧口径仍留在原处——以决策卡与本页为准。
> 后来人不要把这页当成 CI 判据，也不要因为它没有跟进就当成回归失守。

四格 = 宿主 × sidecar 的四种组合。**每格单独报告，不合成一个 PASS/FAIL**（人批口径：能定位是宿主错还是 sidecar 错）。
本页所有结论**都在看到 Rust 结果之后才写**，但向量/阈值在此之前已由各装置自身冻结或登记，未因结果改判据。

## 一、先说清楚：四格今天没有同一台装置

没有任何单一入口能同时跑四格。四格是**拼**出来的，而且拼出来的四份 fixture 集互不相同：

| 格 | 宿主 | sidecar | 本页所用装置 | fixture 集 |
|---|---|---|---|---|
| ① | TS | TS | `scripts/parity/round.py --side ts`（宿主=基线 native，sidecar=基线 TS） | 基线 `tools/agent-loop-parity/scenarios.json` 53 场景 |
| ② | TS | Rust | `scripts/parity/round.py --side rust`（同上宿主，sidecar 换 Rust bin） | 同上 53 场景（**与①同一套**） |
| ③ | Rust | TS | `src/pilotdeck-gateway/tests/fixtures/native-topology/run.py` 的 `Rust-sidecar-ts` | 该 fixture 的 4 case × 5 轮 |
| ④ | Rust | Rust | 同上 `Rust-sidecar-native`；外加 `session_domain.py --suite deadline --native-capture` | 4 case × 5 轮；外加 A19-HOST 72 case |

**能比的和不能比的：**

- ①② 之间可以横比：同一装置、同一 53 场景，只换 sidecar ⇒ 差集归因**sidecar 侧**。
- ③④ 之间可以横比：同一装置、同一 4 case × 5 轮，只换 sidecar ⇒ 差集归因**sidecar 侧**。
- ①② 与 ③④ **不能**横比：装置、fixture、比较器、轮次全不同，宿主也不是同一份实现。谁要把四格数字并成一张分数表，就是在伪造证据。
- 「宿主侧」在 ①② 里指的是**基线 native 宿主 vs 基线 sidecar 适配器**的差异；在 ③④ 里指 Rust 宿主。两者不是同一件事。

结论先行：**四格今天都是"能跑起来、能各自报状态"，不是"四格语义一致"。** 语义一致（A6.9 原始验收）在当前现场**不成立**，本账本不改口径去凑它。

## 二、逐格结果

### 格① TS 宿主 × TS sidecar

三条互补证据，缺一条都不算数：

| 装置 | 结果 | 原始证据 |
|---|---|---|
| `session_domain.py --suite calibrate`（E1，TS 进程内受控，**无 sidecar、无 HTTP**） | `status=PASS`、`executionStatus=PASS`、`expectations=PASS`（72 case / 360 measurement / a19DiffCases 54）、`oracle=PASS`（`oracle.controls` **9 条负控**全部 `detected=true`；`expectations.measurements=360` 是**观测条数**＝72 case × 5 轮，不是负控条数；`e1Controls` 另记 68 条装置变异用例，68/68 `passed`、`status=PASS`）、`cleanupStatus=VERIFIED`、`unstableGroups=[]`、`frozen.exitCode=0`（30 项登记） | `_tmp/20261106-issue-590/calibrate-ts-ts/report.json` |
| `native-topology/run.py` 的 `TS-sidecar-ts` 格 | `NOT_RUN`，`reason="TS host not executed"` | `_tmp/20261106-issue-590/native-topology-001/report.json` |
| `scripts/parity/round.py --side ts` | sidecar 身份 `identity_ok=true`（53/53 场景各一条 `harness.proof`，sha256 与预期一致、`spawnObserved=true`）、`valid_round=true`、`run_py_exit=1`；`summary.json`：`scenarios=53` / `failed=27` / `expectedExtensions=3` / `blocked=0` / `oracleFailures=0` | `_tmp/20261106-issue-590/parity/side-ts/run.json` 与 `artifacts/*-pilotdeck.md` |

E1 calibrate 的**边界必须一起写**：它是 `topology=ts-in-process-controlled`、`sidecar.status=NOT_APPLICABLE`
（"controlled in-process TS loop; no sidecar binary"），`acceptanceStatus=NOT_RUN`、`freezeStatus=DRAFT_AWAITING_A5_4`。
**它证明的是 TS 进程内会话域观察装置自身可用，不是"TS 宿主 + TS sidecar 这一格通过"。**

`summary.json` 的账：**27 个场景 failed**（26 个纯换位 + 1 个含非换位差异 `sidecar_budget_limit`）+
**3 条 expectedExtensions**（`deadline` 的 2 条差异、两条 `sidecar_continuable_followup_*` 的 0 条差异）
+ 23 条完全一致 = 53。（分诊口径可复算：`summary.json` 的 failed / expectedExtensions 名单决定场景归类。两个类别的定义写在比较器的语义差异上，不是猜的：
**「纯换位」** = 差异列表里每一条「一侧有值、另一侧 `null`」都能在另一侧找到**同值**条目与之配对，配完没有剩余，
且没有任何「两侧都有值但不等」的条目（换位就是这么成对出现的，所以配对是判定它的唯一依据）；
**「含非换位」** = 至少有一条不满足上面——配不上对的缺失/多出，或两侧都有值但不等（内容差异）。下文的 26/1、47/2 都是**场景数**，不是差异条目数。）

- **26 个场景纯换位**：该场景的 semantic difference 全是「同一个值在两侧落在不同 trace 下标」。例：`single_tool` 的 `agent.status/turn_continued`
  在宿主侧落在 `trace[10]`、sidecar 侧落在 `trace[12]`，比较器按位置判成 2 条差异。
- **1 个场景含非换位差异**：`sidecar_budget_limit` —— 宿主写 `text="Reached Gateway-owned maxBudgetUsd ($0.500000) after spending $1.000000."`，
  sidecar 侧写 `text="max_budget_reached"`（两侧都有值且不等，是本格唯一的**内容差异**）。
- **`deadline` 不算失败**：它的 2 条差异（`durableStopReason="aborted_streaming"`、`resultType="aborted"` 对 `null`）
  按上面的定义属于**非换位（一侧有值、一侧 `null`）**，但它是场景里**已声明的 transport extension**，
  `declared_extension_matches` 判为 `verified 2 declared transport difference(s)`，因此进 `expectedExtensions` 而不是 `failed`。
  注意：同一份 md 文件里仍印着 "FAIL: 2 semantic difference(s)" —— **判状态看 `summary.json`，不要看 md 标题**。

### 格② TS 宿主 × Rust sidecar

同一份 `native-topology` 装置里也有这一格的对应行（该装置把 Rust 二进制叫 `native`）：
`TS-sidecar-native` = `NOT_RUN`，`reason="TS host not executed"` —— 与①的 `TS-sidecar-ts` 同因，
它说明**该装置根本起不了 TS 宿主**，所以①②两格只能靠 `scripts/parity/round.py` 取证。

| 项 | 值 |
|---|---|
| sidecar 二进制 | `pilotdeck-agent-loop-sidecar`，sha256 `37961b37b9340e49f0ec6d76a65a58703c5f6832aa34f1f49a4a02d72d7009f6` |
| 身份 | `identity_ok=true`，**53/53 场景逐条** `harness.proof` 命中——`round.py:438` 是**五项合取**：`len(proofs)==1`（恰一条 proof）、`side`、`sha256`、`spawnObserved is True`，且 `trace_health(...)` 无异常 |
| 轮次 | `valid_round=true`，`run_py_exit=1`，用时 243.7 s |
| 比较 | **49 个场景 failed**（47 个纯换位 + 2 个含非换位差异）+ **2 条 expectedExtensions**（0 条差异）+ **2 条完全一致**（`auto_compact`、`plan_mode_host_policy`）= 53 |

**归因（这是本账本最有用的一格）：**

- **22 条场景② failed、① 完全无差异** ⇒ 归因 **sidecar 侧**（Rust sidecar 与 ts sidecar 在宿主眼里的可见行为不同）。
  名单：`can_prompt_false`、`cancel_during_tool`、`checkpoint_resume`、`deadline_during_tool`、`image`、
  `malformed_model_response`、`max_turns`、`model_non_retryable_error`、`model_retryable_error`、`multi_tool_mixed_permission`、
  `multimodal_image_and_text`、`plan_mode_bypass_host_policy`、`pure_text`、`sidecar_additional_working_directories`、
  `sidecar_elicitation`、`sidecar_empty_system_prompt`、`sidecar_live_model_stream`、`sidecar_model_metadata`、
  `sidecar_one_shot_parent_abort_after_admission`、`sidecar_one_shot_parent_close_after_admission`、`stream_interruption`、
  `write_snapshot_resume`。
- 第 23 条是 `deadline`：① 里它的 2 条差异被登记为**已声明扩展**（verified），② 里变成 `transport contract mismatch`
  ⇒ 同样归因 **sidecar 侧**，但性质是"已声明的契约没兑现"，不是"凭空多出的差异"。22 + 1 = 23 条 sidecar 侧信号。
  闭合核对（场景级）：② 的 49 = 47 纯换位 + 2 含非换位；47 纯换位 = **22 只在②** + **25 ①②共有**；
  2 含非换位 = 1 共有（`sidecar_budget_limit`，两侧都有值且不等）+ 1 `deadline`（①里 2 条 null 差异被场景声明为扩展、判 verified；
  ②里同一场景是 1 对换位 + 2 条 null 差异 + 1 条多出，共 5 条，判 `transport contract mismatch`）。① 的 27 = 26 纯换位 + 1 含非换位，
  其中 26 纯换位 = 25 共有 + 1 只在①（`plan_mode_host_policy`）。两边共有失败场景恰为 26，对得上。
- 1 条**只在①有差异**（`plan_mode_host_policy`）⇒ 归因 **sidecar 侧的反向不稳定**：换 Rust sidecar 后该场景反而对上了。
  这一条同时说明该场景的"一致"不稳，不能单独当通过证据。
- 2 个场景含非换位差异（`deadline`、`sidecar_budget_limit`）：

  | 场景 | 宿主侧（native） | sidecar 侧 | 归因 |
  |---|---|---|---|
  | `deadline` | `trace[0]` 有 `model.request`；`durableStopReason="aborted_streaming"`、`resultType="aborted"` 有值 | `model.request` 落到 `trace[2]`（换位）；两个字段均为 `null`；另多一条 `trace[6].durable.status=gateway_submit_failed` | **sidecar 侧**：①只把它记成 "verified 2 declared transport difference(s)"（已声明扩展），②变成 `transport contract mismatch (5 semantic difference(s))` —— 声明对不上 + 多写一条失败态，是 Rust sidecar 的真实可见差 |
  | `sidecar_budget_limit` | `text="Reached Gateway-owned maxBudgetUsd ($0.500000) after spending $1.000000."` | `text="max_budget_reached"` | **宿主侧**（①②同一条差异，与 sidecar 是谁无关），且是基线自带、非本 PR 引入；②该场景另多 2 条 `context.budget` 条目（`trace[0]` / `trace[2]`，即 **1 对**换位，合计 3 条差异） |

  余下 **25 个换位场景在①②两格都出现**（同一批事件/字段只是出现位置不同），归因**宿主与 sidecar 的公共排序**，
  不指向任何一侧；只在②出现的 22 个换位场景已列在上面（归 sidecar 侧）。逐条差异见各自 `artifacts/*-pilotdeck.md`。

**这一格的口径边界（必须照写）：** `round.py` 的 `PilotDeck` 比较是「当前 native 宿主 vs 当前 sidecar」，
**不是** `specs/parity/README.md` 冻结尺子的「Rust sidecar 对 TS sidecar」。run.py 自己也写明
`run_py_exit=1` 属 native/sidecar 语义差，**不算 sidecar 身份失败**（`round.py:446`）。
所以格②的 49 条差异**不是** 对拍 FAIL，不能拿去当 A7 收口的分子分母；本账本只把它当**归因信号**用。

### 格③ Rust 宿主 × TS sidecar

`native-topology/run.py` 的 `Rust-sidecar-ts` 格：`status=PASS`，`executedCases` = `model-stream` / `provider-error` /
`provider-unconfigured` / `token-saver-judge` 四案全跑、每案 5 轮。证据 `_tmp/20261106-issue-590/native-topology-001/report.json`。

边界：整份报告 `status=NOT_RUN`、`moduleMatrixStatus=PASS`。`NOT_RUN` 来自 `entryCoverage`（生产入口
`production native request adapter not connected`）与 `retirement`（`complete exec/egress and storage observers absent`），
**不是**来自这两个 Rust 宿主格。`report.json:scope` 自述 "A5.3 Module-host probe; no production entry claim"。

### 格④ Rust 宿主 × Rust sidecar

| 装置 | 结果 | 证据 |
|---|---|---|
| `native-topology/run.py` 的 `Rust-sidecar-native` 格 | `status=PASS`，同 4 案 × 5 轮 | 同上 |
| `session_domain.py --suite deadline --native-capture` | 采集 `WITNESS_CAPTURED`；打分 72/72 `NOT_RUN`（`rule=COVERAGE`），批 `NOT_RUN` / exit 3 | `_tmp/20261106-issue-590/deadline-native-002/report.json`（详见第四节；-001 为同命令首次运行） |

③ 与 ④ 在同一装置、同一 4 案、同一 5 轮下**同为 PASS**，即该 4 案范围内宿主换 Rust、sidecar 换 Rust 各自都保持一致；
这**不能**外推成"Rust 宿主 + Rust sidecar 语义等价于 TS"，4 案 ≠ 53 场景 ≠ 72 case。

## 三、四格各自的结论（一句话版）

| 格 | 一句话结论 | 主要归因 |
|---|---|---|
| ① TS/TS | 装置可跑；E1 进程内校准 PASS，但**带 sidecar 的这一格本身没有 PASS 判据**；宿主 vs sidecar 在 53 场景里 27 个场景 failed（26 纯换位 + 1 含非换位），另 3 个场景是已声明扩展（`deadline` 等）判 verified | 宿主侧（1 条内容差异 `sidecar_budget_limit`）+ 两侧公共排序 |
| ② TS/Rust | Rust sidecar **身份 53/53 可核**；宿主 vs sidecar 差异 49 个场景 failed（47 纯换位 + 2 含非换位），比①多 22 个 failed 场景、且 `deadline` 由"已声明扩展"降为契约不符 ⇒ sidecar 侧信号共 23 条 | sidecar 侧（22 条 + `deadline` 契约不符） |
| ③ Rust/TS | 4 案 × 5 轮 PASS，但整份报告因生产入口/退役未接而 `NOT_RUN` | 无差异可归因（样本小） |
| ④ Rust/Rust | 4 案 × 5 轮 PASS；72 case 的**采集成功、判分全 NOT_RUN**（前置于 #1242） | 无法归因（判分未执行） |

## 四、native 格（`session_domain.py` 里原本 `NOT_RUN` 的那一格）

**结论：跑了。真的在 Linux 上采集到了原生证据；但 72 个冻结 case 仍然全部 `NOT_RUN` —— 因为 #1242 的受控刺激还没实现。**

### 4.1 先跑「裸」deadline（无采集）

```text
$ python3 -I scripts/gateway-parity/session_domain.py --suite deadline --output _tmp/20261106-issue-590/deadline-bare-3
{"status": "NOT_RUN", "suite": "deadline", "report": ".../deadline-bare-3/report.json", "failure": null, "acceptanceStatus": "NOT_RUN"}
EXIT=3
```

（退出码由脚本回显写进 `_tmp/20261106-issue-590/deadline-bare-3.log` 的 `EXIT=3` 行，不是凭终端回忆；
早先 `deadline-bare-2` 那次只留了 JSON 输出、没留退出码，本页以 bare-3 为准。）

`structureStatus=PASS`、`frozen.exitCode=0`（30 项登记全对），四格全 `NOT_RUN`，批 `NOT_RUN` / exit 3 ——
这正是任务书里说的"当前 NOT_RUN"的原始形态。

### 4.2 真采集（cb8，Linux 6.17，strace 6.8，pidfd）

（版本是 2026-11-06 回采集机复核的现场值：`uname -sr` = `Linux 6.17.0-1008-nvidia`，`strace -V` = `strace -- version 6.8`。
这两个值不写进 `capture/` 的元数据里，是本节唯一的「非留存证据」，其余数字都能在 `capture/evidence.json` 里对上。）

```sh
python3 -I scripts/native-witness/collect.py \
  --gateway  $CARGO_TARGET_DIR/debug/pilotdeck-gateway \
  --sidecar  $CARGO_TARGET_DIR/debug/pilotdeck-agent-loop-sidecar \
  --output   <evidence>/native-session-001 --session-domain
```

原始输出：

```text
{"status": "WITNESS_CAPTURED", "scoring": "NOT_RUN", "sessionDomain": true, "output": ".../native-session-001"}
RC-COLLECT=0
```

采集到的东西（证明 Rust 宿主 + Rust sidecar 真的跑了，不是空目录）：

- `processes` 73 条；`execWitness.complete=true`，`source=exec-monitor`，146 条 exec 见证事件 = 73 条 `exec` + 73 条 `exit`。
- `networkWitness.complete=true`，`source=strace-connect`，142 次 connect。
- `transcriptWitness.complete=true`，`source=strace-openat`，1106 次 **openat 调用**——它们分布在 **72 个转录文件**
  （36 个 `*.model.jsonl` + 36 个 `*.tool.jsonl`）上，每个文件被打开 10–19 次；判分侧在 `native_evidence.py:391`
  用 `first.setdefault(key, …)` **按路径取首开**去重，所以「1106」是调用数不是转录份数（转录是 72 份）。
- `topology=native`；`observer` 1 条连接 552 行；`provider` 63 次本地 mock 请求；`cleanup.allStopped=true`。
- `capture/receipt.json` 正常发布（`tracerExitCode` 与六个附件 sha256 齐备）。

### 4.3 用采集去打分 —— 跑起来了，但 72 个 case 全部 `NOT_RUN`

**打分必须回到采集机器上跑**：`capture/prelaunch.json` 记的是 ELF 的**绝对路径**，判分侧要按原路径复核存在性与 sha256。
在 macOS 工作树里直接打分先撞到：

```text
{"status": "BLOCKED", "failure": {"rule": "APPARATUS_IMPORTS",
 "detail": "#1140 BINARY_UNAVAILABLE: restore original absolute path /home/clawbox-08/ws/pdrs-worker-build/issue-590/debug/pilotdeck-gateway"}}
```

所以在 cb8 上按 `git worktree add --detach <2e2a26e4>` 开了一棵同 SHA 的干净检出（该机 `~/ws/pdrs-runner` 已有此提交，
GitHub 亦可 `ls-remote` 到同一 SHA），再打分：

```text
$ python3 -I scripts/gateway-parity/session_domain.py --suite deadline \
    --native-capture <evidence>/native-session-001 --output <evidence>/deadline-native-002
{"status": "NOT_RUN", "suite": "deadline", "report": ".../deadline-native-002/report.json", "failure": null, "acceptanceStatus": "NOT_RUN"}
EXIT=3
```

（`EXIT=3` 来自 cb8 上 `deadline-native-002.log` 的回显行；`deadline-native-001` 是同一命令的首次运行，
两次报告内容一致，-002 只是补上退出码留痕。）

报告本身有内容，不是空壳：

- `structureStatus=PASS`、`frozen.exitCode=0`（30 项登记全对）⇒ 冻结件没被绕过，采集也被审计接住了（否则会是 `BLOCKED`）。
- `rust-host/rust-sidecar` 格：`topology=native`，`sidecarSha256=37961b37b934…`（与 4.2 用的 Rust sidecar 同一枚），
  `processes` 里有真实宿主 argv（`pilotdeck-gateway --native --sidecar …pilotdeck-agent-loop-sidecar --drain-ms 1500`），
  `cases` **72 条**，**全部 `NOT_RUN`**，逐条 `rule=COVERAGE`、detail 前缀统一为
  `"未执行：缺少受控入口及目标边界见证: "`，其后的缺失边界名取自 **12 种组合**（不是 72 种），最多的四组是
  `registry, phase, order` ×20 / `…, failAppend` ×8 / `…, failStatus` ×8 / `…, slowAppend, failStatus` ×8。
- 其余三格仍是 `NOT_RUN` / 0 case（没有原生素材的格不会被凑数）。

根因不在采集器，也不在本单：`scripts/native-witness/stimulus_plan.py:execution()` 目前对每个 stimulus 直接返回
`NOT_RUN` + 上述 reason + `missing=[每个 boundary 名]`。这就是 **#1242（s2–s4）未实现**的直接后果：
采集器把 72 个会话、事件流、进程树、出网、转录首开**都采到了**，但"受控入口 + 目标边界见证"这一层还没有，
所以判分侧拿不到可比的期望，只能逐 case `NOT_RUN`（exit 3；不是 FAIL，也没有被算成 PASS）。

**没有改判分语义**：`session_domain.py`、`native_evidence.py`、`fixtures/session-*.json` 一个字节都没动
（`specs/gateway-parity/FROZEN.md` 登记的 30 项在两次运行里 `frozen.exitCode=0`）。

## 五、真实 provider 冒烟候选（交付③）

复用的是已合入的 `scripts/smoke/real_provider.py`（#709），不新写脚本。它的上限是**脚本常量**，随原始记录留档：

| 常量 | 值 |
|---|---|
| `MAX_REQUESTS` | 3（= `len(SCENARIOS)`；场景 text / stream-cancel / tool） |
| `MAX_OUTPUT_TOKENS_PER_REQUEST` | 128 |
| `MAX_TOTAL_OUTPUT_TOKENS` | 384 |

（以上三项以 `scripts/smoke/real_provider.py` 的常量为准，脚本每次运行也把同名字段写进 `report.json`
（`limits.*`），核对时以原始记录 JSON 为准而不是本表拷贝。）

**本轮已实测（AI 已实测，待人签字）**：2026-11-05T22:50:13Z 起跑，`exitCode=0`、`finalPass=true`、`capsVerified=true`：

| 场景 | 结果 | 请求数 | 输出 token | 说明 |
|---|---|---|---|---|
| `model_runtime_text`（text） | **PASS** | 1 | 6（in 25 / total 31） | `finishReason=stop`，回 `SMOKE-OK` |
| `model_runtime_stream_cancel`（stream-cancel） | **PASS** | 1 | 不适用 | 首个 stream delta 后即中止，Anthropic Messages 的 usage 块在流尾，结构性缺席（`limits.usageNotApplicable` 有记录） |
| `model_runtime_tool`（tool） | **PASS** | 1 | 43 | 工具轮完整 |

合计 **3 次请求 / 49 输出 token**（上限 3 / 384），`limits.violations=[]`、`usageMissing=[]`、`echoMissing=[]`；
`secretSelfCheck.finalPass=true`（三个 key 名与本机 generic 模式零命中）。
provider 是 llm-center `deepseek-v4.1-flash-mb`（`protocol=anthropic`），key 只从 `/Users/da/ws/pilotdeck-rs/.env.local` 读，
证据目录独占（`fcntl.flock`，`evidenceLock.holder.pid=67214`），跑前 `cargo-slots` 为「在跑 1 / 上限 2」。

**这只覆盖 Layer 0**（`pilotdeck-gateway` 的 model HTTP 运行时，经 `scripts/smoke/probe`）；
`report.json` 里另有 8 条 `NOT_YET` 的 gateway-process 场景（`gateway_e2e_*` / `permission_*` / `hooks_*` / `steer_*` / `reload_*` / `cleanup_once`），
它们**既不是 PASS 也不是失败**，本页不拿它们充数。**阶段状态不因此改变**：人签字前不算实效通过。

原始记录：主仓 `_tmp/20261106-issue-590-smoke/`（`report.json` / `report.md` / `run-20261105T225013Z-67214/`；
`report.json` 内部路径记的是工作树绝对路径，归档副本在主仓同目录）。

**待人签字的部分**：AI 只能判「请求/上限/回包/密钥自检」这些可实测项；需要人眼的是
① `report.md` 的可读性与场景叙述是否符合你的验收口味，② 三个场景的输出文案（`SMOKE-OK`、工具轮回复）观感是否可接受，
③ 是否认可「Layer 0 通过 ≠ 阶段通过」这个分层结论。人回复一句「签字」或指出要改哪一条即可。

## 六、边界与未覆盖（不要把本页读大）

1. **没有四格回归闸门**（本节开头已声明），也没有把四格合成一个分数。
2. **四格 fixture 集不同**，跨格只能比同一装置内的横向差；②的 49 个 failed 场景不是对拍 FAIL，①的 27 个也不是。
3. **A6.9 原始验收「四格矩阵同 fixture 语义一致」当前不成立**：#1301 选项 B 只是把完成口径降到本账本，
   没有说语义已经一致。
4. **格①②的宿主不是 Rust**，因此这两格不能证明 Rust 宿主的任何事；Rust 宿主只有 4 案 × 5 轮（③④）加 72 case 采集（④）。
5. **72 case 全部 NOT_RUN**，前置于 **#1242**（受控入口与目标边界见证）。
6. E1 calibrate 仍是 `DRAFT_AWAITING_A5_4` / `acceptanceStatus=NOT_RUN`，不可当已冻结尺子引用。
7. 本账本未改任何冻结件：`specs/gateway-parity/{accepted-diffs.md,README.md,FROZEN.md}` 未触碰。
8. **取消类已批偏离 `A7-HOST-POST-CANCEL-EXIT-001-R3` 的适用性**（正本 `specs/gateway-parity/diffs/A7-HOST-POST-CANCEL-EXIT-001-R3.md` 自述
   「四格矩阵（#590）中涉及取消的场景按本偏离判定」，出处 `state/decisions-20261025.md`「#643 取消退出到点后的有界收尾：选 A」）：
   - 该偏离描述的是 **Rust 宿主** `host/execution.rs` 取消后的 5 s 墙钟期限。①② 的宿主是 TS 基线 native ⇒ **不适用**；
     ② 里 `cancel_during_tool`、`stream_interruption` 等取消场景的差异是**宿主为 TS 时观测到的 sidecar 侧差异**，
     **不落在**这条偏离的「范围」内（它只管 Rust 宿主的取消后收尾），不能被它豁免，仍按 sidecar 侧信号记。
   - ③④ 的 fixture 只有 `model-stream` / `provider-error` / `provider-unconfigured` / `token-saver-judge` 四案，**不含取消场景** ⇒ 未被触发；
     ④ 的 72 case 全部 `NOT_RUN` ⇒ 也判不到。
   - 结论：四格里没有任何一格能验证这条偏离，本账本不对它作裁决，也不据此改任何判据。
     该条目最新版本是 `A7-HOST-POST-CANCEL-EXIT-001-R4`（2026-10-31 人批「全部按默认」，#1158），R4 自述「范围同 R3、只改批准状态」，
     因此上面引用的适用范围以 R3 原文为准。
   - **那句「四格矩阵中涉及取消的场景按本偏离判定」的适用范围有歧义**（窄读=只管 Rust 宿主格；广读=四格全管，
     会把②的 sidecar 侧信号由 23 降到至少 21——具体降幅取决于「涉及取消」是否把名字带 abort/close 的
     `sidecar_one_shot_parent_abort_after_admission`、`sidecar_one_shot_parent_close_after_admission` 也算进去，
     R3 原文没有枚举）。本页采**窄读**，理由与两种读法的逐场景差别见决策单 **#1716**（`question`，AI 代批待人复核）。
     这不是判分语义变更，不动冻结件，只影响本页一处计数；人回一句即可改。
9. **本单的冻结装置授权没用上**：`state/decisions-20261025.md`「#590 A7.2-pr6b 四格矩阵：授权修改冻结装置」允许在本单范围内改
   `session_domain.py` 及配套冻结件；实际执行**没有改动任何冻结文件**（30 项登记哈希在每份报告里 `frozen.exitCode=0`），
   72 case 的 `NOT_RUN` 是装置现状而非本单改判的结果。人要求的「事后详细解释」在 PR 正文里给出。
10. **证据归档位置**：本页链接的原始记录归档在**主仓** `_tmp/20261106-issue-590/`
   （绝对路径 `/Users/da/ws/pilotdeck-rs/_tmp/20261106-issue-590/`），不只在工作树里——工作树回收会连带删掉 gitignored 的 `_tmp/`。
   其中 `native-session-001/` 是 Linux-only 采集，macOS 上无法重采，**删前必须确认 cb8 上的 build 树还在**（见 §七末段）。

## 七、复现

> 平台注记：`sha256sum` 是 GNU coreutils，**macOS 上用 `shasum -a 256`**；`--native-capture` 的判分
> **必须在采集机上按原绝对路径跑**（`capture/prelaunch.json` 记的是 ELF 绝对路径 + sha256，异地判分为
> `BLOCKED / APPARATUS_IMPORTS`，见 §4.3）；`native-topology/run.py` 在 report 级 `NOT_RUN` 时**退出码 2**
> （源码 `run.py:599` 注释 "NOT_RUN never means success"），exit 2 既不是通过也不是失败。

```sh
# 0) 先定 target 目录：后面所有二进制路径都走它（不 export 时 cargo 默认写 src/target/，
#    而 $CARGO_TARGET_DIR 会展开成空的 /debug/...）
export CARGO_TARGET_DIR="$PWD/_build/cargo-target"   # 每树独占一个

# ① E1 进程内校准（TS；默认 --baseline 指本树 resources/PilotDeck，那里通常只有 README.md）
#    需只读基线：--baseline /abs/path/PilotDeck 或 PDRS_BASELINE；缺则打印 BLOCKED、exit 2，且不建输出目录
python3 -I scripts/gateway-parity/session_domain.py --suite calibrate \
  --baseline /absolute/path/to/PilotDeck \
  --output _tmp/<new>/calibrate

# ①② 宿主 vs sidecar（53 场景；需只读基线 + 已构建 Rust sidecar）
# 输出落在 $PDRS_OUT/<label>，PDRS_OUT 必须在本树 _tmp/ 之下；label 每次新起（目录独占创建）
export PDRS_OUT="$PWD/_tmp/<new>/parity"
python3 -I scripts/parity/round.py --side ts   --label side-ts
python3 -I scripts/parity/round.py --side rust --label side-rust \
  --rust-binary $CARGO_TARGET_DIR/debug/pilotdeck-agent-loop-sidecar \
  --expected-sha256 "$(shasum -a 256 $CARGO_TARGET_DIR/debug/pilotdeck-agent-loop-sidecar | cut -d' ' -f1)"  # Linux 用 sha256sum

# ③④ 原生拓扑 4 案 × 5 轮（需只读基线 cc7c4e65；以下四条按 fixtures/native-topology/README.md「运行」原文，
# 在仓库 src/ 目录执行；五个参数全必填，缺一个 argparse 直接 exit 2）
cd src
cargo build -p pilotdeck-gateway --example a53_native_host
cargo build -p pilotdeck-transport --bin pilotdeck-agent-loop-sidecar
python3 -I pilotdeck-gateway/tests/fixtures/native-topology/run.py \
  --baseline /absolute/path/to/PilotDeck \
  --node /absolute/path/to/node \
  --sidecar "$CARGO_TARGET_DIR/debug/pilotdeck-agent-loop-sidecar" \
  --probe "$CARGO_TARGET_DIR/debug/examples/a53_native_host" \
  --output /absolute/path/to/new-evidence-directory
cd ..

# ④ 72 case 原生采集（仅 Linux，pidfd + strace）
python3 -I scripts/native-witness/collect.py --gateway <gw> --sidecar <sc> \
  --output _tmp/<new>/native-session --session-domain
python3 -I scripts/gateway-parity/session_domain.py --suite deadline \
  --native-capture _tmp/<new>/native-session --output _tmp/<new>/deadline
```

**复现时的四条硬约束**（都是本单踩过的，不是假想）：

1. **对拍锁是独占的，但四个装置不是同一份实现**：四者都抢 `/tmp/pdrs-parity.lock`（目录租约，**不要**自己 `flock` 一个文件），
   可得行为并不一样：
   - `round.py` 自带一份同契约实现（`scripts/parity/round.py:211`，**没有 timeout 形参**），抢不到就 `sleep 2` **无限等**，
     不会超时退出、也不写 `run.json`——看到 `waiting for /tmp/pdrs-parity.lock` 就是它在等，别当成死锁去删锁。
   - `collect.py` 等 **1 s**（`:182`），失败抛 `Blocked` → **BLOCKED / exit 2**；`session_domain.py` 等 **30 s**（`:2952`）→ 同样 BLOCKED / exit 2；
     `native-topology/run.py` 等 **默认 600 s**（`common.parity_lock`）→ `TimeoutError` 未被 main 捕获，**traceback / exit 1**。
   - `2400 s` 只是 `MAX_AGE` 上限，**已发布的 owner 不会因年龄被自动接管**：`owner.json` 必带 `cleanupPending`（真值），
     接管条件要求它是假。所以一次被 SIGKILL 的运行会留下「永不自愈」的锁。脱困路径按 `scripts/cli-parity/README.md`
     「锁被保留时怎么解」节：先 `cat /tmp/pdrs-parity.lock/owner.json` 看 `pid` / `token` / `cleanupPending`，
     确认没有残留进程组后**人工**删 `owner.json` 再 `rmdir`；不要在不看残留的情况下删锁。
   同一主机同时只跑一个全量对拍，串行排队。
2. **退出码要单独留存**：这几条命令的 stdout 是单行 JSON，退出码不在 JSON 里。写法统一成
   `python3 -I <cmd> > <新文件名>.log 2>&1; echo "EXIT=$?" >> <新文件名>.log`，
   日志文件名每次新起（`deadline-bare-3`、`deadline-native-002` 就是这么来的；`-2` 那次只留了 JSON 没留退出码，被评审正确指出）。
3. **采集与判分之间不要重建 ELF**：`capture/prelaunch.json` 记的是二进制绝对路径 + sha256（本单 gateway
   sha256 `f2ad413665f3ac12127260d17db78d4648ff063d41826847278ebb408f280679`），判分侧按原路径复核字节。
   中途 `cargo build` 换掉二进制 ⇒ 判分 `BLOCKED / APPARATUS_IMPORTS`。
4. **证据目录独占**：`--output` / `--label` 都是一次性目录（`mkdir(exist_ok=False)`），重跑必须换名字，
   不要复用 `side-ts` / `side-rust` 这类 label，也不要往已完成目录里补文件。

本单 ④ 的判分是在 cb8 上跑的（`--native-capture` 只能回采集机判分）：用的是一棵按 `git worktree add --detach 2e2a26e4`
开的临时检出 `~/ws/pdrs-worker-wt/issue-590-deadline`（HEAD 就是基线 `2e2a26e4`，工作区干净，跑完即可弃）；
该树不在本机，本页所有证据链接都指向归档副本，不指向那棵树。

**但 cb8 上的 build 树不能弃**：`capture/prelaunch.json` 把宿主与 sidecar 钉在
`/home/clawbox-08/ws/pdrs-worker-build/issue-590/debug/{pilotdeck-gateway,pilotdeck-agent-loop-sidecar}`（含 sha256），
判分要按这组绝对路径复核字节。那棵树一删，`native-session-001` 就永久只能得到 `BLOCKED / APPARATUS_IMPORTS`，
而 macOS 上无法重采（`collect.py:178` 的 `need(sys.platform == 'linux')`）。
