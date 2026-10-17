# PilotDeck-RS：把 PilotDeck 后端从 TypeScript 重写为 Rust · 状态板（给 AI · 跨会话唯一接续点）

<!-- READY:BEGIN -->
updated: 2026-10-18 00:31 · by 协调者(Claude Code 交互·opus-5.5·/loop 两小时交接)
- A6.1-pr2 | gate | ready | 分支 codex/a61-pr2（wt-a61）：cancel + 清 #191 十三条；GLM/Claude 确认轮 0/0，本 PR 开出即合并；转跟进三条在 #191（前置节点已写）
- A6.2-pr1 | gate | ready | 分支 codex/a62-ledger（wt-a62）：GLM/Claude 两轮 0/0；DeepSeek（关思考）补报 P1×3，codex 按 TS 实测处理中（_tmp/20261017-a62/fix2-task.md）；差异类跟进 #201
- D0-pr1 | gate | ready | 分支 codex/d0-cli-parity（wt-d0）：真实 TS 自比与五项负控曾 PASS；评审 P0：--disable-warning 吞产品警告、openssl 整树读权、TS/Rust 共用 profile 可 forward-to-TS，codex 修复中（task-r3.md），修完协调者沙箱外重跑定标
- A6.8-pr1 | impl | blocked | user-dialog store（triage：_tmp/20261017-parallel/triage.md），等 #200 合并放宽 worktree 上限后开
- A6.8-pr2 | impl | blocked | upload store，同上
- #200 | review | ready | 守护会话作者；第四轮 codex P0×1/P1×2 已转作者，GLM r4 重跑中；合并后新闸门要求 readonly=tools，在途单开 PR 前用新脚本重跑四角色
- #202 | review | ready | DeepSeek 默认关思考；codex 0/0，GLM 重跑中
- #19 | coord | human | 人定暂缓（10-17，阻塞节点：关 #19／阶段 C 53/53 标完成）· deadline 差异登记：state/decisions-20261017-deadline-diff.md
- D4 | impl | blocked | 已人批（10-15 D4→B，不是待批项）；剩余是实施：A5.4 登记 A19-HOST 逐格期望（帧/durable/registry），两家评审后冻结，修复在 A7；待 A5.4 定标时排期
- #150 | coord | human | 人定暂缓（10-17，未发现依赖，里程碑复查）· Draft 处置
- #144-triage | coord | human | 人定暂缓（10-17，未发现硬依赖，里程碑复查）·「P0 候选」降级、关闭或下一步
- 167-ruler-a4 | coord | human | 人定暂缓（10-17，未发现依赖，里程碑复查）· check_ruler 是否升级为 A4 本地化语义（state/decisions-20261017-167-ruler.md 卡 2，暂不执行）
<!-- READY:END -->

> ## 2026-10-18 00:31 · /loop 两小时交接（自 18:15 起连续）
> 合并：#192（A6.1-pr1）、#185、#193、#194、#196（#187）、#197（loop-fix）、#198（只做产品代码）、#199（P2 清账第一批）。人定「编排提速」后拆出并行单（triage：`_tmp/20261017-parallel/triage.md`），A6.1-pr2 / A6.2-pr1 / D0-pr1 在途，A6.8 两条等 #200。DeepSeek 一度被我误判为大输入不可用并停用，根因是思考吃光输出额度（#202 修，新记忆 20261018-deepseek-thinking）。worktree：主仓 + wt-a61 + wt-a62 + wt-d0 + wt-rrg（守护会话）；磁盘约 39/46 GiB。后台：codex 修 A6.2（fix2）与 D0（r3），GLM 审 #200 r4 与 #202。**下一步**：A6.1-pr2 合并；A6.2 / D0 修完后确认轮；#200 合并后开 A6.8 两条。

> ## 2026-10-17 22:29 · /loop：loop-fix（#197）、#198 合并；p2-sweep 第一批随本 PR；人定「只做产品代码」
> 用户经守护会话定规则（#198 已合）：只做 intent.md 所定产品范围，对拍装置归产品侧；协作工具不开单、不排 READY。据此关闭工具类 P2-跟进 #174/#175/#178，#179 已由 #197 修复关闭。p2-sweep 第一批（codex 实现）：#55 第一条 P2 修复并补三条确定性测试；#120 子项 1、2 核实已随 #137 核销（补 TS 出处注释，第 3 项仍开）；#195 a5-19 四入口与 exp003 不再写死 baseline。GLM+DeepSeek 两轮无 P0/P1（DeepSeek P1 经实测驳回，`_tmp/20261017-p2-sweep/arbitration.md`）；确认轮新 P2 记回 #55/#195。其余开着的 P2-跟进都绑在后续节点（A5.4/A6.x/A7/A8/阶段 C），不提前清。新记忆：收敛后 P2 只修一批。**下一单**：A6.1-pr2（须同时清 #191）。

