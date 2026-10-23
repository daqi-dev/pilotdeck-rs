# gateway 对拍尺子冻结记录（阶段 A · exp006）

> **2026-10-23 重登记（PR #536，透明披露）**：accepted-diffs.md 末尾新增「已知未对齐（未批准，待人批）· A7.2 turn 读面」一节，登记 A7.2-READ-LASTSEQ-DEPTH-001（不复刻 V8 栈溢出临界层数），按决策卡 card-v8-stack-depth 登记，状态未批准，不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希，其余不变。两家评审见 PR。

> **2026-10-23 重登记（board PR #550，人批记录）**：accepted-diffs.md 末尾只追加「人批记录 · #505（#502）补登的 32 条」一节，记录人对 32 条的批复（1 条关闭、5 条要求修、26 条接受为已知差异，接受不豁免判分）；不改任何条目原文、不改判分语义；只重登记 accepted-diffs.md 哈希。

> **2026-10-23 重登记（PR #539，透明披露）**：accepted-diffs.md 末尾追加「A6-JSVALUE-DEPTH-001 追加说明」：fingerprint 合一后 runtime 路径保持 2000 门限、host 路径保持无门限显式栈，两路径相对 TS 的剩余差异写明；不改原条目、不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希。两家评审见 PR。

> **2026-10-23 重登记（PR #535 修复轮 2，更正）**：accepted-diffs.md 末尾追加「A7.4 user_dialog HTTP store 条目更正」：MAX-CONNECTIONS、431-408-SHAPE、KEEPALIVE、CLIENT-ERROR-CLASS、CLIENT-BODY-UNITS、AUTH-BEFORE-BODY 六条各追加 -R1（原条目保留），均未批准，不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希。两家评审见 PR。

> **2026-10-23 重登记（PR #535，透明披露）**：accepted-diffs.md 末尾新增「已知未对齐（未批准，待人批）· A7.4 user_dialog HTTP store」一节，共九条（鉴权先于读 body、请求体 2 MiB、两段超时、未实现 keep-alive 5 s、连接上限 64、431/408 形状、键序、客户端错误类名、客户端上限计量单位），全部未批准，不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希，其余不变。两家评审见 PR。

> **2026-10-23 重登记（p2-hooks PR，透明披露）**：accepted-diffs.md 末尾追加「A6.7 hooks 执行器状态更新与新增条目」：A6.7-HOOK-HTTPS-001 当前状态为已实现（追加说明，不改原条目）；新增 A6.7-HOOK-OUTPUT-001（command hook 输出封顶）、A6.7-HOOK-TLS-TRUST-001（信任源与代理），均未批准，不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希，其余不变。两家评审见 PR。

> **2026-10-23 重登记（PR #533，透明披露）**：accepted-diffs.md 末尾新增「已知未对齐（未批准，待人批）· A7.2 submit 编排」一节，登记 A7.2-SUBMIT-FAILURE-GUARD-001（FailureGuard 运行时未移植，启用时显式拒绝，推迟到 A7.2-pr4b），状态未批准，不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希，其余不变。两家评审见 PR。

> **2026-10-23 PR #513 修复轮 5 重登记（透明披露）**：按 GLM r3 P2 #1（实现方在修复轮 5 中补登，协调者复核后保留），accepted-diffs.md 只追加 D2-CHATSEARCH-REGEX-GIANTQUANT-001（巨量词 V8 饱和语义与 fancy-regex 编译边界），两次真实 TS 重采及 Rust 分歧复算见 RESULT「修复轮 5」。状态为已知未对齐、未批准，不新增豁免、不改变冻结判分语义；仅更新 accepted-diffs.md 哈希，独立评审由协调者续办。

> **2026-10-23 重登记（PR #513，透明披露）**：accepted-diffs.md 末尾新增「已知未对齐（未批准，待人批）· D2 CLI chat search」一节，登记 D2-CHATSEARCH-REGEX-BACKTRACK-001（TS 正则灾难性回溯长时间不返回，Rust 回溯上限有界失败）。状态为未批准，不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希，其余不变。两家评审见 PR。

> **2026-10-23 重登记（#518 PR，透明披露）**：accepted-diffs.md 末尾新增 A7.5-REPLACEMENT-ACTIVE-GETTER-PANIC-001（超时回滚时 hasActiveTurn 端口 panic，Rust 告警后重新约时，TS 拒绝），状态未批准，不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希，其余不变。两家评审见 PR。

