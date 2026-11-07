---
id: D4-TUI-TOOL-VIEW-001
---

# 工具输出查看边界与长行

## 类别

工具输出读取的资源保护与可完整浏览的长行显示；候选差异，尚无冻结验收豁免。

## 基线依据

固定 TS `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的 `TuiApp.tsx` 优先读 resultPath，失败回退 fullText ?? text，短于50个物理行时写终端滚动记录，否则进入 viewer。`ToolOutputViewer.tsx` 对超过终端宽度的物理行截断显示。真实 TS PTY 原件在主仓 `_tmp/20261108-cli-tui/tools-ts-r1/`，覆盖焦点展开、短输出、viewer 首尾和退出；该轮空闲 Ctrl-C 后一秒仍存活，随后只清理自有进程，未判正常退出。未实测 TS 大文件/FIFO 边界，不能把 Rust 限额写成 TS 事实。

## Rust 行为

单次工具输出最多1MiB，独立线程以O_NONBLOCK打开并检查同fd普通文件；超限明确报错。文件不可读或不是普通文件则使用有界 fullText（含空字符串）或预览。每次只允许一个待决读取，会话选择受阻，返回结果另核原session与turn generation，过期结果丢弃。

短于50行、UTF-8不超过64KiB、排版不超过256行及200,000单元格的输出写入滚动记录，其余进入viewer。viewer按Unicode字素宽度连续硬换行，用上下/j/k、PageUp/Down、g/G浏览，用q/Esc关闭。显示缓存分批推进并计入既有约8MiB状态预算，过大明确拒绝；不承诺进程RSS上限或故障文件系统内核IO期限。

## 范围

仅远端TUI工具焦点、展开与输出查看。空闲Esc清除工具焦点，输入新文字也清除焦点。保持原会话/取消互锁；不代表native权限、history、本地fallback或完整D4完成。不修改既有冻结文件或基线原件，不以本条自行豁免差异。

## 机器判据

`src/pilotdeck-cli/tests/fixtures/tui-pty/tools-smoke.py` 用当前Rust bin和隔离动态loopback fake Gateway验证焦点、展开、Esc、短输出、viewer首尾/长行/resize、真实超限普通文件、无writer FIFO回退、退出、termios恢复与socket关闭。lib测试验证空fullText、有界回退、身份失效及连续长行排版。每轮原始输出、源码和bin指纹保留主仓 `_tmp/20261108-cli-tui/`；实际通过轮次见同目录证据与board，测试代码存在不等于通过。TS采集器独立保留，不用Rust结果回写基线。

## 批准记录

2026-11-08，按用户允许功能完整而不要求逐像素复刻的方向实现；资源限额与长行选择属AI代批，待人复核。未批准为冻结判分差异，真人视觉体验待验。