> ## 2026-10-17 20:45 · /loop：#187 合并（#196），loop-fix 随本 PR
> #187：gateway-parity 装置不再写死主仓 baseline（冻结四登，AI 代批待人复核），codex+GLM 三轮收敛 0/0/0；真实数据复算 A2 同树自比 PASS、E1 calibrate PASS（360 次测量、controls PASS）、三条负控 BLOCKED；同类遗留开 #195。loop-fix：`pdrs-loop.sh` 每次启动独立 `run-HHMMSS-pid/` 目录（不再覆盖前一跑 iter 原件），账本 v2 以 board updated 是否变化判 done / no-progress；`scripts/test-pdrs-loop.sh` 桩 runner 覆盖并入 CI。**下一单**：p2-sweep 或 A6.1-pr2（须清 #191）。

> ## 2026-10-17 20:15 · 两小时交接（/loop 协调会话，自 18:15 起）
> 本会话合并 #192（A6.1-pr1）、#185（review-deepseek）、#193（board）、#194（#182 补审）。在途：#187（见 READY，分支已推送，证据在主仓 `_tmp/20261017-issue-187/`）。worktree：主仓 + wt-a61（A6.1-pr2 复用）+ wt-187；磁盘约 36.4/46 GiB。无未结束的后台进程（评审将随 #187 PR 派出）。接续者按 READY 从 #187 继续；交互会话仍可接着跑。

> ## 2026-10-17 · /loop：#182 补审完成
> #182（外层循环产出、合并时零评审）由 codex+GLM 补审四轮收敛到 0/0/0。实质问题在 `state/memory/20261017-parity-binary-path.md`：原文没区分 A2 校准（A1 纯透传源）与 A4（HEAD），也没分开装置树与被测源树，照做会再选错二进制（两家撞车，P1）。已按 #167 r4-a1 实际做法改写正文、description 与索引；原件 `_tmp/20261017-issue-182-review/`。#182 的 board 历史段两家核过，无事实相反陈述。**下一单**：#187。

> ## 2026-10-17 · /loop（Claude Code 交互）：#192 A6.1-pr1 合并、#185 合并；两小时交接
> **合并**：#192（A6.1-pr1 宿主 sidecar client，f479f73，Refs #127）——GLM+DeepSeek 三轮收敛到 0/0/0；首轮四角色 boundary#2 经 TS:1120-1121 判误报，DeepSeek「缺 rustix」驳回（Cargo.toml:19）；遗留 14 条 P2 合进 #191，前置于 A6.1-pr2。#185（review-deepseek.py，982b964）——用户插单，codex+GLM 九轮收敛（codex 前后报 P0×12，全修，含实测 urllib 跟 302 外带 Authorization）；四角色同谱系只计「做了」（决策卡 state/decisions-20261017.md 末）。两次合并 committer 仍是真实邮箱（老坑，待人改 GitHub 设置）。
> **新规则**：最终四角色审计前先 rebase（state/memory/20261017-gate-head-rebase.md），#192 因此多跑一轮。
> **交接**：worktree 主仓 + wt-a61（持有人协调者，A6.1-pr2 复用，开工前 reset 到 main 新分支）+ wt-185（本 board PR 用，合并后可清）。磁盘 36.4/46 GiB（du -sk 核于 19:48）。无后台进程。**下一单**：READY 队首 #182-review；A6.1-pr2 须同时清 #191。

> ## 2026-10-17 · 人定：五项待批暂缓
> 人判断不了 #19、#150、#144、check_ruler 升 A4 这四项对重构的影响（D4 核实已于 10-15 人批，不在其中，READY 改为实施项 blocked），定为「不阻塞就先保持现状，信息更充分后再判」。记录与原则见 `state/decisions-20261017.md` 末节；READY 中四条标「人定暂缓」并写明阻塞节点，平时不追问，到节点或里程碑复查时再提。#19 的决策卡（原来只在本地、未入库）随本 PR 入库，入库不等于批准。

