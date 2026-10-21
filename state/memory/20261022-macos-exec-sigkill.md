---
name: macos-exec-sigkill
description: 本机某个测试二进制 exec 即 SIGKILL（连 --list 都 137）时，是 macOS 拒绝启动该文件，不是测试失败；复制到新 inode 并 ad-hoc 重签后跑出真实结果作证据，不能记为通过也不能记为测试失败。
metadata:
  type: feedback
---

**为什么**：#407（wt-a65-pr2）三次出现同类：router_execute 两次、builtin_tools 一次，cargo 报 `signal: 9, SIGKILL`。2026-10-22 实测：
- `router_execute-… --list` 也 exit 137（未进入任何测试代码）；`codesign -v` 显示 valid；
- 原地 `codesign -f -s -` 重签后 cargo 再跑仍 SIGKILL；
- 不重签直接复制到 /tmp 仍 137；复制到 /tmp **并** ad-hoc 重签 → 正常运行，11 passed / 0 failed；
- 同名测试在别的树（wt-a6-recorder）可正常启动；CI（Linux）`check` 绿。
根因未定位（推断与内核对该 vnode 缓存的签名/启动判定有关），按「同类第三次落规则」记下处理法。

**怎么用**：
1. 全量里某个 test target 报 SIGKILL：先 `<bin> --list; echo $?`。137 且无输出 → 属本条，不是测试失败。
2. 取证：`P=$(mktemp -d); cp <bin> $P/x && codesign -f -s - $P/x && (cd <crate 目录> && PDRS_BASELINE=… $P/x)`，原始输出存进该 PR 的 `_tmp/…/`，用完删 $P。
3. 在 PR 写清「该 target 经新 inode 重签运行 N/0，其余全量 M/0」，并以 CI `check` 为准；不得把 SIGKILL 当通过，也不得为它改测试。
4. 若新 inode 重签后仍被杀，才当真实问题查（内存、子进程、信号处置）。
