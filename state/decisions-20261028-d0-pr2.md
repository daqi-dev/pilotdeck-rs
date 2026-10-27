# 决策卡：D0-pr2 cli-parity 放开单端口 loopback + mock gateway（2026-10-28，AI 代批，待人复核）

- **卡在哪**：cli-parity 装置 `scripts/cli-parity/policy.py:43` 对 TS / Rust 两侧一律 `(deny network*)`。D1 的 SDK 四命令（sessions / resume / settings 等建连后路径）与 D2 cron 必须连网关才有可观测输出，现只能测「建连前退出」的场景，D1 / D2 的主体无尺子。
- **谁受影响**：D 阶段（CLI）D1 / D2 全部建连后场景；#245（exp003 准入元组）顺带。
- **能否撤回**：能。只给带 `mockGateway` 标记的新场景放开；既有 29 例 profile 不变，回退即删标记。
- **动不动尺子 / 意向**：动尺子（非冻结：cli-parity 无 FROZEN 登记，10-27 盘点核过）；不动意向。红线「不碰用户运行中的 PilotDeck」必须保持：默认探测端口 18789 仍须内核级拒绝。

| 选项 | 做法 | 后果 |
|---|---|---|
| **A（建议）** | 带 `mockGateway` 的场景在 sandbox profile 里只追加 `network-outbound` 到 `127.0.0.1:28999`（mock 由装置在沙箱外起，单次轮内独占）；其余仍 `deny network*`。新增负控：同场景连 18789 / 任意其他端口 / Unix socket 必须 EPERM，且 mock 未起时场景必须失败而非跳过 | D1 / D2 主体可对拍；隔离从「全拒」变「全拒 + 一个装置自有端口」，红线有机检 |
| B | 不放开，D1 / D2 只靠 Rust 集成测试 + mock | 尺子不动，但 CLI 建连后行为无 TS 对拍，P1 验收证据缺失 |
| C | Linux netns 只在 CI 跑 | macOS 本机无法复现，证据不可复现（P1） |

- **建议**：A。分级「自决 · 待复核」：可撤回、意向内、判断题（尺子口径放宽到一个端口）。
- **预注册判据**：① 既有 29 例两侧 profile 字节不变（哈希断言）；② 新负控 18789 / 他端口 / Unix socket 三项 EPERM；③ mock 端口被占用时整轮拒跑（不回落）；④ 新场景期望值只由真实 TS 两轮定标，未定标恒 `PENDING-TS-CALIBRATION`。
- **可复用原则**：放宽隔离只能按「场景标记 × 装置自有资源」最小放开，并为被保护对象（用户 PilotDeck 端口）配一条必须拒绝的负控。
- **执行**：开单派 GLM（Codex 沙箱内嵌套 sandbox-exec 不可用）；评审 opus + Codex。