> ## 2026-10-17 · /loop 收尾交接（本会话 /loop 结束）
> **wt-a61 现场**：持有人是本协调会话，结束后移交下一个协调者。codex 后台进程已经结束（`_tmp/20261017-a61/codex-impl.status` = exit=0），没有在跑的进程；日志在 `_tmp/20261017-a61/codex-impl.out` 和 `.err`，worker 自证在 `_tmp/20261017-a61/worker/`（RESULT、交叉测试、负控等原始日志）。codex 在旧基线 board 末尾追加了一段，协调者已撤掉，没有提交。产物已提交并推到 `codex/a61-host-client`@05abe8b：新增 `src/pilotdeck-gateway/src/host/`（connection、execution、tests）、`examples/a6_host_probe.rs`、`scripts/a6-host/`。协调者在沙箱外补跑：`cargo test -p pilotdeck-gateway` 188 通过、0 失败、10 忽略；fmt 和 clippy -D warnings 通过；check_frozen PASS 18 项。（worker 在沙箱里报的 2 条失败是 loopback bind 被拒，沙箱外复现不了。）
> **新会话先核 wt-a61 现场**（`git -C _build/wt-a61 status`，HEAD 应为 05abe8b，工作区干净），再决定续跑还是重派；正常情况下直接续跑：rebase main（旧 naming baseline 会让 wt 内 check-naming 报 `_tmp/rust-r1.err`，rebase 后消失）→ 合并闸门。还没有任何评审。
> 本会话另合并了 #186（#167 关单）和 #188（chore-cleanup，#183 已关）。worktree：主仓、wt-a61、wt-handover（本 PR 合并后可清）。

> ## 2026-10-17 · /loop 第三轮：chore-cleanup（用户经 pilotdeck-rs-69 会话插单），本会话 /loop 到此结束
> board 瘦身：10-17 以前的段和 OmO 调研段原样移到 `state/board-archive-202610.md`。#183 的接手段和 READY 项（#182-review、loop-fix）并入本 PR，#183 以「已并入」关闭。新开 #187（B1 硬编码路径）；READY 新增 p2-sweep、#144-triage（human），167-ruler-a4 也列为待人批。分支清理和 _tmp 旧件已由 pilotdeck-rs-69 做完（`_tmp/20261017-noise-audit/`）。
> **交接**：在途只有 A6.1-pr1——codex 在 wt-a61 后台实现中（本会话起的进程，`codex-impl.status` 写入 exit= 表示结束），协调者还没有提交过任何东西。wt-chore 合并后可清。磁盘约 34.4/46 GiB，worktree：主仓 + wt-a61 + wt-chore。用户会开新会话重启 /loop，从 READY 队首接续。

> ## 2026-10-17 · /loop 第二轮：#186 合并，#167 关单
> 两家独立审查 codex（P0–P3 全零）和 GLM（只有 3 条 P3）都给 independent_review=true；pr-gate 走纯文档豁免，4 项检查全绿后 rebase 合并（ac94046/184214b）。收尾自查贴在 #167 后关单；wt-167 与 wt-167-a1src 在证据核对一致后清掉。合并提交的 committer 仍是真实邮箱（老坑，要人在 GitHub Settings → Emails 处理）。**下一单**：A6.1-pr1。

> ## 2026-10-17 · /loop 交互首轮：#167 A2 尺子 r4 重采机器项全绿，待两家审
> 主仓 HEAD 构建 gateway 后六轮都跑起来了，但 proxy 轮判 G2.observer：A4 已把十个只读 method 本地化，冻结规约要求 A1 代理全透传——尺子没错，是被测物换代了；同一原件用规约已授权的 `compare --local-method ×10` 复核 PASS。正式重采改用 A1 纯透传源 896b37f 构建 proxy（冻结件零改动）：`check_ruler` 机器项全 true、PENDING_REVIEW（原件 `_tmp/20261017-167-resample/ruler-r4-a1/`）。登记见 exp006 readme r4 段与 `state/decisions-20261017-167-ruler.md`（A1 选取 AI 代批待复核；check_ruler 升 A4 语义列入待批）。codex + GLM 独立审查在跑。杂项：主仓 `_tmp/` 五个裸 tgz 违反命名规范（上一会话归档遗留），待整理。**下一步**：审查收敛→开 PR 合并→A6.1-pr1 发车。

