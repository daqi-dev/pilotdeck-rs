我先读装置变更 diff 与冻结规则，再核定标证据。
再读装置 diff 全文与 FROZEN/accepted-diffs 的变更。
再读装置代码变更（94.1/94.2 的实现与测试）。
再看测试变更与定标 summary/batch 元数据。
summary 符合预期形态。接着核 batch.json 与 attempts 目录。
再提取关键字段：运行数量与唯一性、ref16 两次尝试、workspace_packages、装置哈希绑定。
核对 ruler 报告关键字段与 10-14 批次档案。
再核 pairs/U1/U2/proxy_compare 细节，并调出 10-14 批次档案对照。
上一条输出被 controls 撑爆了，改用紧凑提取。
修正提取脚本。
核对 U1/U2 类构成，并调出 10-14 批次档案对照。
U1/U2 类构成合规。现在调 10-14 批次档案。
调 10-14 厚 ruler 对照（基线/依赖摘要、pairs、U1/U2 类）。
关键差异出现：基线与 node_modules 摘要在 10-14→10-18 之间变了。核批次内一致性、基线现状与登记。
核 final-ruler 顶层计分 pairs 与 run 身份，并读两家确认轮。
机器门与计分结构确认。读两家确认轮与基线变更登记情况。
DeepSeek 的 P1 与 GLM 结论相反（跨行吞并），按「仲裁要实测」跑最小实测，并抽查 FROZEN 哈希与 HEAD 一致性。
实测驳回 DS 的 P1。最后抽查 FROZEN 哈希与工作区实算一致性、CI 状态。
最后核一点：基线演进是否有登记痕迹。
材料读毕（18 次工具调用，未写任何文件）。以下为冻结评审完整结论。

---

# B2 ui/server 尺子重冻结评审（#94，claude/b2-94 @ ce77149，批次 b2-calibration-2026-10-18-a025aa97）

## ① 冻结程序满足性：满足

- **同日**：26/26 遍 `capture_utc_day` 全为 2026-10-18，与 batch `utc_day` 一致。
- **20/5/1**：ref1–20 池 + ts1–5 计分 + proxy；25 个 run_id / 25 个 recorder PID / 25 个 recording SHA256 全唯一，proxy 另计；固定 `fixture_path` 单 slot 26 遍一字不差。
- **HEAD 与装置一致**：batch.head = summary.head = ce7714962a…，worktree 干净且 HEAD 同为 ce77149；FROZEN 草稿 8 个改动文件哈希我逐个实算 8/8 相符；ruler 报告内 `apparatus_sha256`（如 apparatus.py `7bab891b…`）与 `pinned.apparatus` 均等于工作区现状。
- **仓外 checkout**：root=/Users/da/ws/pdrs-b2cal-20261018，批内独立 cargo-home/cargo-target（构建产物 ddf42642…= 每遍 `recorder_proxy`），无 `_build` 旧产物钉住。
- **机器门全过**：self_compare / per_rule_negative / same_bad_rejected = true，`independent_review: null` 不自填；计分 10 对 10/10、proxy_compare PASS、26 遍时钟全 PASS、`approximate: False`；verdict=PENDING_REVIEW 正是评审前应有状态。磁盘门：cap_bytes=85899345920（=80 GiB，du 1024 口径），实测 50.1 GB、投影 52.6 GB，与 CLAUDE.md 2026-10-17 人定精确一致。

装置变更本体核实：94.1（空行/注释行跳过、行内 `#` 整份作废）与 acceptance.md 新措辞逐句对应，实测 `alice # contractor` → 整份白名单为空、注释样例只剩 `zhengdaqi`；94.2 手写 YAML 子集对基线原文（verbatim fixture @ cc7c4e6）解析为 `['apps/desktop','ui','packages/sdk']`，NBSP/U+3000/CRLF/tab/BOM 等 40+ 反例 fail-closed；CAP_BYTES 仅出现于 calibrate.py 定义/启动门/测试，compare/check_ruler/summarize 的 FROZEN 哈希未变——不进判分路径成立。accepted-diffs.md 已追加 #94 条目（GLM 确认轮 P2-1 已落地）。

## ② ref16 重采：合规，无偷换参考池

