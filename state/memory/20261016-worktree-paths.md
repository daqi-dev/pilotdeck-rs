---
name: worktree-path-inventory
description: 隔离工作树中定位源文件和主仓只读资源，避免按旧路径猜文件。
type: feedback
---

本轮在未核目录前猜了 crates、src/crates 与工作树 resources 路径，三次读失败；隔离树的忽略资源并不保证与主仓相同。

先用 `rg --files` 确认该树真实源码路径；跨树共享材料先用任务卡的绝对路径核存在，再只读。不能从主仓有 resources 推断每个 worktree 有链接，也不能因为读取失败而换一串猜测目录。

2026-10-16清账复发：把HTTP源码猜为http.rs、测试脚本猜为顶层tests/upgrade，均不存在。已知工作树根后先枚举该根，不从预想子目录继续猜；按rg输出的完整相对路径读。Cargo筛选须在输出核实际新增测试全名和通过数，模块名由源码声明确定，不能把旧子模块9项当作全部20项。