> ## 2026-10-17 · Claude Code 接手：核验三跑汇报、改由交互会话主持、磁盘清缓存
> **核验 opencode 三跑汇报**（原件对 git/GitHub/ledger/out.log）：属实——#182 合并 44163e6、#167 根因（`proxy/run.json` ERROR + `run_round.py:160` 硬编码路径）、三处留痕、$2.77；不实——「12 分钟单迭代」实测三跑 257 s，「tmux 保尸会话」不存在（且 iter-001 被覆盖、前两跑原件已失），「四天从零」实为当日约 2.5 h；**漏报**——#182 零评审过闸（违合并闸门），近三提交 committer 为主邮箱（§五老坑仍在，待人勾 GitHub 隐私设置）。
> **通道判断（AI 代批，待人复核）**：claude 通道下 dontAsk 白名单逐命令匹配，协调者 shell 开放集合补不完（三跑仍被 `cd &&`/`ps`/`sed` 拒），改为 **Claude Code 交互会话 + `/loop` 驱动 READY**；`pdrs-loop.sh` 留给 opencode 通道，修复项入 READY `loop-fix`。A6.1-pr1 挂起原因是通道权限，已恢复 ready。
> **资源**：board 旧记 43.6G 已陈旧；实测 34.30 GiB，按 decisions-20261016-resource 先例只删 10 个 `debug|release/incremental` 缓存（清单 `_tmp/20261017-takeover-dryrun/incremental-list.txt`），现 **33.68/46 GiB**；worktree 仅主仓，三把锁均无。
> **下一单**：#183/#182 评审欠账 → #167-resample → A6.1-pr1 → loop-fix。

> ## 2026-10-17 · 外层循环三跑：A6.1-pr1 挂起，#167 调查结案
> **A6.1-pr1 → blocked**（任务书指令：上一迭代未推进即挂起；现场仅主仓 worktree、无 wt-a61、无在途证据）。**#167 根因**：「controls 自比门」失败不是装置失配——proxy 轮 `run.json` 为 ERROR，`run_round.py:160` 硬编码的 `src/target/debug/pilotdeck-gateway` 在 wt-167 不存在，G0/G1/G3 全是级联；TS 五轮自比全 PASS。处置不动冻结件：wt 内按该路径构建后重采（`_tmp/20261017-167/investigation.md`，已评论 issue）。**权限观察**：dontAsk 下 `cd … &&` 复合命令与 `ps` 被拒，单段命令可用。**下一单**：#167-resample。

> ## 2026-10-17 · 外层循环二跑验收（--max-iter 1）：trust 卡点已解，白名单微补
> 二跑（15:40，opus 17 轮/$0.52）机制依旧全过（落账 done/exit0）；A6.1-pr1 仍未开工——**workspace trust 未标记**导致 allow 被忽略（opus 正确拒绝改 `~/.claude.json` 自我解锁）。协调者复核：`hasTrustDialogAccepted` 现已为 True（探针实证 settings 被读取、Write 生效），首跑 err.log 的 Ignoring 为旧记录。**磁盘 34.3/46G**（归档压缩 11.7G→2.16G tgz，零丢失校验后删源，board 旧值 43.6G 作废）。白名单补 shell 原语（echo/printf/grep/find/sort 等，复合命令逐段匹配所需）。**三跑待本 PR 合并**。READY：A6.1-pr1 保持 ready。

> ## 2026-10-17 · 外层循环首跑验收（--max-iter 1，runner=claude）
> **机制四项全过**：拉起协调会话→任务书派发→7200s 时限→ledger 落账 done/exit0→按 max-iter 自然停。**通道被权限卡死**：claude dontAsk 无项目 allow 列表，Edit/codex/cargo/git 全拒，A6.1-pr1 未开工；opus 协调者（14 轮/$0.92）正确拒绝自我扩权并留完整交接（`_tmp/20261017-pdrs-loop/iter-001/out.log`）。修复：`.claude/settings.json` 白名单（与第 14 条授权对齐）+ pdrs-loop.sh bash3.2 多字节变量 9 处花括号化。**磁盘 43.6/46G**：开 wt-a61 前须先清 _tmp（归档后删）。二跑待人确认。

