---
type: research
number: "008"
date: 2026-11-06
title: A7.2-pr6b 四格矩阵一次性账本（#590）
tags: [phase-a, session-domain, four-cell, native-witness, parity]
status: 一次性账本已记录；四格分别报告，无后续四格回归闸门
---

# 四格矩阵一次性账本（#590 / A7.2-pr6b）

> ## 一次性账本
>
> **这是一次性账本，不是常设闸门。** 按 #1301 选项 B（人批），A6 的完成口径降为「#590 一次性四格账本」；
> **之后没有四格回归闸门**。常设回归闸门是 host suite #1314，另单另授权。
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
| `session_domain.py --suite calibrate`（E1，TS 进程内受控，**无 sidecar、无 HTTP**） | `status=PASS`、`executionStatus=PASS`、`expectations=PASS`（72 case / 360 measurement / a19DiffCases 54）、`oracle=PASS`（360 条负控全部命中）、`cleanupStatus=VERIFIED`、`unstableGroups=[]`、`frozen.exitCode=0`（30 项登记） | `_tmp/20261106-issue-590/calibrate-ts-ts/report.json` |
| `native-topology/run.py` 的 `TS-sidecar-ts` 格 | `NOT_RUN`，`reason="TS host not executed"` | `_tmp/20261106-issue-590/native-topology-001/report.json` |
| `scripts/parity/round.py --side ts` | sidecar 身份 `identity_ok=true`（53/53 场景各一条 `harness.proof`，sha256 与预期一致、`spawnObserved=true`）、`valid_round=true`、`run_py_exit=1`；比较 53 场景 **26 条一致 / 27 条有差异** | `_tmp/20261106-issue-590/parity/side-ts/run.json` 与 `artifacts/*-pilotdeck.md` |

E1 calibrate 的**边界必须一起写**：它是 `topology=ts-in-process-controlled`、`sidecar.status=NOT_APPLICABLE`
（"controlled in-process TS loop; no sidecar binary"），`acceptanceStatus=NOT_RUN`、`freezeStatus=DRAFT_AWAITING_A5_4`。
**它证明的是 TS 进程内会话域观察装置自身可用，不是"TS 宿主 + TS sidecar 这一格通过"。**

对 `round.py --side ts` 的 27 条差异分诊（脚本口径：把每份报告的 semantic difference 条目取左/右非 null 值做多重集比对）：

- **26 条是纯换位**：同一批事件/字段，出现位置不同。例：`single_tool` 的 `agent.status/turn_continued`
  在宿主侧落在 `trace[10]`、sidecar 侧落在 `trace[12]`，比较器按位置判成 2 条差异。
- **2 条是内容差异**（两格都有，见下）。

### 格② TS 宿主 × Rust sidecar

| 项 | 值 |
|---|---|
| sidecar 二进制 | `pilotdeck-agent-loop-sidecar`，sha256 `37961b37b9340e49f0ec6d76a65a58703c5f6832aa34f1f49a4a02d72d7009f6` |
| 身份 | `identity_ok=true`，**53/53 场景逐条** `harness.proof` 命中（side/sha256/spawnObserved 三项全对） |
| 轮次 | `valid_round=true`，`run_py_exit=1`，用时 243.7 s |
| 比较 | **4 条一致 / 49 条有差异**（47 条纯换位 + 2 条内容差异） |

**归因（这是本账本最有用的一格）：**

- 23 条场景**只在②有差异、①没有** ⇒ 这 23 条归因 **sidecar 侧**（Rust sidecar 与 ts sidecar 在宿主眼里的可见行为不同）。
  名单：`can_prompt_false`、`cancel_during_tool`、`checkpoint_resume`、`deadline`、`deadline_during_tool`、`image`、
  `malformed_model_response`、`max_turns`、`model_non_retryable_error`、`model_retryable_error`、`multi_tool_mixed_permission`、
  `multimodal_image_and_text`、`plan_mode_bypass_host_policy`、`pure_text`、`sidecar_additional_working_directories`、
  `sidecar_elicitation`、`sidecar_empty_system_prompt`、`sidecar_live_model_stream`、`sidecar_model_metadata`、
  `sidecar_one_shot_parent_abort_after_admission`、`sidecar_one_shot_parent_close_after_admission`、`stream_interruption`、
  `write_snapshot_resume`。
