# parity-attrib —— `#6` 归因装置（帧级反证判据）

`frame_attrib.py` 把「对拍 FAIL 是否由已登记的**原版竞态 #6** 导致」写成可机检判据。
它**不修改任何冻结件**：`specs/parity/**`、`scripts/parity/**` 只读；它唯一调用的外部程序是
冻结比较器 `scripts/parity/g03_compare.py`（子进程，只读）。

## 来历

| 时间 | 事件 |
|---|---|
| 2026-11-06 | `#1811` 选项 A 产出装置，唯一副本在 `_tmp/20261106-issue-1811/frame_attrib.py`（sha256 `e24560f9…eb6f0`） |
| 2026-11-06T15:47Z | `#1926` 人拍板 **① 补豁免 + 加 C6** |
| 2026-11-07 | 按 ① 修订并**入仓**（本目录）：C5 豁免 `requestEvidence`、新增 C6、补 2 条 negctl |

入仓理由：`#1157` 的常驻复查条件写着「用同一份 `frame_attrib.py` 重跑」，而 `_tmp/` 是可回收目录。
放 `scripts/parity-attrib/` 而**不是** `scripts/parity/`：后者在冻结检查（`check_frozen.py`）扫描范围内。

## 用法（必须 `-I`）

```bash
# 1) 逐 pair 归因；rc=0 全部可归因或 PASS，rc=2 有 NOT_6
python3 -I scripts/parity-attrib/frame_attrib.py <rounds 目录> <场景>
#    <场景> ∈ {deadline, deadline_during_tool}
#    <rounds 目录> 下同时含 ts-r1..ts-rN 与 rust-r1..rust-rN（parity-full 落盘布局）

# 2) 负控（会往 --negctl-out 写变异副本，该目录须独占新建）
python3 -I scripts/parity-attrib/frame_attrib.py <rounds 目录> <场景> --negctl-out <新目录>

# 3) 自测（纯合成数据，不依赖 trace）
python3 -I scripts/parity-attrib/test_frame_attrib.py
```

脚本内 `_repo_root()` 从自身位置向上找含 `scripts/parity/g03_compare.py` 的仓库根，因此 `_tmp/` 里的
历史副本与本目录的入仓副本都能跑，不绑死某一棵树。

## 判据

| 判据 | 内容 | 不满足 |
|---|---|---|
| C1 | 场景 ∈ `{deadline, deadline_during_tool}`（`accepted-diffs` #6 的登记范围） | NOT_6 |
| C2a | G03 `triggered_rules` 恰好 `[E10, E11, G03_channels]`（别的规则是另一种缺陷） | NOT_6 |
| C2b | G03 语义差异**恰好 1 条**，且是 `left=null` 的 `durable.status/gateway_submit_failed/error`，`text` = TS 宿主常量 | NOT_6 |
| C3 | 该 gsf 在右侧恰好 1 条、左侧 0 条，且序号严格晚于 `result_unknown_fail_closed` | NOT_6 |
| C4 | 右侧 sidecar 身份自证为 rust 且 `spawnObserved=true`（"无持久化写路径"靠外部 grep，见 `#1811` 证据） | NOT_6 |
| C5 | 删掉右侧那条 gsf 后，两侧帧**多重集**相同；`norm()` 抹掉 `invocationId`、`sequence`、**`requestEvidence`** | NOT_6 |
| C6 | 两侧**都存在** `requestEvidence` 的同名帧，规范化（`sort_keys` + 紧凑）后必须逐字节相同 | NOT_6 |

C5 用集合而非逐位：`#1841` 的 `context.budget` 相对顺序偏离已被 G03 判为可接受，逐位比会把它误报成「不是 #6」。

## C5 / C6 的残余（#1926 明确接受）

- **C5 残余**：`norm()` 豁免 `requestEvidence` 后，**单侧多出 `requestEvidence`** 这一类差异不再被 C5 抓到。
  依据是冻结口径：`specs/parity/normalization.md:32`「G03 与原版严格 comparator 都去掉 `requestEvidence`
  后再比语义」，`scripts/parity/g03_compare.py` 两处 `pop("requestEvidence")`。装置与冻结尺子保持同一口径。
- **C6 残余**：C6 只在**两侧都存在**时才比较。因此「单侧有、另一侧没有」既不被 C5 抓也不被 C6 抓，
  装置对此只计数（证据行里的「单侧存在 N 条」），判 ATTRIB_6。
- **C6 不比较的量**：`requestEvidence` 内部键序（规范化后比较，键序不算差异）；两侧都不存在时不适用。
- **要求**：换 Rust 宿主后首次全量对拍须**逐轮记录 ev 出现率**（`#1317` 报告模板已加此条），
  出现率异常升高时人工复核，不要只看 `NOT_6=0`。

## 负控（13 条，期望值即判据口径）

| 名字 | 期望 | 抓它的判据 |
|---|---|---|
| A-baseline-attrib6 | ATTRIB_6 | 基线（未变异） |
| B-event-renamed | NOT_6 | C2b |
| C-duplicated | NOT_6 | C2a（trace_health） |
| D-moved-before-fail-closed | NOT_6 | C2a |
| E-terminal-substituted | NOT_6 | C2a（E6） |
| F-scenario-out-of-scope | NOT_6 | C1 |
| G-text-tampered | NOT_6 | C2b |
| H-frame-dropped | NOT_6 | C2a |
| I-unrelated-extra-frame | NOT_6 | C2a |
| J-field-q-changed | NOT_6 | **C5**（G03 看不见，只有集合比对照抓） |
| K-lastApplied-changed | NOT_6 | **C5** |
| L-single-side-requestEvidence | **ATTRIB_6** | 无 —— 这是**残余确认**：证明 C5 豁免确实生效 |
| M-requestEvidence-content-differs | NOT_6 | **C6** |

`L` 是唯一「期望装置放过」的用例：它的价值是盯住豁免没被悄悄改严/改松。若 `L` 变回 `NOT_6`，
说明 C5 口径又收紧了，必须同步 `#1157` 的论断与本 README。

## 验证记录

真实数据（`#1811` 静默窗口 20 轮，HEAD `1569f785`，`deadline` 场景，100 pair）：

| 版本 | PASS | 归因 #6 | 不能归因 |
|---|---|---|---|
| 修订前（仅抹 invocationId/sequence） | 80 | 10 | 10 |
| 修订后（C5 豁免 + C6） | 80 | **20** | **0** |

- 复算原始输出：`_tmp/20261107-issue-1926/recompute-20261106T1611Z/`
  （`01-baseline-before.txt` / `02-after-c6.txt` / `03-negctl-after.txt`）
- 旧 11 条 negctl 结果与 `#1811` 完全一致（仍全部按期望命中），新增 L/M 亦符合期望。
- 每条 pair 的「ev」列给出两侧 `requestEvidence` 出现数：本轮 20 轮里只有 `ts-r4` 与 `rust-r5` 各 1 条，
  且两者规范化后逐字节相同（C6 判 PASS）。

## 维护规则

1. 改任何判据都要：跑 `test_frame_attrib.py` + 全量 negctl + 用**真实数据**原地重算，
   原始输出独占新建目录留档，并同步 `#1157`（归因结论）与 `#1317`（报告模板）。
2. 不要把本目录搬进 `scripts/parity/`（冻结检查范围），也不要改 `scripts/parity/g03_compare.py`。
3. 本装置是**判定装置**：影响对拍结论（假 PASS / 假 FAIL）的改动按 `P1` 处理，需两家非作者评审。