> ## 2026-10-17 · 第十轮（本轮收尾）：A6.1 调研完成，任务书就绪
> **A6.1 发车调研**：TS 协议面 2021 行（client 1784 + stdio 237）已量；Rust 侧方向确认（宿主 client 放 `pilotdeck-gateway/src/host/`，与 sidecar 侧 transport 分离；复用 protocol crate 与 exp015 交叉装置）。**任务书 `_tmp/20261017-a61/brief-next.md`**——下一棒直接派 codex 发车第一 PR（协议 client：spawn/handshake/execute + stream identity/seq/ACK + 断线 fail-closed + TS↔Rust 交叉测试）。
> **本会话累计（连续推进五~十轮）**：合并 5 个代码 PR（#138/#141/#142/#161/#163/#169——含今日全部）+ 3 个 docs PR；关闭 #46/#56/#134/#139/#154；#19 决策卡待批；A4 收官、A5 兼容主体齐、A6.1 就绪。开着的 P2-跟进：#120/#164/#165/#166/#167（重采缺口）/#170 等；待人批三项：#19 deadline 差异、D4 评分口径、#150 处置。
> **下一棒顺序**：① A6.1 第一 PR（任务书就绪）→ ② #167 重采调查（controls 自比门）→ ③ A6.1 replay/cancel 后续 PR。资源：worktree 已清至仅主仓，磁盘 _build+tmp 约 40G/46G。

> ## 2026-10-17 · 第九轮（连续推进）：#167 修复部分合并（#169），A5.4 兼容主体齐
> **#169 合并**：validate_calibration 核对 repetition/phase/variant 与预注册 ID + 空账本拒绝（audit P1 缺口闭合）；24 条负控；FROZEN 三登 check_frozen 18 项 PASS（评审 P1 条件项经协调者实跑闭合）。#170（Node 侧轮次标签）登记。
> **#167 留开**：六轮重采被 controls 自比门挡（fresh proxy 产物与 fixed manifest 不匹配：G0.round unfinished / G1.coverage 场景目录缺多）——独立装置问题待调查，前置于 A5.4 定标与 E0 证据。
> **A5 状态**：A5.1✅ A5.2✅ A5.3✅ A5.4 兼容主体齐（calibrate 360 次 PASS）；未冻结部分= D4 评分口径 + deadline 差异（均待人批）+ #167 重采。**#67 保持 OPEN**（按 plan005 口径标「兼容部分完成，修正口径待批」的时点未到——重采未完）。
> **下一单**：#127（A6 Rust 宿主能力）调研与首包（A5.1 账本已在 main，A6 按包拆 PR 可起步；D4 未批不阻塞 A6 的存储/宿主基建）。

> ## 2026-10-17 · 第八轮（连续推进）：A5.3 合并（#163），四张 P2-跟进登记
> **A5.3 完成**：session_domain.py E1 入口（calibrate 360 次测量全过 + 10 类负控 + F5 补四规则内存变异 + deadline 四格 NOT_RUN 结构自检）双家评审三轮收敛合并 main。**FROZEN 双重登记**：①补 #142 漏登的 accepted-diffs 哈希（人批 2026-10-17）②白名单 15 件 + 三新脚本；check_frozen 18 项 PASS。P2-跟进：#164（进程组免ps探测）#165（分母入FROZEN）#166（NOT_RUN退出码）#167（A2六轮重采+repetition=0缺口）——各标前置节点。
> **待批清单（人出现时提醒，不停机）**：① #19 deadline 差异登记（decisions-20261017-deadline-diff.md，推荐A）；② D4 评分口径（exp014 六格材料齐）；③ #150 Draft 处置。
> **下一单**：A5.4 兼容部分（E0 八组回归 + calibrate k=5 已备 + 评审记录整理）；冻结语义部分待 D4 人批。随后 #127（A6 起）。

