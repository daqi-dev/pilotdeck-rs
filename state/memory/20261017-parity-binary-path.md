---
name: parity-binary-path
description: gateway-parity 的 Rust 轮从固定路径 src/target/debug 取二进制；worktree 未在该路径构建时 G0/G1/G3 大面积失败是级联，不是装置失配。
type: project
---

**为什么**：#167 六轮重采被误判为「fresh proxy 与 fixed manifest 不匹配」，实为 `run_round.py:160` 硬编码 `ROOT/src/target/debug/pilotdeck-gateway`，wt-167 里没有该文件，proxy 轮 0.87 s 即 ERROR，compare 报出 G0.round + 八场景缺失（2026-10-17）。

**怎么用**：跑 gateway-parity 的 rust/proxy 轮前，在该 worktree 内 `CARGO_TARGET_DIR=<wt>/src/target cargo build -p pilotdeck-gateway`（仍满足各树独立 target 规则），并先核二进制存在。看到 G1.coverage「missing scenario」成片出现时，先读右侧 `run.json` 的 `status/error`，再怀疑装置。run_round.py 属冻结件，不为此改它。