> **2026-10-23 重登记（#516 PR，条目格式更正）**：accepted-diffs.md 末尾只追加「A7.3 interaction 边界条目更正（#516）」小节，内含 A7.3-INTERACTION-BOUNDARY-001-R1（把原条目按标准六字段重述，原条目逐字保留）；仍为边界登记、非豁免，不改判分语义；只重登记 accepted-diffs.md 哈希，其余不变。两家评审见 PR。

> **2026-10-23 A7.2-pr1 修复轮 8 重登记（PR 待开，sink 所有权条目承诺范围更正）**：按本单 codex r8 两条 P1 的要求（评审原件 `_tmp/20261022-a72-pr1-replay/rv-codex-r8.md`），对 accepted-diffs.md 的 A7.2-COORDINATOR-SINK-OWNERSHIP-001 做**只追加更正**（原条目文字未删改，范围以追加块为准）：① 轮 7「最后一个注册 drop 后 emit 拒收」并未成立——emit 以 sink 强引用存活代理注册存活，在途交付与队列均持强 sink；轮 8 起注册存活标记与交付用强 sink 分离（map 弱持注册记录、队列只持内层 sink）；② 「泄漏已从设计上消除」收窄为仅 coordinator 侧环——宿主侧 `Host → registration → sink → Host` 环任何库 API 无法阻止，本轮删除代码注释与轮 7 登记注中的过度承诺，改为文档化所有权约定（sink 回调宿主须弱捕获；会话结束显式 drop 注册或调用新增显式注销 `unregister`），宿主侧残余环按条目登记为已知残余。新增五条钉住测试（断言消息含编号），negative.py 各配专属变异（实测转红）。只重登记 accepted-diffs.md 哈希，其余文件哈希不变。

> **2026-10-23 A7.2-pr1 修复轮 7 重登记（PR 待开，sink 所有权模型差异补登）**：按本单 gate7 P1 #1 的要求（评审原件 `_tmp/20261022-a72-pr1-replay-gate7/`），对 accepted-diffs.md 的 A7.2 节做只追加变更：新增 A7.2-COORDINATOR-SINK-OWNERSHIP-001（sink 所有权模型：coordinator 只持 `Weak` sink、`start` 返回强所有者 `TurnSinkRegistration`，最后一个注册 drop 后 emit 拒收——TS 的 sink 存 `Map` 由 GC 管生命周期；类别实现选择、未批准，引用循环泄漏本身已按 gate 优先项从设计上消除、不登记）。coordinator.rs 模块文档加账本指针，新增两条钉住测试（断言消息含编号），negative.py 各配专属变异（实测转红）；`with_run_id` 非对象事件改为照 TS `{ ...event, runId }` 展开（真实 TS 金样新增 `with-run-id-non-object-spread`，oracle.json 重生成两次逐字节一致，含 number/string/array/null/boolean 五类与缺会话负控）。只重登记 accepted-diffs.md 哈希，其余文件哈希不变。（轮 8 注：本条中「已从设计上消除」的表述已由轮 8 重登记注收窄，原文保留。）

> **2026-10-22 A7.2-pr1 修复轮 6 重登记（PR 待开，交付语义差异补登 + 状态行统一）**：按本单 gate6 sibling P1 与 Grok r6 P1 的要求（评审原件 `_tmp/20261022-a72-pr1-replay-gate6/sibling.md`、`_tmp/20261022-a72-pr1-replay/rv-grok-r6.md`），对 accepted-diffs.md 的 A7.2 节做只追加变更：① 在 A7.2-REPLAY-RETENTION-TIMER-001 条目末尾追加「当前状态（以本行为准）」一行（已知未对齐、人批 2026-10-22、前置于 A7.2-pr5、非已接受差异），消除同条目内「未批准/待人定」与「人批」的表述冲突，原有文字未删改；② 新增 A7.2-COORDINATOR-DELIVERY-001（多线程宿主下的事件交付边缘与 TS 单线程同步交付不一致：(a) `retain_terminal`/替换性 `start` 返回后旧 sink 的在途交付、(b) sink panic/`dispose` 静默丢弃已接受未交付事件、(c) 重入 `emit` 的 panic 归因、(d) 无界交付队列无背压；类别实现选择、未批准、前置于 A7.2-pr5），补登依据 gate6 sibling P1——此前只在 `coordinator.rs` 模块文档自我披露、账本无条目；③ coordinator.rs 模块文档加账本指针，并新增四条钉住测试（断言消息含编号），negative.py 各配一条专属变异（实测转红）。只重登记 accepted-diffs.md 哈希，其余文件哈希不变。