首采（attempts/ref16-8e1eb954…，全证据含 result.json 原档保留）被健康门拒收：`wall clock regression relative to monotonic clock`——这是装置按设计拒收时钟异常，不是判分失败。重采只替换了该未接受轮次一次（27 attempts = 26 接受 + 1 拒收）；重采发生在计分开始前（池末 wall_end 1792304288258 < ts1 wall_start 1792304398671），不触「计分开始后不可替换参考池」；新 ref16 是全新 run_id/PID/录制哈希，池 20 遍身份无重叠；计分 ts1–5 与 proxy 无任何重试记录。

## ③ 与 10-14 批次对照：结论同构，但基线变了（不是解析器变了）

- **一致**：两轮 ruler 同构——k=5、池 20 captures、严格两两 190 对保留、计分 10/10、proxy_compare PASS、26 时钟 PASS、三布尔门同 true、U1/U2 位置相同（projects_updated[0].changeType / [1].changedFile），类值集合相同（change/add；projections.json/.jsonl），各类成员 ≥2、双连接同值。
- **不一致（如实报告）**：严格对分布 132F/58P → 81F/109P，类大小 add 6→3、change 14→17、U2 .jsonl 6→2。原因是**基线自身演进**：baseline 57fcd761→c248d699（resources/PilotDeck @ cc7c4e65，主仓不跟踪该嵌套仓），node_modules b744de7b→a619eaf9（node e6019c69 不变）。**「依赖摘要不变」不成立**——但变动源头是基线 bump，不是 94.2：摘要算法未动，解析器等价性由基线原文 fixture + 差分单测保证；批内 26 遍基线/依赖/node/recorder 摘要完全一致，同源池要求满足。workspace_packages 本批为 `['apps/desktop','packages/sdk','ui']`；10-14 原始 batch.json 已随仓外 checkout 清理灭失，无法直接对（仅厚 ruler 在主仓 `_tmp/archive/` 可查基线摘要）。
- **仲裁（实测）**：DeepSeek 确认轮唯一 P1（`_YAML_OPAQUE` 字符类吞 `\n` 跨行）不成立——解析器先 `text.split('\n')` 逐行处理，行内无换行可吞；实测 `packages:\n  - ui\nfoo: bar\n` → `['ui']`，与 PyYAML safe_load 逐值相等；引号内 NBSP 同样与 PyYAML 一致。DS 其余「需核对」项（CAP_BYTES 判分引用、check_frozen DRAFT 中间态、80 GiB 人定存在性）均已核实无问题。

## ④ 是否同意重新冻结：同意，附三项落地条件

机器门、程序、证据链、两家确认轮（GLM 0/0；DS 的 P1 经实测驳回、P2 均已由 GLM 实测或本次核查关闭）均支持。条件：**(a)** 终稿去掉首行「待定标」，把 Approval-Basis / Calibration-Evidence / Independent-Reviews 换成 10-18 批次（summary 与两 ruler 已在主仓 `_tmp/archive/b2-calibration-2026-10-18-a025aa97/` ✓、评审在主仓 `_tmp/20261018-b2-94/` + 本份，三份 realpath 互异 ✓）；**(b)** 终稿如实写「ref16 首采 REFUSED 后按程序单轮重采」，不得沿抄 10-14 的「无重采/REFUSED」措辞；**(c)** 合并仍走完整闸门（CI 绿 + simplify + pr-ready + 双谱系无 P0/P1），本评审为其中一家。

## 问题分级

| 级 | 条目 |
|---|---|
| P2-1 | 基线演进无留痕：57fcd761→c248d699（cc7c4e65）未见于 decisions/board；FROZEN 清单不钉基线/依赖摘要，跨批漂移只能靠翻厚 ruler。建议终稿 Calibration-Evidence 附注本批 baseline/node_modules 摘要与 workspace_packages，并开 `P2-跟进`（前置于下次定标）登记基线演进规则 |
| P3-1 | 终稿叙述须如实记 ref16 重采（条件 b，措辞要求） |
| P3-2 | CAP_BYTES 边界通过侧用例缺口与 `>`/`>=` 边界（GLM P3-3、DS P3 同源，顺手补） |

VERDICT: P0=0 P1=0 P2=1 P3=2
冻结结论：同意