- 1 条**只在①有差异**（`plan_mode_host_policy`）⇒ 归因 **sidecar 侧的反向不稳定**：换 Rust sidecar 后该场景反而对上了。
  这一条同时说明该场景的"一致"不稳，不能单独当通过证据。
- **两条内容差异两格都有** ⇒ 归因 **宿主侧**（与 sidecar 是谁无关），且是基线自带、非本 PR 引入：

  | 场景 | 宿主侧（native） | sidecar 侧 |
  |---|---|---|
  | `deadline` | `trace[3].durableStopReason="aborted_streaming"`、`resultType="aborted"` | 两者均为 `null`（②另报 `transport contract mismatch`） |
  | `sidecar_budget_limit` | `text="Reached Gateway-owned maxBudgetUsd ($0.500000) after spending $1.000000."` | `text="max_budget_reached"` |

  另 21 条换位差异②有①也有，归因**宿主与 sidecar 的公共差异**（排序），不指向任何一侧。

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
| `session_domain.py --suite deadline --native-capture` | 采集 `WITNESS_CAPTURED`；打分 72/72 `NOT_RUN`（`rule=COVERAGE`），批 `NOT_RUN` / exit 3 | `_tmp/20261106-issue-590/deadline-native-001/report.json`（详见第四节） |

③ 与 ④ 在同一装置、同一 4 案、同一 5 轮下**同为 PASS**，即该 4 案范围内宿主换 Rust、sidecar 换 Rust 各自都保持一致；
这**不能**外推成"Rust 宿主 + Rust sidecar 语义等价于 TS"，4 案 ≠ 53 场景 ≠ 72 case。

## 三、四格各自的结论（一句话版）

| 格 | 一句话结论 | 主要归因 |
|---|---|---|
| ① TS/TS | 装置可跑；E1 进程内校准 PASS，但**带 sidecar 的这一格本身没有 PASS 判据**；宿主 vs sidecar 在 53 场景里 27 条有差异（26 换位 + 2 内容） | 宿主侧（2 条内容）+ 两侧公共排序 |
| ② TS/Rust | Rust sidecar **身份 53/53 可核**；宿主 vs sidecar 差异 49 条，比①多 23 条 ⇒ 这 23 条是 Rust sidecar 的真实可见差 | sidecar 侧（23 条） |
| ③ Rust/TS | 4 案 × 5 轮 PASS，但整份报告因生产入口/退役未接而 `NOT_RUN` | 无差异可归因（样本小） |
| ④ Rust/Rust | 4 案 × 5 轮 PASS；72 case 的**采集成功、判分全 NOT_RUN**（前置于 #1242） | 无法归因（判分未执行） |

## 四、native 格（`session_domain.py` 里原本 `NOT_RUN` 的那一格）

**结论：跑了。真的在 Linux 上采集到了原生证据；但 72 个冻结 case 仍然全部 `NOT_RUN` —— 因为 #1242 的受控刺激还没实现。**

### 4.1 先跑「裸」deadline（无采集）

```text
$ python3 -I scripts/gateway-parity/session_domain.py --suite deadline --output _tmp/20261106-issue-590/deadline-bare-2
{"status": "NOT_RUN", "suite": "deadline", "report": ".../deadline-bare-2/report.json", "failure": null, "acceptanceStatus": "NOT_RUN"}
EXIT=3
```

`structureStatus=PASS`、`frozen.exitCode=0`（30 项登记全对），四格全 `NOT_RUN`，批 `NOT_RUN` / exit 3 ——
这正是任务书里说的"当前 NOT_RUN"的原始形态。

### 4.2 真采集（cb8，Linux 6.17，strace 6.8，pidfd）

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

