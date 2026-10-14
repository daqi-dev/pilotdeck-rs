# exp009 · 原版多态场景改用 TS 结果集合判法

2026-10-14；GitHub Issue #35。方案 B 人已批，场景登记与本实现仍待协调者评审冻结。本实验只读已有 trace，没有起服务、运行 sidecar 或补跑对拍；未改 G03、归一化、`state/` 和 `FROZEN.md`，未提交。

## 假设

`cancel_during_tool` 的原版自身存在多个合法结果。把判法改成「每遍 Rust 都必须与 TS 参考池中的至少一遍完整 G03 PASS」，可避免仅因逐遍配对恰好抽到另一 TS 结果而失败，同时拒绝 TS 从未产生的 Rust 结果。参考池、类别及准入只由 TS 数据决定，不看 Rust 来调口径。

## 做法

1. 先使用 `/Users/da/ws/pilotdeck-rs/_tmp/20261014-flake35/ts-r{1..20}.d`，验证身份、无 BLOCKED、G03 自比与全部 190 对 TS 两两比较。得到两类：类 0 为 17 遍（代表 ts1）；类 1 为 3 遍（代表 ts2，成员 ts2、ts7、ts11）。139 对 PASS、51 对 FAIL，类内全部 PASS、类间全部 FAIL；没有单例类。登记在 [multimodal.md](../../../specs/parity/multimodal.md)。此步完成后才读取 Rust。
2. [score.py](../../../scripts/parity/score.py) 对两侧各五遍离线计分。普通场景保留第 i 对第 i；仅 `cancel_during_tool` 对 TS 池逐遍寻找 G03 PASS。池不足 20、单例类、污染池的 Rust 身份、不合法输入及 BLOCKED 均拒绝计分。当前没有「已批」原版不稳定排除，分母为 53。
3. 输入为主仓 `_tmp/20261014-reg2`（main）与 `_tmp/20261014-reg3`（S1）。捕获时 output 均为 `/Users/da/ws/pilotdeck-rs/_tmp/20261014-parity/cur/artifacts`，TS 身份 sha256 为 `f4df7006f29c3d56656c9cee2fcfad1ebce77708bdc133e0fd1662796402e2c0`。Rust 身份分别为：

| 批次 | Rust 二进制 sha256（账本与 proof 一致） |
|---|---|
| reg2 · main | `e53d772ea9f81110f58eb56daa2b4ba9e9a7a8fc2537636b787025639bbfe180` |
| reg3 · S1 | `aea925aed8f1ae2e58481ac982e4dbb52a1f8c7237f9ee5d9cde9dad33c03360` |

历史复算命令（第 2 轮已补显式池和登记锁；省略参考池不再可用）：

```sh
python3 scripts/parity/score.py \
  --ts /Users/da/ws/pilotdeck-rs/_tmp/20261014-reg2/ts-r{1..5}.d \
  --rust /Users/da/ws/pilotdeck-rs/_tmp/20261014-reg2/rust-r{1..5}.d \
  --reference-pool /Users/da/ws/pilotdeck-rs/_tmp/20261014-flake35/ts-r{1..20}.d \
  --reference-registration _tmp/round2-ts-pool.json \
  --json _tmp/round2-reg2-score.json
python3 scripts/parity/score.py \
  --ts /Users/da/ws/pilotdeck-rs/_tmp/20261014-reg3/ts-r{1..5}.d \
  --rust /Users/da/ws/pilotdeck-rs/_tmp/20261014-reg3/rust-r{1..5}.d \
  --reference-pool /Users/da/ws/pilotdeck-rs/_tmp/20261014-flake35/ts-r{1..20}.d \
  --reference-registration _tmp/round2-ts-pool.json \
  --json _tmp/round2-reg3-score.json
```

~~两条命令均退出 1，表示有效分数中有场景 FAIL；不是 REFUSED。~~ **已作废：第 2 步及第 2 轮复算均 exit 2 / REFUSED，见下文；命令现须显式传池。** 加 `--paired-only` 可独立复算原判法。完整 JSON 包含逐场景原因、每遍 Rust 对池内全部 20 遍的 G03 判决、TS 190 对矩阵、类成员与输入/代码/上游 sha256；当前 worktree 的 `_tmp/reg{2,3}-{score,paired}.json` 为结果证据，`_tmp/ts-pool.json` 为首次 TS-only 定标证据。协调者删除 worktree 前应归档这些一次性证据。

## 结论