> **2026-10-22 人批重登记（PR #487）**：A7.2-REPLAY-RETENTION-TIMER-001 人批为已知未对齐，前置于 A7.2-pr5 对齐（采纳决策卡建议 B）；只重登记 accepted-diffs.md 哈希。

> **2026-10-22 A7.2-pr1 修复轮 5 重登记（PR 待开，范围与证据更正）**：按本单 codex r5 P1 #2/#3 与 Grok r5 P1 的要求（评审原件 `_tmp/20261022-a72-pr1-replay/rv-codex-r5.md`、`rv-grok-r5.md`），对 accepted-diffs.md 的 A7.2 节做**只追加更正**：在原 A7.2-ZERO-RETENTION-TIMER-001 条目下追加「更正」一行（该条范围写错——差异不限于 `terminalRetentionMs=0`，生产 `ThreadTimer` 下所有保留时长的到期回收都不等调用方当前同步段与微任务；「不可避免」归类依据不足，`with_timer` 已提供宿主调度注入口），再新增 A7.2-REPLAY-RETENTION-TIMER-001（类别：实现选择，待人定——接受，或在 A7.2-pr5 接宿主串行计时器后对齐；基线证据换为真实 timer 探针：不替换 `setTimeout`，retention 0/5/30000 的同步段与 Promise 微任务内均可见、下一个 timer 回合才回收，`_tmp/20261022-a72-pr1-replay/probe-retention-real-timer.mts`，两次运行一致）。原条目文字未删改。同步更正代码注释与 fixture README 的范围表述，Rust 测试宣称收窄为可证部分（生产忙转测试只断言最终回收、不对「忙转期间已回收」做宣称；边界改由注入受控计时器 + 回调执行标记钉住，覆盖 0/5/30000）；`ThreadTimer` 回收行为未改。只重登记 accepted-diffs.md 哈希，其余文件哈希不变。另：本分支已 rebase 到 main，A6.4 stats 节与 A7.2 节已在同一文件并存，修复轮 4 注中「合并时须 reconcile」的提示已完成使命（Grok r5 P3）。两家评审见 PR；人批另行。

> **2026-10-22 A7.2-pr1 修复轮 4 重登记（PR 待开，透明披露补齐）**：按本单 codex r4 P1 #2 与 gate4 sibling P1 的要求（评审原件 `_tmp/20261022-a72-pr1-replay/rv-codex-r4.md`、`_tmp/20261022-a72-pr1-replay-gate4/sibling.md`，两评审均指出 A7.2-ZERO-RETENTION-TIMER-001 差异此前只在代码注释与 fixtures README 自我披露、账本无条目），在 accepted-diffs.md 末尾新增「已知未对齐（未接受，待人批）· A7.2 turn replay」一节：A7.2-ZERO-RETENTION-TIMER-001（零保留终态的生产回收时机：真实 TS 的 `setTimeout(0)` 依赖宿主事件循环，同步忙转 20 ms 仍可见；Rust 生产 `ThreadTimer` 取 Node 自身 1 ms 下限、到期即回收）。状态为「未批准，非已接受差异」，不新增豁免、不改判分语义、不改门禁；登记流程沿用 PR #476 对 gate3 sibling P1 的同类补登（只追加待批条目，check_frozen 通过，人批前不算已接受）。只重登记 accepted-diffs.md 哈希，其余文件哈希不变。两家评审见 PR；人批另行。注：本分支基线早于 PR #476 的 A6.4 stats 节，合并时须与其追加节 reconcile 后一次性重登记哈希。

> **2026-10-23 重登记（PR #510，边界登记）**：accepted-diffs.md 末尾追加 A7.3-INTERACTION-BOUNDARY-001（pending 输入类型域与线程映射），类别为边界登记、非豁免，不新增已接受差异、不改判分语义；只重登记 accepted-diffs.md 哈希，其余不变。两家评审见 PR。

