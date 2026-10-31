# 决策卡 · TS 基线 Node 构建口径（2026-10-31 19:4x，AI 代批，待人复核）

| 卡项 | 内容 |
|---|---|
| 卡在哪 | 「Node v22.22.0」这个基线说法里有三份构建：本机 homebrew `node@22 22.22.0_1`（ICU 78.2，共享 libuv 1.52.1），macmini 与 cb8 的 `~/opt` 源码编译版（自带依赖 ICU 77.1，libuv 1.51.0）。#133 / #132 的排序金样和 ICU 门（PINNED_ICU='78.2'）取自本机；cb8 上 Linux 实测在 ICU 门处 BLOCKED（#133 r1 DeepSeek 实测）。macmini 历史上「Node 版本致 regeneration INVALID，须本机补跑」很可能同源。 |
| 谁受影响 | 排序 / ICU / errno 敏感的金样与 Linux 实测：#133、#117、#132，以及今后所有 collation 类对拍。 |
| 能否撤回 | 可以；只影响宿主环境与各装置的 ICU 门，不改判分语义。 |
| 动不动尺子 / 意向 | 不改判分；明确「基线环境」的定义。 |
| 选项 | **A** 以本机 homebrew 构建（ICU 78.2 / libuv 1.52.1）为准：现有金样不用重录；cb8 另编一份同版本 ICU 与 libuv 的 Node（源码包，红线合规），Linux 敏感实测用它。**B** 以 Node 官方源码自带依赖（ICU 77.1）为准：本机改用源码版，重录所有 ICU 敏感金样，工作量大，还会推翻已入库的金样。 |
| 建议 | **A，自决 · 待复核**。 |
| 原则 → 断言 | 对 ICU 敏感的装置必须打印并断言 `process.versions.icu` 与 `process.versions.uv`，不符即 BLOCKED，不得静默改用别的 Node。 |

## 补证（19:5x）
#132a macmini 宿主（HEAD ace4eb1c）：surrogate-collation-matrix 负控 22 KILLED / 5 INVALID，其中 4 条报 `positive control did not pass`。macmini `~/opt` Node 的 ICU 是 77.1，金样是本机 ICU 78.2 录的，这直接坐实了本卡的判断。在 cb8 编好 ICU 78.2 Node 之前，ICU 敏感的宿主只在本机跑。

## 补（20:0x）：cb8 ICU 78.2 Node 已就绪
路径 `/home/clawbox-08/opt/node-v22.22.0-icu78-linux-arm64/bin/node`（`v22.22.0 icu=78.2 uv=1.52.1`），构建报告 `_tmp/20261031-node-baseline/cb8-build.md`。差异：tz 是 2025c（本机 2026a），时区规则敏感的金样不要在 cb8 判。run-linux.sh 默认 Node 路径用它（`PDRS_NODE` 可覆盖）。