| 批次 | 原第 i 对第 i | 新结果集合口径 | `cancel_during_tool` 原判法 | 新判法 |
|---|---:|---:|---|---|
| reg2 · main | 51/53 | **50/53** | 五遍全 PASS | 五遍均与池中全部 20 遍不匹配，FAIL |
| reg3 · S1 | 50/53 | **50/53** | 第 2 遍 FAIL，其余 PASS | 五遍均与池中全部 20 遍不匹配，FAIL |

两轮新判法的 FAIL 都是 `deadline`、`deadline_during_tool`、`cancel_during_tool`。前两项仍按原配对判法，触发 E10/E11/G03_channels；未作任何排除。

这两批历史回归与登记池有一项真实请求体差异：回归的 modelView.systemPrompt 中环境段为 `now: 2026-10-13`，参考池为 `now: 2026-10-14`。其余 modelView 字段一致；同类比较中 G03 的差异路径为 `parent/A:trace[0]~[0].modelView`，触发 E10/G03_channels，双方健康检查为空。不同取消类还会触发额外的语义增删与 E11。`now` 位于普通 prompt 文字中，冻结归一化没有删它；因此不能声称新判法已经替这两轮消除了取消时序造成的误挡，也不能把此项偷偷归一化为 PASS。

判法本身的正例通过：构造两个经过真实 G03 自比验证的 TS 类（各 10 个离线 fixture），让某一遍 Rust 与同序号 TS 不同但与池内另一类一致，仍得 53/53。四项要求的负例全部通过：一遍 Rust 不匹配任何类→FAIL；池仅 19 遍→REFUSED、无分数；Rust 反复产生 TS 池外类→FAIL；普通场景虽能匹配其他序号 TS，但同序号有差异→FAIL。单例类、Rust 身份进入池、重复目录与排除状态也有对照。fixture 只验证算法，不作为实际 TS 定标证据。

## 改变了哪个取舍

- 原来要求每遍 Rust 与恰好配对的 TS 结果相同；现在仅对登记场景要求每遍 Rust 属于 TS 已确认的结果集合。仍逐遍完整比较三类契约、oracle、E 规则、健康与身份。
- 允许 Rust 只采用某个合法 TS 类，不要求其覆盖全部类或复现频率；代价是集合判法不检验概率分布。
- 没有缩小分母；参考池不足或仅一次出现的类不会放行。~~历史回归与参考池的 prompt 日期差异继续严格失败，实际分数不会为预期结论而抬高。~~ **已作废：跨采集日由账本判 REFUSED；同采集日的 prompt 日期差异仍由 G03 判 FAIL，见第 2 轮。**

## 边界

仅登记 `cancel_during_tool`，只有 20 遍 TS、两批各五遍 Rust。日期与捕获时输入上下文都属于完整模型请求体；当前池无法为更早的 prompt 日期提供匹配结果。跨日期分数不能据此推断 S1 产品退化，也不能据此宣布到达 53/53。若要让历史数据与新的池跨日期匹配，应另提请求体日期处理的尺子变更并批准，或在同一输入上下文采集足够 TS 参考；本任务没有做这些操作。

`acceptance.md` 只追加章节；G03、`check_ruler.py`、`check_frozen.py` 保持原样。冻结检查会如实报告 `acceptance.md` 与 `controls/run_all.py` 的旧哈希不匹配，以及三个新增尺子文件未登记；这不是自动登记或冻结通过。新增登记、更新哈希及双谱系评审由协调者处理，具体 sha256 与完整检查结果见当前 worktree `_tmp/RESULT.md`。

## 日期前提（Issue #35 第 2 步 · 2026-10-14）

本节记录第 2 步的新判决，覆盖上文第 1 步中「日期差异继续严格失败」「有效 50/53」等历史描述；上文保留为上一轮实验记录。日期上下文一致与捕获时输出路径相同均属于计分前提，日期不同现在拒绝计分，仍不删除或归一化 prompt。

参考池必须与被测 Rust 轮次在同一 UTC 日采集；TS 池自身、两侧各五遍的一轮回归都不得跨 UTC 午夜（UTC+8 本地 08:00）。跨日轮次整轮作废重跑。`score.py` 在场景比较前检查每份 trace 的全部 `model.request.modelView.systemPrompt`，提取 `now: YYYY-MM-DD`；普通场景对同序号 TS/Rust，多态场景对 Rust 与 TS 参考池。日期不同判该遍和场景 **REFUSED**，不进入 G03 匹配；任何场景 REFUSED 均使整轮 `valid_score=false`、`verdict=REFUSED`、`score=null`，退出码 2。一个 trace 或同一侧五遍出现多日，整轮拒绝；多日池拒绝定标。

