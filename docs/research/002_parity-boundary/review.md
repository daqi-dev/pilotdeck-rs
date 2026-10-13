# 对抗评审记录 · decision.md 与 gap.md（第 1 轮）

> 2026-10-14。两条谱系各自独立评审，只读，允许回源码核对。原始输出在 `_tmp/20261014-research002-review/`（不入库）。
> A = codex-cli 0.160（GPT 系）· B = opencode 2.0.25 + glm-5.3（GLM 系）。结论：**两路都判无 P0**。

| # | 级 | 问题 | 谁提 | 主会话核实 | 处理 |
|---|---|---|---|---|---|
| 1 | P1 | 「最强依据」跳步：sdk 测试连真端口，但 gateway 进程内构造、注入假模型，不是两进程黑盒 | **A + B 撞车** | ✔ `tests/sdk/session-transcript-restore-e2e.spec.ts:96-105` | 依据降级为「WS 客户端形态现成」，成色归 exp001 |
| 2 | P1 | 漏拍板：supervisor 与桌面端写死起 TS gateway，Rust 占 18789 会撞端口 | **A + B 撞车** | ✔ `webRuntimeSupervisor.js:50-57`、`main.ts:251-258` | 拍板表加第 6 行 |
| 3 | P1 | 「共享一个 mock」会让两边抢同一个回复队列；prime-agent 实际每侧一个实例 | A | ✔ prime-agent@39bc99a9 `run_battery.py:158` | 改为「同一份脚本、每侧一个实例」 |
| 4 | P1 | 「统一出口」不完整：google 流式走 `googleClientFactory`，不经 `options.fetch` | B | ✔ `streamModel.ts:172-181, 358` | 改措辞；mock 先覆盖 openai / anthropic 协议 |
| 5 | P1 | 可复现的验收语义没定；一律抹 ID 会漏掉引用关系错误 | A | 推理成立 | 拍板表加第 7 行（ID 一致映射、seq 保留） |
| 6 | P1 | 命门 1 无验证步骤：真客户端是带重连 / 重放的 `createRemoteGateway`，不是 `GatewayWsClient` | B | ✔ `pilotdeck-bridge.js:60` | 加 exp003：真 `ui/server` 连透明代理 |
| 7 | P2 | gap.md 写「B 覆盖不到：无」，与桌面主进程直连矛盾 | B | ✔ `main.ts:941` | 已改 |
| 8 | P2 | 复用 AGPL 的 `GatewayWsClient` 有传染性 | B | **不成立**：本仓已是 AGPL-3.0 | 在拍板表第 3 行注明 |
| 9 | P3 | 帧 `seq` 每请求从 0 自增，本身确定，别当噪声抹掉 | B | ✔ `GatewayWsConnection.ts:129` | 写进拍板表第 7 行建议 |

**收敛**：P1 六条全部处理，P2 一条处理、一条判不成立，P3 采纳。按 `dev-launch` 的口径（无 P0/P1/P2）本轮已收敛；探针跑完、取舍卡再改时，再评一轮。