> ## 2026-10-17 · 第七轮（连续推进）：#19 调研收尾（#161 合并）、A5.3 开工
> **#19（阶段 C 冲 53/53）**：codex 深挖修正旧现场——`sidecar_budget_limit` 与 `deadline_during_tool` **历史已修、各 5/5 PASS**（budget wire 语义链已查实：toAgentStatusEvent 省略 text→client 按 event 回退→原因码）；`deadline` **4/5**，两侧 sidecar wire 逐帧一致，r3 残差为 TS 宿主对账竞态（原版缺陷）。PR #161（deadline 部分文本边界回归，72+ 行纯测试）双家评审合并，冻结哈希核对过。**决策卡 `state/decisions-20261017-deadline-diff.md` 待人批**（推荐 A：登记已接受差异→复算→关 #19）；issue OPEN 不冒充。
> **A5.3 开工**（codex 在 wt-19，分支 codex/a53-session-domain）：session_domain.py E1 入口（calibrate 编排 + deadline 四格结构 + 原生拓扑审计 + 九类负控 + 白名单同 PR 变更）。分阶段收。
> **队列**：A5.3 → A5.4（定标冻结）→ #127（A6–A8）→ #128（B3–B6）→ #129（F3–F6）→ 阶段 D。D4 第二步评分口径待人（exp014 六格 TS 侧已稳定，呈人材料在 readme/results.json）。
> **待人（出现时提醒，不停机）**：① #19 deadline 差异登记批卡；② D4 差异与评分口径；③ #150 Draft 处置（A5 装置线是否复用）。

> ## 2026-10-17 · 人定：持续自主推进授权（用户指示，另一协调会话转述）+ 技能协议入库
> **授权基调**：按 board 就绪队列**连续自主推进**，不每单停等发车、不把阶段汇报当批准请求（CLAUDE.md 既有规则从现在用足）；两小时到点写交接后**继续下一单**，不等用户重新启动。依据：Claude Code 时代已验证此模式（10-13~14 单会话 40.9h + 84 子代理连续自主产出，`~/.claude/projects/-Users-da-ws-Auto/`）；opencode 机制同构（长会话 + 子代理），缺的只是授权颗粒与协议执行度。**边界不变**：放权边界 / 红线 / 决策分级照旧，第 14 条未授权动作（对外发布、不可逆删除等）照停；两小时交接与合并闸门不因此省略。
> **流程参考**：`docs/changes/ops/001_skill-protocol.md`（42plugin 技能 × 重构环节映射；pr-ready 三角色 #46 起必跑、simplify 二选一落定、交付收尾固定 checklist）。**下一单：#46（A4 gateway 透传 + 10 method 本地化，P0 主线）立即开工**，按 ops/001 环节表走，先评估是否拆 sub-issue。

> ## 2026-10-17 · 第六轮（收尾）：#157 合并，#154/#139 清账
> PR #157（文件级 EACCES/ENOENT 原件+回放）双家评审无 P0/P1、CI 三绿、rebase 合并 main@fcbc953；#154、#139（P0）关闭——A4 全部前置债清零。wt-142 归档（`_tmp/archive/worktrees-20261017/wt-142-154/`）后删除，worktree 仅剩主仓。磁盘 _build 17G + _tmp 23G。
> **当前开着的债**：Draft #150（Ready 装置，A5 需要时评估）；P2-跟进 9 张（#55/#91/#98/#115/#117/#119/#120/#132/#133/#145/#147/#148/#152 中仍开者）；P1 验收债 #61/#65/#94/#117/#133。
> **下一棒（按序）**：① #67（A5 深水区，含 #19 六格实测冲 53/53）；② #127（A6–A8 宿主/会话域与 gateway 退役）；③ #128（B3–B6 全路由接管）；④ #129（F3–F6 前端迁移）；⑤ 阶段 D（#122 起 CLI 对拍）。分工与通道矩阵见第四轮段。

