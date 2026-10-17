---
name: parity-binary-path
description: gateway-parity 的 Rust 轮从「所跑脚本所在 checkout」的 src/target/debug 取二进制；A2 校准用 A1 纯透传源、A4 用 HEAD；成片缺场景先查 run.json error。
type: project
---

**为什么**：#167 六轮重采被误判为「fresh proxy 与 fixed manifest 不匹配」，实为 `run_round.py:160` 硬编码 `ROOT/src/target/debug/pilotdeck-gateway`，wt-167 里没有该文件，proxy 轮 0.87 s 即 ERROR，compare 报出 G0.round + 八场景缺失（2026-10-17）。

**怎么用**：
- **选对被测物**：A2 尺子重校准（`check_ruler.py`，proxy 轮要求十个只读 method 全透传）按 `state/decisions-20261017-167-ruler.md` 卡 1 用 **A1 纯透传源**（已登记 896b37f，`LOCAL_METHODS` 为空）构建；当前 HEAD 的 A4 本地化二进制只用于 `compare --local-method` 对拍。拿 HEAD 跑 A2 会得到真实的 G2.observer 拒绝，不是级联（exp006 readme r4 段）。
- **在跑的那份 checkout 里构建**：`run_round.py` 的 ROOT 按脚本自身 `__file__` 解析（`common.py:15`），跑哪份 `scripts/` 副本，就在那份 checkout 构建：`cd <checkout>/src && CARGO_TARGET_DIR=<checkout>/src/target cargo build -p pilotdeck-gateway`（仍满足各树独立 target 规则）。跑前核 `<checkout>/src/target/debug/pilotdeck-gateway` 存在且 sha256 与刚构建产物一致，免得静默测到旧二进制；记录写明源提交与 sha256。
- **诊断顺序**：G1.coverage「missing scenario」成片出现时，先读右侧 `run.json` 的 `status/error`（#167 首采就是缺二进制级联），再排查采集不全、证据目录残缺或装置本身——成片缺场景本身不能排除装置问题。
- run_round.py 属冻结件，不为此改它。
