# 2026-11-05 · 闸门通道名额按实测调整（zai 6 / minimax 16 / deepseek 16 / anthropic 2）

> 标：**AI 代批，待人复核**。实测依据：`_tmp/20261105-channel-ramp/RAMP.md`（原始日志同目录 `raw/`，
> 2026-11-05 台阶压测）。本条只改协作工具的排队名额，不碰产品代码、尺子与意向。

## 卡

- **卡在哪**：`fleet` P4（闸门评审）与 `run-review-roles.py` 的每通道并发名额原先写死
  「zai 8 / minimax 4 / deepseek 4 / anthropic 2」，其中 deepseek/minimax 的 4 从来不是实测值，
  而 zai 的 8 实际超出单 key 配额——名额与实际容量脱节，评审要么被无谓排队，要么在 zai 上撞 429 白烧配额。
- **谁受影响**：评审吞吐（所有闸门块的四角色并行度）；GLM 实现 worker（`glm_impl.py` 与闸门共用同一把
  `ZAI_API_KEY`，zai 名额直接决定留给 worker 的请求预算）。
- **能否撤回**：能，一行改动（`P4_CAP` 与 `RUNNER_SLOTS` 两张表），无数据迁移、无外部状态。
- **动不动尺子/意向**：不动。判分语义、冻结程序、评审门槛、双谱系要求全部不变；只是同一批角色排队方式变了。

## 实测依据（详见 RAMP.md 各节）

- **zai**：并发不是瓶颈——干净窗口 24 并发 24/24 成功；真正的顶是**单 key 50 请求/分钟**
  （超出即 429 `code 102503`），且**被 429 拒掉的请求同样计入配额**。
  需求侧实测：一个 4 角色 GLM 门块聚合 **32–43 RPM**（p50/p90/max），8 个角色需 **64–86 RPM**，已超配额 30–70%。
  叠加 GLM 实现 worker 共用同一把 key，8 这个数字只会让两边互抢。
- **deepseek**：96 并发 **96/96 全成功**、p95 0.47 s，全程无 429 → 4 是自设过低值，有约 6 倍余量。
- **minimax**：96 并发 **96/96 全成功**、p95 1.79 s，全程无 429；原值本就是占位（CAPACITY.md 明写「未实测」）。
- **anthropic**：本轮**未压测**（不在范围），现网常见 4–5 角色在跑且能出块，保持 2。

## 选项与后果

| 选项 | 后果 |
|---|---|
| A. 维持 zai 8 / ds 4 / mmx 4 | zai 持续撞 50 RPM，429 与自动退避把评审时间浪费在重试上；ds/mmx 明明有 6 倍余量却排长队 |
| B. 按实测改为 **zai 6 / mmx 16 / ds 16 / anthropic 2** | zai 降到接近配额（仍会在 GLM worker 活跃时贴顶，但有排队而不是 429）；ds/mmx 不再自设瓶颈，评审吞吐随之上去 |
| C. zai 升到 16 | 明确错误：速率上限与并发无关，只会让 429 更多（被拒请求还继续占配额），并挤掉 GLM 实现 worker |

## 建议

取 **B**：`zai 6 · minimax 16 · deepseek 16 · anthropic 2`。zai 是「贴着配额但不放弃」，不是容量提升；
真正的吞吐增量来自 deepseek/minimax 放开。zai 若要再提，须先给 GLM 实现另配一把 key，或把角色请求数压下来。

## 落点（同一提交内容跨两仓）

- `pilotdeck-rs/scripts/run-review-roles.py` 的 `RUNNER_SLOTS` 与模块 docstring；
  `scripts/test-review-roles.py` 新增名额钉死断言（改值必须同步 ops，防漂回旧值）。
- `pdrs-ops/scripts/fleet/fleet.py` 的 `P4_CAP`（fleet 显示与闸门排队同源）。`lib/fleet-gate.py` 只用 P1，无 P4 落点。
- 复核入口：`run-review-roles.py` 模块说明「通道并发闸」段；`fleet.py` 的「P4_CAP」段。

## 验证（AI 已实测，待人签字）

- 自测：`python3 -I scripts/test-review-roles.py` → `test-review-roles: 全部通过`（含新增名额断言与 anthopic 2 路排队用例）。
- 负控：`_tmp/20261105-gate-caps/negctl.py` 用真 `acquire_slot` 起 4 个并发子进程——
  `cap=1` 时 `peak=1` 且打印 `[review-slot] waiting 1/1`；`cap=6` 时 `peak=4` 无等待。
- fleet 显示：`P4 zai 2/6 有余量 · minimax 2/16 有余量 · deepseek 0/16 有余量 · anthropic 2/2 贴顶`。

## 原则（一次拍板 → 一条原则）

**名额必须由实测容量决定，且要按「被限的度量」设闸**：限并发的通道按并发设，
限速率的通道按请求速率设，不能拿并发数假装速率配额。

## 登记者

协调者会话（DeepSeek v4.1 flash-mb 通道）代批，待人复核；数据来自 RAMP.md 原始日志，未重测。