> ## 2026-10-17 · 第五轮：#134 人批落地——#142 合并，A4 收官（#46 关闭）
> **人批**：「批准 #134 的登记范围」→ accepted-diffs 追加 A4-ARCHIVE-SEC-001/002/003（两档拒绝口径、硬链接/挂载披露、合法外部存储布局显式列出、按 method 分别登记），授权记录 `state/decisions-20261017.md` + 安全差异登记原则。
> **#142 三轮评审合并**：r1 双家无 P0/P1（P2 六条三分）→ r2 codex 便宜修（未测量错误本地拒绝 + transcript 50MiB+1 上限）但 **rebase 机械取 main 侧丢 8 个 pairing 测试被 Claude 评审 P1 拦下** → r3 恢复测试适配 Pair::finish/join_pair_task/eof（178 passed）→ CI 三绿 rebase 合并 main@0e07260。教训已沉淀为 CLAUDE.md 规则（PR #155 合并）：**状态文件冲突可取 main 侧；代码测试必须真合并**。
> **A4 收官**：十项 (a) method 全部 LOCAL_METHODS；#46 按「大步骤收尾自查」关闭（证据表贴 issue：前置 #45/#40 已关、双计分条件机器判据、零占位、178 passed；桌面端人肉验收标注未做待人工）。#134 关闭（六要素闭合）。阶段 A 下一棒：#67（A5 深水区含 #19 六格）、#127（A6–A8）。
> **分支清理（legacy 化）**：本地 35 个旧分支删除、远端 21 个删除（PR 已合并/快照类）；保留 #150 的 ready-admission-probe（Draft 开着）与 archive-safe-candidate（刚合并）。
> **在途**：codex 正做 #154（session 文件级 errno 补证，wt-142 内新分支 codex/metadata-file-errno，前置 #139 关闭）——完成后双家快审 → 合并 → 关 #139。协调者接手提交（codex 沙箱不能写 worktree git 元数据）。
> **坑（新）**：主仓 untracked 文件与 ff 目标同名会静默挡住 `merge --ff-only`（错误信息被管道 tail 吞）——先核对 untracked 同名文件再快进。
> **下一步**：① 收 #154 → 评审 → 合并 → 关 #139；② #150 处置（装置 Draft，A5 需要时评估）；③ 开 #67/#19（A5 深水区）；④ #128（B3–B6）/#129（F3–F6）排队。

> ## 2026-10-17 · 第四轮：#138、#141 双双合并，P0 链清偿推进
> **合并**：#141（metadata 三路径 errno 回放 + baseline_source_guard 工具链护栏 + CI observer job）经两家非作者评审（GLM 全实测 + Claude 无 P0/P1，R2 由协调者裁决维持 P2）+ 作者轮 r2（guard `GIT_*` env 清洗 + 影子仓库负控 + git 路径参数化 + md 三标注）+ rebase 到新 main 后 CI 三绿，rebase 合入 main@1b5798b。#139（P0）**暂不关闭**——新开 #154（P2-跟进：session 文件级 errno 补证，前置于 #139 关闭），按 P2 折中「到点必还」执行。
> **累计（今日四轮）**：#138+#141 两笔真实代码合并、#56 关闭、#151 关闭、#154 登记、4 worktree 清理回收 6G（归档 115M 于 `_tmp/archive/worktrees-20261017/`，含 wt-pr41）、CLAUDE.md 精简（PR #153 已 rebase 新 main 待合并）。
> **教训**：① worktree `_tmp` 归档必须 `rsync -aH --sparse`（codex 用硬链接+稀疏文件，裸 rsync 展开 8.4G）；② 管道 `cmd | tail` 会吞非零退出码，checkout 失败后 && 链继续执行导致误提交到错误分支（已 cherry-pick 修复 + force-with-lease 还原）；③ codex 沙箱仍不能写 worktree 的 git 元数据，提交/rebase 由协调者接手；④ write 全文件会覆盖 board 历史——board 只用 edit 插段。
> **通道矩阵（实测 2026-10-17）**：`gpt-6-astra` ✅（codex CLI 默认，ultra）｜`gpt-6.1-sol` ✅（llm-center /v1 直连）｜`claude-sonnet-5-5` ✅｜`claude-opus-5-5` ✅｜`claude-fable-5-1` ❌｜`claude-fable-5-5` ❌挂起｜GLM ✅。搭配：实现=codex(astra-ultra)，评审A=Claude(sonnet-5-5)，评审B=GLM，重活/仲裁=opus-5-5/6.1-sol；谁写谁不审。
> **下一步（重启即做）**：① 合并 #153（CI 已绿）；② 做 #154（15 分钟级：扩展 record_metadata_io.mts 录文件级 errno 原件→回放→双家评审→合并→关 #139）；③ 开 #46（A4 主线：gateway 透传 + 10 method 本地化，P0）；④ #142（待 #134 精确授权，问人）/ #150（装置，A4 需要时评估）；⑤ 阶段 D #122 起。

