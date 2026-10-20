---
name: 20261018-glm-concurrency
description: ZAI（GLM）这把 key 全局并发 8~12（2026-10-20 用户本人确认），四角色闸门同时 2 个（用 gate-pool.sh 两槽排队，不再全局串行）；超限立即 429
metadata:
  type: project
---
2026-10-18 用户转达 DSH 的实测：同一把 ZAI key、同一条 `claude -p --model glm-5.3` 路径，全局并发上限约 10 个请求（8–10 之间，看闸门角色是否正在流式输出）。超过上限不排队，直接返回 429（rate_limit_error，code 1302）。`claude -p` 遇到 429 会自动退避重试，一个极小请求能从 4–7 s 拖到 60 s，闸门角色的 1800/2400 s 超时就是这么被推爆的。并发规模与结果：1 个闸门（4 个请求）不会 429；2 个（8 个）贴着上限；3 个（12 个）开始溢出；4 个（16 个）约 6 个被拒。

**Why:** 今天下午四个闸门并行时，GLM 角色成批超时，根因是共用 key 的并发上限，不是 diff 太大（大 diff 单请求超时是另一回事）。

**How to apply:** 四角色闸门同时最多跑 2 个（用 `_tmp/20261018-a6-split/gate-serial.sh` 排队）；GLM 第二家评审也走同一把 key，闸门跑着时同时最多再开 2 个。要开 3 个以上闸门，先请人提 ZAI 的并发档位。评审继续以 DeepSeek / codex 为主。相关：[[20261018-dsh-channel]]

**2026-10-20 更新（用户本人在协调者会话确认）**：GLM 允许 8~12 并发，即**两个四角色闸门可以同时跑**。旧的 `gate-serial.sh` 实际是全局串行（有任一闸门在跑就等），没用满这个额度；改用 `_tmp/20261018-a6-split/gate-pool.sh <wt> <tmpdir> <outname>`：两槽（`/tmp/pdrs-gate-pool/slot1|2`，mkdir 原子占槽，持有进程退出即释放，死槽按 pid 自动清）。两个闸门在跑时 GLM 已占 8 个请求，此时不再另开 GLM 评审，第二家评审改用 DeepSeek / Grok。