> **2026-10-22 人批重登记（PR #476）**：A6.4-STATS-RETENTION-001、A6.4-STATS-BASELINEMODEL-001 两条接受为已知差异（用户本人在协调者会话批准），并要求 Rust 补实现（见 accepted-diffs.md 该节末尾的跟进 issue）；只重登记 accepted-diffs.md 哈希。

> **2026-10-22 修复轮 3 重登记（PR #476，透明披露补齐）**：按本单 gate3 sibling P1 的要求（`_tmp/20261022-a64-pr9b-gate3/sibling.md`，工作指令「gate3 的 P0/P1 一并修」），在 accepted-diffs.md 末尾新增「已知未对齐（未接受，待人批）· A6.4 stats」一节：A6.4-STATS-RETENTION-001（retention 只解析 `toISOString` 规范形，真实 TS 删除 8 类非规范行而 Rust 保留）与 A6.4-STATS-BASELINEMODEL-001（未知 `stats.baselineModel` ref 的 fatal 诊断未实现）。两条状态均为「未批准，非已接受差异」，不新增豁免、不改判分语义、不改门禁；真实 TS 探针（固定时钟与 retention）与 Rust 侧机器判据见该节。只重登记 accepted-diffs.md 哈希，其余文件哈希不变。两家评审见 PR（gate4）；人批另行。

> **2026-10-21 重登记（PR #391，人批）**：accepted-diffs.md 追加 A6.5-MCP-OUTSCHEMA-REGEX-001（正则长串预算，人批 13:10）与 A6.5-MCP-OUTSCHEMA-DEPTH-001（深度 / 宽度预算 2048，人批 14:25），见 state/decisions-20261021.md；其余文件哈希不变。两家评审见 PR。

> **2026-10-21 重登记（PR #406，人批）**：accepted-diffs.md 追加 A6.5-WEBFETCH-DEPTH-001（web_fetch HTML→Markdown 深度上限 256，向严，人批 2026-10-21 15:25，state/decisions-20261021.md）；其余文件哈希不变。两家评审见 PR。

> **2026-10-21 修复轮 5 重登记（PR #406，表述准确性修正）**：依据本 PR 引入 A6.5-WEBFETCH-DEPTH-001 的人批（2026-10-21 15:25），将「深度 ≤ 256 两侧逐字节一致」收窄为除 pr9c 两条解析器版本差异外，并在 accepted-diffs.md 末尾透明披露真实 TS / Rust 输出与测试名。两条均「未批准，非已接受差异」，不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希，其余不变。两家评审见 PR。

> **2026-10-17 四登（#187，只改路径解析、不改判分语义）**：run_round.py / session_domain.py 的默认 baseline 由主仓绝对路径改为「显式参数 > PDRS_BASELINE > 本 checkout 的 resources/PilotDeck」，缺失时两者都 BLOCKED（exit 1 / 2）并提示、不建输出目录（session_domain 按 six_cells.mts 实际导入的 10 个 E1 源码入口核对，不要求 dist）；driver.mts 去掉主仓回落，缺 PDRS_BASELINE 即报错（run_round 总会设置）。不再在 worktree 里静默用主仓 baseline。真实数据复算：同树两轮 TS 自比 PASS（`_tmp/20261017-issue-187/r2/compare-ts-a-b.json`，含 driver.mts 改动）；无 baseline 负控两入口均 BLOCKED（`r2/negative.log`：run_round exit 1、session_domain exit 2 且不建输出目录）；E1 calibrate 正向：status PASS、72 例 360 次测量、frozen exit 0、e1Controls PASS（`r3/calibrate/report.json`）；E1 前检正/负控（`r3/precheck-probe.log`、`r3/negative.log`）。两家评审见 PR。**AI 代批，待人复核**。

> **2026-10-17 重登记（两类，均见 PR）**：① accepted-diffs.md 追加 A4-ARCHIVE-SEC-001/002/003（#142，人批 2026-10-17「批准 #134 的登记范围」，state/decisions-20261017.md）；② A5.3 新增 E1 适配脚本三件（session_domain.py / session_observations.mjs / controls/session_domain.py）入白名单并登记哈希（plan 005：A5 PR 带理由+双评审登记新 hash）。

