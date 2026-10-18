# 协调者核查结论

逐条回应 DeepSeek 的三处「需核对」，并给出最终裁决：

## 1. 重采程序（对应 DS ②）

DS 要求的规则依据已定位：`scripts/ui-server-parity/README.md:34` 明确「某遍 REFUSED 时只重采该遍」，`:40` 明确「重采只能替换那一个尚未接受的轮次；PASS 轮次不可覆盖」。ref16 原档保留在 `attempts/`，REFUSED 日志原文为 `"reason": "wall clock regression relative to monotonic clock"`，与 DS 推测一致。重采发生装置哈希未变、同一 UTC 日、且只替换 REFUSED 那一遍——**完全符合 README 规定的单遍隔离重采程序**。此项应从「需核对」降为**已核实、合规**。参考池未被偷换：ref16 重采后的 identity 摘要与其余 25 次逐项相同。

## 2. 装置与 HEAD（对应 DS ① 部分）

确认：批次 head = `ce77149`，与 claude/b2-94 的 HEAD 一致；94.1、94.2、CAP_BYTES、accepted-diffs 四项变更均包含在 `ce77149` 内。装置 pinned 脚本集一致、proxy 二进制 `ddf426…7a6c` 批次内恒定。此项**已核实**。

## 3. 依赖边界（对应 DS ③ 的核心缺口）

DS 因缺 `ts-ruler-*`/`final-ruler-*` 内容无法比对 workspace_packages 与依赖摘要，故标「需核对」。现补齐关键事实：

- 新批与 10-14 批的 `node_modules.coverage` **逐项相同**：node_modules 1594 个、workspace_packages = `[apps/desktop, packages/sdk, ui]`、link_targets 1483 个、covered_roots 4 个。→ workspace_packages 与依赖边界不变，③ 的实质结论一致。
- `node_modules.sha256` 确实不同（新 `a619eaf…`，旧 `b744de7…`），但这正是第 4 条要定级的问题。

## 4. 摘要差异的定级（本核查的关键）

原因查明且可复现：基线 `ui/` 在 10-15 被写入 `ui/dist`（Vite 构建产物，约 200 文件，16:37）与空目录 `ui/node_modules/.vite-temp`。B2 的覆盖根含 `ui`，故 `node_modules.sha256` 变了；`apps/desktop`、`node_modules`、`packages/sdk` 三个根自 10-14 以来零改动。变的是**基线构建产物**，不是依赖、不是装置、不是参考池。

**定级判断**：这不构成 P0/P1。理由：（a）冻结程序要求每批自己同日定标，不要求与旧批摘要相同；（b）新批 20/5/1 全部在同一摘要下录制，批内一致；（c）变更源是基线的可重建构建产物（`ui/dist` 是 Vite 输出，可由源码重建），非依赖清单或 lockfile，不影响「依赖边界不变」这一实质结论。但它确实改变了摘要指纹，若审批材料只写「摘要一致」会造成审计误导。因此应定为 **P2（审批材料需显式说明的记录缺陷）**，而非 P0/P1 的实质性冻结障碍：要求在新审批依据中写明「node_modules.sha256 变化源于基线 ui/dist 构建产物，依赖边界逐项相同」并附对照表。DS 原拟的 P2 三项中，ref16 重采合规依据与 ruler 一致性比对现均已闭环，撤销；仅保留摘要差异说明这一项 P2。

## 最终结论

- ① 定标：同日、20/5/1、HEAD/装置一致、机器门全过——**满足**。
- ② ref16 重采：单遍隔离重采，符合 README:34/:40，参考池未换——**合规**。
- ③ ruler/依赖边界：workspace_packages 与依赖覆盖逐项相同——**一致**；摘要差异见 ④。
- ④ 摘要差异：定为 **P2 记录项**（须在审批材料中显式说明来源与对照），不阻塞冻结。

据此**同意重新冻结**，条件是新审批依据中必须包含第 4 条的摘要差异说明与两批 node_modules 覆盖对照表（P2 闭环后登记）。DS 原 P2=3 中两项已核实撤销，P2 收敛为 1；P3 建议保留（对照表+ref16 事件说明入档）。

VERDICT: P0=0 P1=0 P2=1 P3=2；冻结结论：同意（附 P2 记录项须入审批依据）