每份 trace 的日期、观察到的日期集合、缺日期的请求 sequence 和说明都记入 JSON，异常拒绝也保留已读取的日期证据。找不到日期记 null：实际历史数据的 `auto_compact` 无模型请求，`sidecar_empty_system_prompt` 与 `sidecar_full_request_compaction_budget` 的 prompt 无 `now` 日期。双方均 null 时继续原 G03 检查；一侧 null、另一侧有日期则拒绝。null 不证明采集日期相同，采集者仍须满足同日要求。

同一组只读原始 trace 的最新复算：

| 批次 | 方案 B 最新整轮判决 | 场景明细 | `--paired-only` |
|---|---|---|---|
| reg2 · main | **REFUSED**；valid_score=false；score=null；exit 2 | 50 PASS、2 FAIL、1 REFUSED | valid_score=true；51/53；exit 1 |
| reg3 · S1 | **REFUSED**；valid_score=false；score=null；exit 2 | 50 PASS、2 FAIL、1 REFUSED | valid_score=true；50/53；exit 1 |

两轮 REFUSED 的场景均为 `cancel_during_tool`：五遍 Rust 日期都是 2026-10-13，参考池 20 遍都是 2026-10-14。逐遍理由明确列出两日期；`comparisons=[]`，没有把日期差异作为池外 Rust 类或 G03 FAIL。`deadline`、`deadline_during_tool` 仍按原配对判为 FAIL；其余场景结果不变。paired-only 两侧同日，故不因池日期拒绝。分母规则仍为 53，拒绝场景不会被排除来制造分数。历史方案 B 的 50/53 已作废，不能作为产品退化或通过证据。

本轮重新完成全部上一轮离线检查：TS-only 定标仍为 17/3 两类，190 对中 139 PASS / 51 FAIL；controls 从 33 增至 **35/35 PASS**，新增普通对日期不同与多态遍日期不同两个负例，均明确断言 REFUSED、无有效分数。另以真实归档 trace 的临时副本验证跨午夜整轮 53 场景 REFUSED、一侧 null 日期拒绝，以及同日配对可以 PASS 但仅参考池日期不同仍拒绝多态；缺日期说明也通过验证。边界复算脚本与报告为 `_tmp/date-boundary-probes.py` 和 `_tmp/date-boundary-probes.json`，它们只作离线验证，不是参考池证据。

复算命令仍使用上文命令；输出覆盖为最新 `_tmp/reg{2,3}-{score,paired}.json`、`_tmp/ts-pool.json`。TS 五遍自比仍 valid_calibration=true、S=53；check_ruler 的 self_compare、positive/negative controls、both_broken 均 true。check_frozen 继续如实报 acceptance/run_all 两处旧哈希不匹配，以及 multimodal 规范、score 与 multimodal controls 三处未登记；未改 FROZEN.md，也未将既有评审记录当成本次独立评审。最新哈希表、命令退出码及只追加/受保护文件检查见 `_tmp/RESULT.md`。全程未提交、未改 state/、未起服务、未补跑对拍。后续由协调者评审登记，并在同一 UTC 日重采参考池与 Rust 回归。

## 第 2 轮（PR #49 两家评审 · 2026-10-14）

本节覆盖历史的 prompt 日期前提及任一池成员匹配判法。采集日以每份有效账本 `started_wall` 的 UTC 日为准；本轮历史回归十份账本为 2026-10-13，池二十份为 2026-10-14，故方案 B 仍 REFUSED。原因现在来自账本。账本同日而 prompt now 日期值不同的对照得到有效 FAIL，日期值或时区错误作为产品缺陷暴露；日期缺失仍按本轮要求拒绝。全部无日期的两侧保留 G03，一侧 null 或部分 request 缺日期且对侧有日期则拒绝。

多态每遍须完整命中某一类所有成员；每类命中/未命中遍号写入证据。池去重仅按 trace sha256；更改账本不能让同内容副本算新捕获。池和计分 TS 的目录、trace 哈希均不得重叠；池内和计分 TS 的四项来源哈希逐项全等，否则 REFUSED。定标及可选登记锁核对在读取 Rust 前完成；真实 paired G03 在日期拒绝时保留。