> **2026-10-17 三登（#167，P1 缺口修复）**：validate_calibration 新增 repetition/phase/variant 与预注册 ID 一致性核对及空账本拒绝（audit 原件见 PR）；负控 24 条含新两条。计划 005 程序：理由+双评审+实证复算。

> **2026-10-15 重登记（issue #68，人批）**：accepted-diffs.md 新增 A4-FILE-TOCTOU-001（project_file_read 检查后符号链接替换，原版安全缺陷）；其余文件哈希不变。两家评审见 PR。

> **2026-10-14 重登记（A4 前置 #45，PR #50）**：时钟归一收紧到容器组合、装置哈希改 12 件显式白名单（未登记脚本即 FAIL）、具名键前置写入 normalization；两家（GLM、Claude 替补 DeepSeek）两轮均无 P0/P1；rebase 后正式重跑 `check_ruler` 机器项全 true。**AI 代批，待人复核**。
> 2026-10-14 冻结。**AI 代批，待人复核**（依据：`check_ruler.py` 机器项全 true——TS 自比 k=5 十对、五个不同 gateway 进程、阳性对照 33 项主规则命中、两侧同坏拒绝、代理一致；双谱系评审第 3 轮 GLM、DeepSeek 均 `independent_review: true`，无 P0/P1，P2 转跟进 issue）。
> 冻结后改任何一份都要另开 issue、两家评审同意、写明理由（`CLAUDE.md` §五「尺子不许自己改」）。A3 的 schema 产物与脚本另见 `schema-FROZEN.md`，不在本表。
> 校验：`python3 scripts/gateway-parity/check_frozen.py`（下面每行「sha256 两空格 相对本目录的路径」）。

886d12cdd36a5ecac53d314fabd52fed9ecd093b2a07939bbcf553d88b37e519  acceptance.md
122eb3f8f75300144f76445bb7f9576ae06c1ae7795481c13d50b0f92376fcfa  normalization.md
6454f8058f04d797b21ec56d4007485a52d5389fdc55b0fac791414bb9ecc9b0  accepted-diffs.md
cdea9efadae918a7894f54d3fba7bb535e4da2cda961cfd54997e956cc4994b0  ../../scripts/gateway-parity/check_frozen.py
3517f52c318da237f17d8634ee4bf3da64f0d8c063feecd38cd664c33ea12d89  ../../scripts/gateway-parity/check_ruler.py
967969d8b1b1cea7bdc25d361ac39eeda964e23f8bec64f4fceadd30898e8a9e  ../../scripts/gateway-parity/common.py
4ae7570b725bfb79fdef01b0860d67f0a36864582a667da5a1e53eba1ecb8436  ../../scripts/gateway-parity/compare.py
0bf89d5277e04fea0da89da8d24a2c05680d1d77a6c20390a8a27a2cd28b0da5  ../../scripts/gateway-parity/controls/prerequisites.py
13e1f91b3fe5c81140dd8fcbf9d6ac24695ec8f1098b330c6ab0bc4ef2c382ea  ../../scripts/gateway-parity/controls/run_all.py
5f8e906fa532a58b994dff4f5e6438b07e31f14195b8142b75b398b6778e9e63  ../../scripts/gateway-parity/driver.mts
95fe265a7b7512f1969f19b7ee2d14133ce56916fb25c0953a84b5ec05568f74  ../../scripts/gateway-parity/mock.mjs
3a758aa57cbb5d8129c2e9b73c0baa04028a87aa7bae5dba6d8519f713869692  ../../scripts/gateway-parity/observer.py
d3e99961f28d4fdbd8521be51b689e6f075bbc78412a41387b5dc2b46202efca  ../../scripts/gateway-parity/run_round.py
9d8a1825e4a152affbcb6df6c8e3e682edad634281e782ad21c1493e673c1d67  ../../scripts/gateway-parity/test_safety.py
e1dfeac23d5cfd0777fd2836cb1011430c73d6b3b10fe44724738194f49901df  ../../scripts/gateway-parity/watcher.mjs
70f248715b6f3c3c20127df963999706e1432765340be411a0c98340089ba0bc  ../../scripts/gateway-parity/session_domain.py
855136b60c4579eda257224fe75c8faaf89ad54eeaa85abff9eb369e02cc6661  ../../scripts/gateway-parity/session_observations.mjs
31d0fe7dd11da319bacd60cabff27053cc0d0e28d3c386adea4f931488099d16  ../../scripts/gateway-parity/controls/session_domain.py

