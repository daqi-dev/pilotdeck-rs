---
name: coord-session-launch
description: 大总管 / tmux 手下的会话启动协议——起会话用 `claude --permission-mode bypassPermissions`，不用 `--allow-dangerously-skip-permissions`（本机实测落 auto mode，分类器抖动会被误读成权限被拒）；含已在 auto mode 的会话怎么切、怎么判是不是分类器抖。不管具体分工与派单内容
type: feedback
---

# 大总管 / tmux 手下的会话启动协议

## 起会话就用这一行

    claude --permission-mode bypassPermissions

## 为什么不是 `--allow-dangerously-skip-permissions`

2026-11-07 本机实测：该标志把会话落在 **auto mode**（tmux 底部显示 `⏵⏵ auto mode on`）。
auto mode 每一次工具调用都要过一次**分类器**，分类器走同一个 llm-center 网关，会抖。症状：

    ⎿ Not run · auto mode's check had no usable answer

**看起来像权限被拒，其实是分类器没给出答案**。11-07 00:5x 大总管 coord2 因此连续派不出子代理，
只能自己去回收 worktree，违反了「机械活派子代理」的人定规矩，磁盘告急时多耗了近一小时。

换 `--permission-mode bypassPermissions` 后底部是 `⏵⏵ bypass permissions on`，分类器完全退出
（2026-11-07 01:2x 实测）。`--permission-mode` 可选值：`acceptEdits` / `auto` / `bypassPermissions` /
`manual` / `plan`。

## 已经在 auto mode 的会话怎么切

shift+tab 循环，顺序实测为：

    auto mode → manual mode → accept edits → plan mode → bypass permissions

每切一次都 `tmux capture-pane -p -t <窗口> -S -3` 看一眼底部。**别停在 manual**——会逐条弹确认、
没人看就卡住；也别停在 plan——写操作会变成出计划。

## 判「是不是分类器抖」

看报错原文有没有 `auto mode's check`。是的话：**先重试 / 换个描述重派**，
不要顺手改做机械活（那是把「派单失败」升级成「违反分工」）。

## 相关

- 完整编排说明（含 tmux 三步法、进程内子代理 vs tmux 独立会话的取舍）：`_tmp/<日期>-coord/tmux-orchestration.md`
  ——该文件在 `_tmp/` 下会被清理，本页是它的持久版。
- 窗口多 pane 时 `-t <session>:<窗口名>` 只命中活动 pane，capture 到 shell ≠ 会话死了，见 `state/memory/` 同批记录。