登记改为每日独立重采池，显式指定目录与钉住的 TS-only JSON；没有固定日期默认池。历史二十份 trace 的 sha256 已逐份登记在 multimodal.md，重算仍为 17/3 两类。定标 JSON 保存类、完整矩阵、trace 哈希、账本 UTC 日和四项来源哈希；`--reference-registration` 不一致则拒绝，不会套用旧分类。

本轮全部离线检查、负例数量、退出码、最新文件 sha256 和证据目录见 worktree [_tmp/RESULT.md](../../../_tmp/RESULT.md) 的「第 2 轮」。部分成员匹配负例使用明确标记的比较结果故障注入：仅一项 Rust/池比较替换为真实 A/B G03 FAIL，其余比较仍调用冻结 G03，验证聚合器不会被部分命中放行；它不作为实际 TS 定标证据。其余新增拒绝负例全部使用真实 G03 和离线 fixture，不运行 sidecar。

第 2 轮真实归档复算结果（均 exit 2，valid_score=false，score=null）：

| 批次 | 方案 B | paired-only |
|---|---|---|
| reg2 · main | 42 PASS / 2 FAIL / 9 REFUSED | 43 PASS / 2 FAIL / 8 REFUSED |
| reg3 · S1 | 42 PASS / 2 FAIL / 9 REFUSED | 42 PASS / 3 FAIL / 8 REFUSED |

两个模式共同拒绝的 8 场景为 `sidecar_projected_request_compaction_budget`、`sidecar_continuable_followup_live`、`sidecar_continuable_followup_cold`、`sidecar_parent_close_after_admission`、`sidecar_one_shot_subagent_success`、`sidecar_one_shot_subagent_failure`、`sidecar_one_shot_parent_abort_after_admission`、`sidecar_one_shot_parent_close_after_admission`，它们包含部分无 now 日期的 request；即使两侧形状相同，只要对侧有日期，本轮规则仍拒绝。方案 B 另拒绝 cancel_during_tool 的池/回归账本跨日。reg2 的取消 paired 五遍仍真实 PASS；reg3 第 2 遍真实 FAIL，其余 PASS。原 paired-only 51/53 与 50/53 至此也作废，不再作为有效分数报告。


## 第 3 轮 · 部分请求缺 now 不拒绝（PR #49 · 2026-10-14）

本节覆盖第 2 轮的缺日期拒绝规则。结构性无 now 的子代理、压缩、续跑请求保留原样，不再由 fixture 补日期。缺日期 sequence 和说明继续进入 JSON，完整请求体交给冻结 G03；两侧同样部分缺日期正常计分，单侧缺日期作为 systemPrompt 差异判 FAIL。整份 trace 只有一侧完全无日期时，也只以账本日判断日期前提，同日交 G03。

日期 REFUSED 仅用于上下文不一致：账本 started_wall 的 UTC 日两侧（或 Rust 与参考池）不同；两份 trace 的日期值集合都非空且不等；两侧各五遍的整轮跨 UTC 午夜。池自身跨账本日仍拒绝定标。相同的多日期集合可进入 G03，多日期 prompt 本身不代表采集跨午夜。此前「账本同日、非空 prompt 日期值不同直接 G03 FAIL」也由本节覆盖。外层拒绝仍保留真实 paired 结果。

### 同日 reg5 + flake35 真实复算

只读输入为主仓 `/Users/da/ws/pilotdeck-rs/_tmp/20261014-reg5/{ts,rust}-r{1..5}.d` 与 `/Users/da/ws/pilotdeck-rs/_tmp/20261014-flake35/ts-r{1..20}.d`。十份回归账本与二十份参考账本全部为 **2026-10-14 UTC**；捕获时 output 路径一致，TS 四项同源哈希、池去重/与计分 TS 分离及登记锁核对均通过。没有修改归档、路径或 prompt，没有补采集。

| 模式 | valid_score | 真实分数 | 场景数 | 退出码 |
|---|---|---|---|---|
| paired-only | true | **51/53（96.23%）** | 51 PASS / 2 FAIL / **0 REFUSED** | 1 |
| 方案 B（显式池 + TS-only 登记锁） | true | **51/53（96.23%）** | 51 PASS / 2 FAIL / **0 REFUSED** | 1 |