- `processes` 73 条；`execWitness.complete=true`，`source=exec-monitor`，146 条 exec 事件、73 次尝试。
- `networkWitness.complete=true`，`source=strace-connect`，142 次 connect。
- `transcriptWitness.complete=true`，`source=strace-openat`，1106 次 openat（会话转录首开）。
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
    --native-capture <evidence>/native-session-001 --output <evidence>/deadline-native-001
{"status": "NOT_RUN", "suite": "deadline", "report": ".../deadline-native-001/report.json", "failure": null, "acceptanceStatus": "NOT_RUN"}
EXIT=3
```

报告本身有内容，不是空壳：

- `structureStatus=PASS`、`frozen.exitCode=0`（30 项登记全对）⇒ 冻结件没被绕过，采集也被审计接住了（否则会是 `BLOCKED`）。
- `rust-host/rust-sidecar` 格：`topology=native`，`sidecarSha256=37961b37b934…`（与 4.2 用的 Rust sidecar 同一枚），
  `processes` 里有真实宿主 argv（`pilotdeck-gateway --native --sidecar …pilotdeck-agent-loop-sidecar --drain-ms 1500`），
  `cases` **72 条**，**全部 `NOT_RUN`**，逐条 `rule=COVERAGE`、`detail="未执行：缺少受控入口及目标边界见证: registry, phase, order"`。
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

运行与人工复核步骤见 PR 正文与 `_tmp/20261106-issue-590-smoke/` 下的原始记录；本轮实测结论与"待人签字"状态见 PR 正文
（AI 实测不改变阶段完成状态，人签字后才算实效通过）。

## 六、边界与未覆盖（不要把本页读大）

1. **没有四格回归闸门**（本节开头已声明），也没有把四格合成一个分数。
2. **四格 fixture 集不同**，跨格只能比同一装置内的横向差；②的 49 条差异不是对拍 FAIL，①的 27 条也不是。
3. **A6.9 原始验收「四格矩阵同 fixture 语义一致」当前不成立**：#1301 选项 B 只是把完成口径降到本账本，
   没有说语义已经一致。
4. **格①②的宿主不是 Rust**，因此这两格不能证明 Rust 宿主的任何事；Rust 宿主只有 4 案 × 5 轮（③④）加 72 case 采集（④）。
5. **72 case 全部 NOT_RUN**，前置于 **#1242**（受控入口与目标边界见证）。
6. E1 calibrate 仍是 `DRAFT_AWAITING_A5_4` / `acceptanceStatus=NOT_RUN`，不可当已冻结尺子引用。
7. 本账本未改任何冻结件：`specs/gateway-parity/{accepted-diffs.md,README.md,FROZEN.md}` 未触碰。

## 七、复现

```sh
# ① E1 进程内校准（TS）
python3 -I scripts/gateway-parity/session_domain.py --suite calibrate \
  --output _tmp/<new>/calibrate

# ①② 宿主 vs sidecar（53 场景；需只读基线 + 已构建 Rust sidecar）
python3 -I scripts/parity/round.py --side ts   --label side-ts
python3 -I scripts/parity/round.py --side rust --label side-rust \
  --rust-binary $CARGO_TARGET_DIR/debug/pilotdeck-agent-loop-sidecar \
  --expected-sha256 "$(sha256sum $CARGO_TARGET_DIR/debug/pilotdeck-agent-loop-sidecar | cut -d' ' -f1)"

# ③④ 原生拓扑 4 案 × 5 轮（需只读基线 cc7c4e65）
python3 -I src/pilotdeck-gateway/tests/fixtures/native-topology/run.py --output _tmp/<new>/native-topology

# ④ 72 case 原生采集（仅 Linux，pidfd + strace）
python3 -I scripts/native-witness/collect.py --gateway <gw> --sidecar <sc> \
  --output _tmp/<new>/native-session --session-domain
python3 -I scripts/gateway-parity/session_domain.py --suite deadline \
  --native-capture _tmp/<new>/native-session --output _tmp/<new>/deadline
```
