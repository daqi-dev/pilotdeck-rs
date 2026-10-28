# live-e2e 装置（#969，由 #748 拆出）

端到端装置自测：真实 TS sidecar 客户端（`agentLoopSidecarClient`，经
`createStdioAgentLoopSidecarConnectionFactory`）→ Python stdio 中间人 → 真实
sidecar CLI（`pilotdeck-agent-loop-sidecar.ts`，经 tsx）→ 本地 mock provider
（HTTP）+ 沙箱内 `read` 工具。不依赖 #743/#745；不碰 Rust 产品代码；不监听
18789；每格 HOME 指向各自沙箱，不写 `~/.pilotdeck`。

## 组成

| 文件 | 角色 |
|---|---|
| `stdio_mitm.py` | stdio 中间人：记录双向 NDJSON 帧（`frames.jsonl`），按声明式注入计划改写；`mitm-summary.json` 记录每个注入是否命中（未命中非零退出） |
| `mock_provider.mjs` | 本地 HTTP mock provider：文本流 / `read` 工具调用 / 流中途挂起（连接保持，客户端 abort 时记 `connection:"aborted"`） |
| `driver.mts` | TS 探针：从只读基线导入真实客户端，model 端口走 mock，工具端口执行沙箱 `read`；证据写 `driver-result.json` |
| `selftest.py` | 编排：起 mock（进程组 + 有界回收）→ 跑窗口×注入矩阵 → 逐格断言（每条绑定 `#969 …` 专属消息）→ `report.json` |
| `negative_controls.py` | 变异负控：关掉一种注入 / 伪造一种可观测 → 对应断言必须以绑定消息变红；含 baseline 正控 |

注入计划（`plan.json`）：`{"injections":[{"action":"drop|duplicate|early_final",
"direction":"s2h","kind":"event","final":false,"nth":N}]}`，nth 为第 N 个匹配帧
（1 基），确定性可复现。矩阵统一用：drop=第 2 个非 final event、duplicate=第 1
个非 final event、early-final=第 2 个非 final event。

## 运行

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck   # Node v22.22.0，只读
python3 selftest.py <新目录>            # 全矩阵 12 格
python3 selftest.py <新目录> --cases text-drop   # 单格
python3 negative_controls.py <新目录>   # 变异负控（需源码已提交，见脚本头注）
```

## TS 期望值表（Node v22.22.0 实测；#748b Rust 侧对拍基准）

窗口：W1 文本 / W2 工具 / W3 取消（首个 `model_event` 后 abort）。原始输出出处
列指向本目录跑出的证据文件名（`matrix-run/<格>/driver-result.json`、
`frames.jsonl`、`mitm-summary.json`、`mock.log`）。

| 窗口×注入 | TS 客户端事件序列 | terminal | run() 错误 | 原始输出出处 |
|---|---|---|---|---|
| W1×I1 passthrough | `model_request_started`, `model_event`×5(request_started/message_start/text_delta×2/message_end), `assistant_message`, `stop_requested`, `turn_completed` | `success`/`completed`，turns=1，messageCount=2 | 无 | `text-passthrough/driver-result.json` |
| W1×I2 drop | `model_request_started` 后即断 | 无 | `Sidecar event sequence mismatch: expected 1, got 2.` | `text-drop/{driver-result.json,frames.jsonl,mitm-summary.json}` |
| W1×I3 duplicate | `model_request_started` 后即断 | 无 | `Sidecar event sequence mismatch: expected 1, got 0.` | `text-duplicate/{driver-result.json,frames.jsonl,mitm-summary.json}` |
| W1×I4 early-final | `model_request_started`, `turn_completed`（合成） | `success`/`mitm_premature_final`，turns=1，messageCount=0 | 无 | `text-early-final/{driver-result.json,frames.jsonl,mitm-summary.json}` |
| W2×I1 passthrough | 20 事件：首轮 `model_event`×6(tool_call 全套)+`assistant_message`+`tool_calls_detected`+`tool_result`+`tool_results_projected`+`turn_continued`，次轮 `model_event`×4+`assistant_message`+`stop_requested`+`turn_completed` | `success`/`completed`，turns=2，messageCount=4；read 实际执行返回沙箱文件内容 | 无 | `tool-passthrough/{driver-result.json,mock.log}` |
| W2×I2 drop | 同 W1×I2 | 无 | 同 W1×I2 消息 | `tool-drop/…` |
| W2×I3 duplicate | 同 W1×I3 | 无 | 同 W1×I3 消息 | `tool-duplicate/…` |
| W2×I4 early-final | 同 W1×I4（工具回路未完成，turns=1，messageCount=0） | `success`/`mitm_premature_final` | 无 | `tool-early-final/…` |
| W3×I1 passthrough | `model_request_started`, `model_event`(request_started), `turn_completed` | `aborted`/`aborted_streaming`，turns=1；h2s 发出 `cancel`；model 端口连接被中断（`modelInterrupted`），mock 记 `connection:"aborted"` | 无 | `cancel-passthrough/{driver-result.json,frames.jsonl,mock.log}` |
| W3×I2 drop | 错误先于 abort 生效（被丢的正是 abort 等待的首个 `model_event`） | 无 | `expected 1, got 2.` | `cancel-drop/…` |
| W3×I3 duplicate | 同 W1×I3（abort 未及触发） | 无 | `expected 1, got 0.` | `cancel-duplicate/…` |
| W3×I4 early-final | 合成 final 先于 abort 生效（abort 等待的 `model_event` 被改写为终态） | `success`/`mitm_premature_final` | 无 | `cancel-early-final/…` |

MITM 侧证据（每种注入、每个窗口都成立）：drop → `frames.jsonl` 有
`action:["drop"], delivered:false` 记录；duplicate → 对应帧记录
`action:["duplicate"]` 且客户端报 `got 0`（同帧收到两次）；early-final →
`action:["early_final"]` 且帧被改写为 `final:true, outcome:"completed"`、payload
带 `stopReason:"mitm_premature_final"`；passthrough → 全部帧 `action:[]`、
序列号连续无重。`mitm-summary.json` 的 `fired:true` 是注入命中的直接证据。

## #748b Rust 侧的对接点

- 序列错位的两个错误消息是 Rust 侧必须复现的 TS 可观测行为（缺号
  `expected N, got N+1`；重号 `expected N+1, got N`）。
- early-final 合成 terminal 的接受路径：`outcome:"completed"` + 合法
  AgentTurnResult（`result.type:"success"`）即被接受，客户端不再消费后续帧
  （真实 final 不再可见）。
- 取消窗口：客户端 abort → h2s `cancel` 请求 → 终态 `aborted`/
  `aborted_streaming`，model 端口连接中断对 mock 可观测（`connection:"aborted"`）。
