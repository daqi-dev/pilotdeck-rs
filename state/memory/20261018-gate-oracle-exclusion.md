---
name: 20261018-gate-oracle-exclusion
description: 闸门用 --paths 排除 oracle.json 时，GLM 角色会误报「oracle 未入库」P0/P1；PR 描述先写明排除并附 ls-tree 证据
metadata:
  type: feedback
---
2026-10-18 共三次（A6.5-pr1 gate2、A6.4-pr2a gate1、A6.4-pr1 gate2）：闸门为避开 GLM 超时，用 `gate-serial.sh` 的 `--paths` 去掉了 oracle.json，GLM 角色因此在 diff 里看不到 oracle，判成「oracle 未提交，CI 会编译失败」，给出 P0 或 P1。三次都是 `git ls-tree -r HEAD -- <fixture 目录>` 一查就驳回。

**Why:** 误报一次就多一轮人工驳回；要是驳回时没贴证据，后来的读者会以为装置确实有缺口。

**How to apply:** 用排除 oracle 的方式跑闸门后，PR 描述的评审段固定写一句「闸门以 --paths 排除 oracle.json，oracle 已入提交树（ls-tree 证据）」，再贴上 ls-tree 的输出。闸门里的「oracle 未入库」类结论，先 ls-tree 核实，再定级。
