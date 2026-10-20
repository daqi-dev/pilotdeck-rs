---
type: ops
number: "002"
date: 2026-10-18
title: 仓库迁入 daqi-dev 组织、main 改 ruleset + merge queue、组织 Project 看板
tags: [repo-settings, merge-queue, ruleset, project-board]
related: ["001"]
---

# 仓库迁入组织 · merge queue · 组织看板（2026-10-18 人批）

> 本文记录生产操作与配置细节；每轮要执行的规则留在 `CLAUDE.md`「合并闸门」。原文：`git show 4fd3ee5:CLAUDE.md` 第 93、95 行。

## 一、仓库位置

- 仓库在组织 `daqi-dev/pilotdeck-rs`；旧 `zhengdaqi/pilotdeck-rs` 永久重定向。**不得在 zhengdaqi 下再建同名仓库**，否则重定向断（红线，CLAUDE.md 保留）。
- 旧 classic 分支保护快照：主仓 `_tmp/20261018-org-transfer/classic-protection-before.json`；现行 ruleset 导出：同目录 `ruleset.json`（均未入库）。

## 二、main 的 ruleset「main」

- 禁删除、禁强推（`deletion`、`non_fast_forward`）。
- 必需检查：`check`、`gateway-schema-baseline`；**不要求与 main 同步**（`strict_required_status_checks_policy: false`）。
- merge queue：`merge_method: REBASE`、`grouping_strategy: ALLGREEN`（build 1–5 条，超时 60 分钟）。队列在「最新 main + 前序 PR + 本 PR」的临时提交上重跑必需检查，全绿才合入，历史线性。
- PR 自身 HEAD 不变，pr-gate 结果块的 `head=` 不因排队失效。
- **pr-gate 不设为必需检查**：它读 PR 描述，`merge_group` 事件上没有 PR 上下文。
- 队列是异步的：`gh pr merge` 成功返回不代表已经合入——必需检查未齐时可能只是开了 auto-merge，齐了才入队（相关教训：memory「auto-merge-armed」）。
- merge queue 按 rebase 合入，**不**解决服务端提交暴露主邮箱的问题（memory「merge-email-exposure」）。

## 三、组织 Project「pilotdeck-rs 重构」

- 地址：https://github.com/orgs/daqi-dev/projects/1 ，自动收录 issue / PR。
- Status：Todo · In Progress · In Review · Blocked · Done；健康字段：阶段组 · 实现方 · 评审轮数 · 重派次数 · 阻塞原因（字段 ID 见 `scripts/project-fields.json`）。
- 内置工作流「Item closed」「Pull request merged」已启用，合并与关闭自动置 Done。
- `scripts/project-mark.py` 尽力而为：GitHub 失败只告警不中断；评审轮数和重派次数是读后写，偶发少计时以 PR 评审表为准。