> ## 2026-10-17 · 第三轮：#138 已合并、#56 已关，#141 评审中，清理完成
> **合并**：PR #138（WS 握手字节保留）经两家非作者评审（GLM+Claude 均无 P0/P1）+ 134 Rust 测试 + 18 真 TCP 样本 PASS（产品源码 SHA 与原件逐字节一致核验）后 rebase 合入 main@2944ef5；issue #56 已关。邮箱核验：作者 noreply ✅、committer 主邮箱为 CLAUDE.md §五已知服务端缺口（人侧未勾 private），非新问题。
> **#141 评审**：GLM 一家已过（无 P0/P1，全实测：16 Rust 测试 + observer/driver/guard 三套正负控，`_tmp/20261017-pr141-review/glm-review.md`）；Claude 第二家后台进行中（`_tmp/20261017-pr141-review/claude-review.md`）。收敛后合并 → 评估关 #139（P0）。
> **清理（人已授权）**：4 个 codex worktree 全删（cli-server/wt-a4m3/wt-d6/wt-p2c，frame-resource-probe 领先 2 提交已先推远端）；`_tmp` 证据用 `rsync -aH --sparse` 归档至 `_tmp/archive/worktrees-20261017/`（115M——wt-a4m3 的 isolated 快照是稀疏文件+硬链接，勿用裸 rsync 展开）。_build 23G→17G。wt-p2c 的 none/ 为 Node v22 运行时缓存（300K），已核无证据价值，随树删除。
> **通道矩阵（实测）**：`gpt-6.1-sol` ✅ / `gpt-6-astra` ✅（llm-center /v1 直连，codex CLI 默认 astra+ultra）；`claude-sonnet-5-5` ✅ / `claude-opus-5-5` ✅ / `claude-fable-5-1` ❌渠道未启用 / `claude-fable-5-5` ❌挂起；GLM（本会话）✅。搭配建议：实现=codex(astra-ultra)｜评审A=Claude(sonnet-5-5)｜评审B=GLM｜重活/仲裁=opus-5-5 或 6.1-sol；正交原则=谁写谁不审。
> **下一步**：① 收 Claude #141 评审→合并→关 #139；② #142/#150/#151 处置（#151 纯文档 Draft 直接关）；③ 开 #46（A4 主线）；④ 阶段 D #122。

> ## 2026-10-17 · 第二轮：PR#153 已开，#138 评审中（GLM 一家已过）
> **做了**：① 开 PR #153（CLAUDE.md 精简 + 本板接续段）；② #138 双 CI 已绿、此前零评审——GLM 第一家评审完成：**无 P0/P1**（原件 `_tmp/20261017-pr138-review/glm-review.md`）；③ Claude Code 通道修复：CLI 2.1.296 不认 settings 里的 `claude-fable-5-1`，网关（llm-center）有效名为 `claude-sonnet-5-5`。工作树清理授权已在第三轮落实。

> ## 2026-10-17 · 人定「开发优先」重启（main @ b888563 + 本提交）
> **人判**：CLAUDE.md 被 codex 官僚化改写（dfa7df3 起）后，会话陷入写文档循环——origin/main 领先的 48 个提交几乎全是 docs，board 变成无人能接续的密码文。本提交精简 CLAUDE.md（恢复 f80089a 的简洁度 + 保留人定框架 + 新增「开发优先」最高优先规则）。
> **现场核实**：main 已含全部实质成果（ICU collation、gateway errno、set-merge、A5.1 账本、exp014/015 fixtures），`cargo check --workspace` 通过。旁线研究 009/010 已保存在 codex/a5-112-resume@c7d9f53。
> **下一步（按序）**：① 救 Draft PR #138（WS 握手字节保留，真代码，对应 P1 #56）与 #141（metadata errno，对应 P0 前置 #139）走完评审合并；② 评估 #142/#150 去留，关闭纯文档 PR #151；③ 开 #46（A4 gateway 透传 + 10 method 本地化，P0 主线）；④ 阶段 D（#122 起）。分工：codex 写实现、Claude Code 一路评审、GLM 协调+评审。
> **资源**：_build+_tmp 已到 46G 上限、worktree 5 个满；开新树前必须先清理回收（wt-p2c 的未跟踪 none/ 已在主仓，可清）。下方 codex 密码文段落一律作历史，不作为当前指令。

> 📦 2026-10-17 以前的历史段和「OmO 大规模重构案例调研」段已原样移到 [`state/board-archive-202610.md`](board-archive-202610.md)。
