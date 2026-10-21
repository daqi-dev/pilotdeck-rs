# gateway 对拍尺子冻结记录（阶段 A · exp006）

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
0cbf85f56a58269d89a61781409a313881016049b0807f1495531ab08d6d633c  accepted-diffs.md
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
