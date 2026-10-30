# #1120 D-4 EdgeClaw 真实模型质量冒烟

`scripts/smoke/edgeclaw.py` 驱动 Rust 原生 `EdgeClawMemoryService` 与固定 TS
`cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，逐格执行
`capture → flush → retrieveContext`。Rust 驱动位于
`src/pilotdeck-gateway/examples/edgeclaw_smoke.rs`；TS 驱动是本目录 `run.mts`。
两侧都用原生抽取、存储和召回实现，未替换模型抽取结果，未改产品代码。

`cases.json` 是运行前登记的全合成输入，包含中文项目约定、英文长期偏好和纯闲聊。
每组每端跑 3 次，共 18 格；每格独立项目、HOME、PILOT_HOME、TMPDIR、XDG
配置/缓存目录。后续 recall 的 `recentMessages=[]`，没有把捕获对话放进召回上下文。
这是 service 层的后续 context turn，不启动完整 gateway/助手回答，也不接触 18789。

## 预算与并发前置

- 模型固定 `glm-5.3-mb`，仅发往已配置的 llm-center `/v1/messages`。
- 两侧原生 Anthropic 请求原本使用 `max_tokens=32768`；回环转发器统一裁至
  4096，设置 `temperature=0`、`thinking=disabled`，具体外发 payload 随证据保存。
- 每格最多 8 个实际 dispatch（含原生重试），全跑最多 144 个；总输出上限
  589824 tokens。请求体不超过 65536 UTF-8 bytes，响应留存上限 2 MiB。
- 单次外发墙钟 60 秒，单格 300 秒，构建 1200 秒。失败不重跑挑优。
  原生驱动非零/超时停止全跑；单格观测不完整则记 BLOCKED，不算质量零命中。
- 真实密钥只由 Python 从 `.env.local` 读取，不经命令行，不传入 Rust/TS 环境。
  native 子进程只收到每格轮换的回环令牌。外发前去掉工作树绝对路径。

现有模型调用入口未共用一个统一信号量；不能用自建 smoke 锁或 `ps` 的瞬时计数
冒充全局并发保障。**协调者先在实际使用的评审队列中预留一个单路窗口**，然后写一份
预约 JSON（本文件不会代填预约）：

```json
{
  "issue": 1120,
  "owner": "实际持有人",
  "model": "glm-5.3-mb",
  "provider_slots": 1,
  "expires_unix": 0
}
```

`expires_unix` 要填真实截止时间（最多未来 2 小时）。活动预约文件不得复制、替换、
原地修改或用于另一运行；装置对同一个 FD 加 `flock` 后读取，整个运行持锁，
每次发请求前核到期时间。内部 HTTP worker 还核父 PID、预约 inode/在用锁、单次
dispatch 收据及 payload 上限，自有 60 秒 alarm；直接调用 `--transport` 不会默认外发。
预约是协调者对实际资源窗口的承诺，文件本身无法替协调者阻止其他未参与队列的进程。

```sh
export CARGO_TARGET_DIR="$PWD/_build/cargo-target"
export CARGO_PROFILE_DEV_DEBUG=line-tables-only
export CARGO_PROFILE_TEST_DEBUG=line-tables-only
python3 -I -B scripts/smoke/edgeclaw.py \
  --baseline /Users/da/ws/pilotdeck-rs/resources/PilotDeck \
  --reservation _tmp/20261026-issue-1120/provider-reservation.json \
  --evidence _tmp/20261031-edgeclaw-smoke/run-001
```

必须从本树根目录运行。证据目录独占创建，不覆盖；重跑必须另选未存在目录。
装置在构建前调用 `cargo-slots.sh`，其 stderr 非空（例如沙箱禁止 `ps`）也拒跑。
脚本自己构建本树 example，不使用其他 worktree 的产物。

## 证据和判读

- `manifest.json`：固定模型/参数/预算、预约、Rust HEAD/工作区状态、驱动哈希；
  每个 TS 格的 `node.json` 记录精确 Node `v22.22.0` 和全源码基线守卫的 provenance。
- `dispatch-*.json` 在外发前落盘；`request-*.json` 留原始响应文本、解析结果、usage
  和失败原因（均脱敏）。`.sent` 收据防止一个 dispatch 重发。
- 各格记录 capture、flush 前后 snapshot、完整 records、用户摘要、index trace、
  recall 和进程输出；L0 SQLite/记忆文件保留在该格隔离目录。
- `accounting.json` 区分报告用量、验证用量、未知用量和已预留上限，失败路径也生成。
- `summary.json` / `comparison.md` 按样本、实现、事实给命中次数和 x/3 比率；未完成
  3 次的组显示 N/A 和已观测次数，不能把有效的 1/1 写成 100%。
- `verdict.json` 才是最终技术状态，写完统计后扫描所有产物的真实 key 和 key 前缀。
  exit 0 只表示完整采样、账本/脱敏检查通过，至多「AI 已实测，待人签字」。

抽取命中仅从 `records[].content`、`user-summary.identityBackground` 和
`user-summary.files[].content` 读取；召回命中仅从 `systemContext` 读取。输入、L0、
query、trace、preview 和 metadata 不参与计分。闲聊在 L0 留存是正常行为，写入长期
记忆才计污染；纯闲聊组任何非空长期内容都单独计数。

字面标记统计是保守观测，不能识别否定、语义改写和所有闲聊改写。AI 在真实运行后
仍需逐格复读原件，记录语义判定及与字面统计的分歧；内容观感与召回是否有用归
「仅人眼」。3 个合成样本的结果不外推成总体质量或统计显著差异。

## 离线验证和提交后负控

```sh
python3 -I -B src/pilotdeck-gateway/tests/fixtures/edgeclaw-real-provider/test_smoke.py
python3 -I -B src/pilotdeck-gateway/tests/fixtures/edgeclaw-real-provider/negative.py --check
python3 -I -B src/pilotdeck-gateway/tests/fixtures/edgeclaw-real-provider/offline_native.py \
  --baseline /Users/da/ws/pilotdeck-rs/resources/PilotDeck \
  --out _tmp/20261026-issue-1120/offline-host \
  --rust-probe "$CARGO_TARGET_DIR/debug/examples/edgeclaw_smoke"
```

`offline_native.py` 的模型在本地硬编码，仅验证探针能读到真实落盘/召回和 no-store，
不能充当真实模型质量证据。它既不生成入库 TS 金样，也不替代真实模型重复采样。

`negative.py` 有 8 个 Python 装置源码变异。`--check` 校验唯一锚点，并对每份变异文本
调用 Python `compile()`；不改源码。真正负控须协调者提交新文件后，在独占本树时运行：

```sh
python3 -I -B src/pilotdeck-gateway/tests/fixtures/edgeclaw-real-provider/negative.py \
  _tmp/20261026-issue-1120/negative-host
```

共享 `mutating_sources` 仍按 HEAD 校验，未提交的新文件会被拒绝；本单不改守卫、不建
副本仓库。进程由 `run_owned` 有界回收；所有变异逐项记录 KILLED/SURVIVED/INVALID，
恢复校验完成后原子发布汇总。这里没有 Rust 源码变异，逐变异编译用 Python `compile()`，
产品 workspace 的 no-run/fmt/clippy 仍是单独必需检查。