原先被缺日期规则拒绝的八个场景现在两模式全部 PASS：`sidecar_projected_request_compaction_budget`、`sidecar_continuable_followup_live`、`sidecar_continuable_followup_cold`、`sidecar_parent_close_after_admission`、`sidecar_one_shot_subagent_success`、`sidecar_one_shot_subagent_failure`、`sidecar_one_shot_parent_abort_after_admission`、`sidecar_one_shot_parent_close_after_admission`。这两个模式的仅有失败为 `deadline` 第 1 遍、`deadline_during_tool` 第 3 遍，均触发 E10、E11、G03_channels，差异路径 `parent/B:trace[2]`；其余遍 PASS。两个 deadline 场景是有效 **FAIL**，没有 REFUSED，分母保持 53。

TS-only 定标重新读取全部二十份参考，不读取 Rust：仍为 17/3 两类，190 对中 139 PASS / 51 FAIL，类 1 成员为 ts2、ts7、ts11。reg5 的五遍 Rust 在方案 B 中均完整命中类 0 的全部 17 个成员；paired 取消场景也全部 PASS。当前仍为待协调者评审的登记证据，未改 FROZEN.md。

```sh
python3 _tmp/round3-calibrate-pool.py
python3 scripts/parity/score.py \
  --ts /Users/da/ws/pilotdeck-rs/_tmp/20261014-reg5/ts-r{1..5}.d \
  --rust /Users/da/ws/pilotdeck-rs/_tmp/20261014-reg5/rust-r{1..5}.d \
  --paired-only --json _tmp/round3-reg5-paired.json
python3 scripts/parity/score.py \
  --ts /Users/da/ws/pilotdeck-rs/_tmp/20261014-reg5/ts-r{1..5}.d \
  --rust /Users/da/ws/pilotdeck-rs/_tmp/20261014-reg5/rust-r{1..5}.d \
  --reference-pool /Users/da/ws/pilotdeck-rs/_tmp/20261014-flake35/ts-r{1..20}.d \
  --reference-registration _tmp/round3-ts-pool.json \
  --json _tmp/round3-reg5-score.json
```

历史 reg2/reg3 也重新计算：paired-only 恢复有效 51/53、50/53，均 exit 1；方案 B 均为 50 PASS / 2 FAIL / 1 REFUSED，exit 2、valid_score=false、score=null。唯一拒绝为 cancel_during_tool 的回归/池账本日 2026-10-13 vs 2026-10-14，不再有部分缺日期拒绝。这些跨日方案 B 仍无有效分数。

### 第 3 轮离线检查与证据

controls **59/59 PASS**（exit 0），check_ruler 内重新运行的 59 条也全部通过。移除四条 TS/Rust 单侧 null/部分缺日期 REFUSED 断言，改为同账本日有效 G03 FAIL，明确检查 G03_channels 和 modelView 差异；新增两侧同样部分缺日期在 paired-only/方案 B 下正常计分的两个正例，纳入 positive_controls。普通/多态非空日期集合不等的两条负例改为 REFUSED，同时断言真实 paired FAIL 保留、拒绝多态 comparisons 为空。fixture 不再给原始结构性无日期请求补 now。

TS 五遍自比 valid_calibration=true、S=53，十对全部 53/53，exit 0。归档临时副本边界检查 5/5 PASS，exit 0；reg5 八场景真实部分缺日期证据检查 8/8 PASS。语法、git diff --check、受保护文件逐字比 HEAD、state 无差异、未提交、acceptance/exp009/RESULT 本轮只追加及六份计分报告、定标、自比的输入/代码/上游哈希回查全部通过。

check_ruler exit 1：self_compare、positive_controls、negative_controls、both_broken 全 true，冻结条件 false。check_frozen exit 1：acceptance.md/run_all.py 两处旧哈希不匹配，multimodal.md/score.py/multimodal controls 三份未登记。independent_review=true 仍来自历史评审记录，不代表本轮独立评审已完成；本轮等待协调者复核登记。FROZEN.md 未改，未提交、未改 state/ 或 G03、未起服务。

完整命令、最新 sha256 表见 worktree [_tmp/RESULT.md](../../../_tmp/RESULT.md)「第 3 轮」。最新证据为 `_tmp/round3-reg5-{score,paired}.json`、`round3-ts-pool.json`、`round3-controls-verified.json`、`round3-self-compare.json/.md`、`round3-ruler-check.json`、`round3-frozen-check.json`、`round3-date-boundary-probes.py/.json`、`round3-reg5-partial-dates.json`、`round3-reg{2,3}-{score,paired}.json`、`round3-invariants.json`、`round3-change-sha256.json`。原始 trace 只读；删除 worktree 前先归档。
