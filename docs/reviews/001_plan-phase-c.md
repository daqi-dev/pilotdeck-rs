# 评审 001 · 计划 001（阶段 C）、架构 001、specs/parity —— 第 1 轮

> 2026-10-14。A = codex-cli 0.160（GPT 系）· B = opencode 2.0.25 + glm-5.3（GLM 系）。两路都判**无 P0**。原始输出在 `_tmp/20261014-plan001-review/`（不入库）。
> Issue #1 里对尺子的审查另记 `001_ruler-recheck.md`（同号）。

| # | 级 | 问题 | 谁提 | 主会话核实 | 处理 |
|---|---|---|---|---|---|
| 1 | P1 | **配对错位**：旧驱动只比「native 对 X」，照跑量到的是 native 对 Rust | **A + B 撞车** | ✔ 旧 `formal-round.py:31-32`、`verify_a9.py:428-429`、`formal-bind.mjs:56-57` | S1 加「sidecar 对 sidecar」：各跑一遍、各取 sidecar 侧 trace；summary 断言两侧身份 |
| 2 | P1 | **sidecar 入口会拒绝 gateway 传的路径参数**，「零改动接入」不成立 | A | ✔ 基线 `AgentLoopDeploymentProfile.ts:73`；旧入口 `pilotdeck-agent-loop-sidecar.rs:22` | S4 加入口适配（单独提交）；arch 更正；接入方式列为待人拍板 |
| 3 | P1 | Issue #1 完成条件与降级路径矛盾；审查轮次无上限 | **A + B 撞车** | 成立 | 降级改为停下交人；审查最多两轮；完成条件改成 `check_ruler.py` 输出的 JSON |
| 4 | P1 | S 集合可被做小（k 无上限、环境失败算不稳定、无下限、无人批） | **A + B 撞车** | 成立 | k = 5 定死、不许重跑挽救、BLOCKED 整轮作废、\|S\| < 45 停下交人、排除逐个人批；acceptance.md 先人批再冻结 |
| 5 | P1 | E 规则失败只旁报、场景靠 glob 取，四项对照漏掉「缺场景 / 截断」 | A | ✔ 旧 `verify_a9.py:423, 435` | 总判决强制 53 场景齐全 + 身份 + 全部适用 E 规则；加端到端负向对照 |
| 6 | P1 | 冻结只校验 README；比较器代码没封；「追加不算改动」成了豁免 | A | 成立 | FROZEN 覆盖规约 + `scripts/parity/` 全部代码；AI 追加的差异条目一律「待批」 |
| 7 | P2 | 照搬会构建失败：workspace 继承项（`exclude`、`license-file`、lints）与 crate 自有依赖漏列 | **A + B 撞车** | ✔ 旧 `rust/Cargo.toml:3, 10` | S4 写明一起搬，依赖以 crate manifest 为准，改动列入适配例外 |
| 8 | P2 | 计划里的验收命令跑不通（`--expected-sha256` 必填、round 名受限、Node 路径写死） | B | ✔ 旧 `formal-round.py:14, 27, 34-37` | 命令示例重写；三处一并参数化 |
| 9 | P2 | 「TS 自比」术语与实跑配对不符，阈值三处口径不统一 | B | 成立 | 规约里先定义术语；m 规则统一为「S 内 k 遍全一致」 |

**收敛**：P1 六条、P2 三条全部处理。新增待人拍板 2 项（接入方式、阈值程序与分母），见计划文末。