> **2026-10-22 修复轮 8 重登记（PR #406，透明披露补齐）**：按本轮明确授权，accepted-diffs.md 的「已知未对齐（未接受，待 pr9c）」表格只追加第三条（240 层格式化元素 + 宽 P 子树在真实 TS 抛 `Invalid array length`、Rust 成功返回 Markdown），保留原有条目原文，并追加适用范围说明；本条属 V8 数组上限运行时行为，与深度批准无关。真实 TS 重测 2 / 4 MiB 并二分至 256 KiB 区间，Rust 2 / 4 / 8 MiB 原值与输出 SHA256 见 `_tmp/20261021-a65-pr9b/RESULT.md`「修复轮 8」。状态为「未批准，非已接受差异」，不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希，其余不变。两家评审见 PR。

> **2026-10-22 修复轮 9 重登记（PR #406，select 家族透明披露补齐）**：按本轮明确授权，在「已知未对齐（未接受，待 pr9c）」表只追加 select 内标签家族一行，保留已有条目原文；真实 TS（默认 Turndown + domino）与正式 Rust 实测 11 例，其中 9 例不一致录入 `parserGaps`，2 例一致仅记 RESULT。追加说明当前为 3 类解析器差异、1 类宽子树运行时差异，共 4 条已知未对齐；select 家族未穷举。第三条补入 2 MiB 完整真实 TS 金样与 Rust 逐字节回放、4 MiB Rust 成功断言。两侧原值、负控与验证见 `_tmp/20261021-a65-pr9b/RESULT.md`「修复轮 9」。状态仍为「未批准，非已接受差异」，不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希，其余不变。两家评审见 PR。

> **2026-10-22 修复轮 4 重登记（PR #476，issue #493）**：按用户本人明确指令，在 A6.4-STATS-BASELINEMODEL-001 节末追加「已实现，差异消除（本 PR 修复轮 4）」及实现/金样依据；Rust 补齐 baselineModel 与 modelPricing 键的引用校验，保留原账本条目。只重登记 accepted-diffs.md 哈希，不新增豁免、不改判分语义；两次真实 TS 生成一致与负控证据见 `_tmp/20261022-a64-pr9b/RESULT.md`「修复轮 4」，两家评审由协调者续办。

> **2026-10-22 人批重登记（issue #496）**：按用户本人选 B 的明确授权，accepted-diffs.md 只追加 A6.4-STATS-SIDECAR-001（降级追加写旁路文件，向安全偏离 TS），修复 retention 锁超时追加与压缩 rename 交错丢账；保留判断、账目行格式与聚合输出不变。真实 TS 两次生成逐字节一致、原版丢账实测及各类变异负控见 `_tmp/20261022-issue-496/RESULT.md`；仅重登记 accepted-diffs.md 哈希，其余哈希与判分语义不变，独立评审由协调者续办。

> **2026-10-22 issue #494 补实现重登记**：按本单用户明确授权，只在 A6.4-STATS-RETENTION-001 末尾追加 V8 共享解析器接入、三个时区真实 TS 回放依据与仍保留的未验证本地 TZif 子集；未宣称全量消除，PR 号待协调者创建后补记。仅重登记 accepted-diffs.md 哈希，不改阈值、装置与判分语义。金样两次一致、变异负控及测试证据见工作区 `_tmp/20261022-issue-494/RESULT.md`，独立评审由协调者续办。

> **2026-10-23 issue #502 只追加补登重登记**：按本单用户明确授权，全仓自述差异审计后，accepted-diffs.md 追加 32 条「已知未对齐（未批准）」六字段记录。既有账本原文逐字节保留，AI 决策不冒充人批，既有人批条目只在代码加编号引用；不新增豁免、不改阈值或判分语义。五处低成本对齐（count URL、managed env、合法扩展年份、transcript/run registry JSON 语法文案）的真实 TS 金样两次一致及独立变异负控见 `_tmp/20261022-issue-502-diff-backfill/RESULT.md`。仅重登记 accepted-diffs.md 哈希；未改在途 turn/edgeclaw 文件。独立评审由协调者续办，本记录不声称已完成双评审。
