# prime-agent 怎么做 TS→Rust 对拍与推进 —— 核查结果

> 2026-10-14。被查：`resources/prime-agent`（MIT，完整历史）。角色：**要学的**。
> 由子 agent 产出，主会话在切换提交 `39bc99a9` 上抽查 5 条（✔）。行号以 `39bc99a9` 时点为准（装置在 HEAD 已删）。

## 一、差分装置（parity battery）

| 项 | 做法 | 证据 |
|---|---|---|
| 位置 | `scripts/battery/run_battery.py`（6236 行 ✔）+ 根目录约 45 个 `*_parity.py` | `39bc99a9` |
| 两侧 | TS = PATH 上已装的 `prime-agent`（基准）；Rust = `target/release/prime-agent`；每侧独立沙箱、独立 TMPDIR、自己的 mock；`ts_identity.py` 防止把 Rust 误当 TS | `scripts/battery/README.md:17-18`；`run_battery.py:149-190` |
| 输入 | 24 个 flow；tmux 发按键 + daemon JSONL 协议客户端发命令 | `run_battery.py:54`；`batterylib.py:457-530` |
| **比什么** | ① 终端帧（`frame_diff`）② daemon 事件流与 wire 字节 ③ **发给模型的请求体**（`wire_request_diff`）④ **会话持久化文件**（`session_shape_diff`）⑤ 性能比值门限 | `run_battery.py:4195, 438, 620, 104-106` |
| 去噪 | `normalize()` 把 timestamp / id / sessionId / pid 换占位符；帧文本正则替换路径、uuid、版本号、≥4 位数字、耗时 | `batterylib.py:672`；`run_battery.py:4229-4250, 3339-3354` |

→ **它比的正是我们的三类契约**：协议输出（事件流）、落盘（会话文件）、发给模型的请求。

## 二、假模型：同一份脚本喂两边

- `scripts/battery/mock_provider.py` ✔：**OpenAI 兼容的 SSE 服务**，读一份 JSON 脚本；两个二进制都经 `models.json` 指向它；按 user 文本里的 marker 选**会话级队列**；识别并剔除状态栏请求（防止后台请求打乱队列）；每个请求记入 `requests.jsonl`。
- 另有进程内的两套：TS 侧 `scripts/ts_faux_extension.js`（读 `PRIME_AGENT_FAUX_SCRIPT`）、Rust 侧 `crates/pa-ai/src/providers/faux/script.rs`（读同一个环境变量）。

## 三、允许差异怎么记

- **没有集中清单。** 两种机制：`FLOW_LANES` / `FLOW_STEP_LANES` 把已知缺口标 **EXPECTED-FAIL** 并挂负责 lane（`run_battery.py:56-91`）；「sanctioned divergence」带日期的裁决**散在代码注释里**（`run_battery.py:4253-4280` 等约 20 个文件）。
- 合并门禁写在 `AGENTS.md:82-91` 与 PR 模板。
- ✔ README 开头自己说：**battery 全绿只覆盖这些脚本化流程，不代表产品一致**。

## 四、推进方式

- `39bc99a9` 只有一个父提交 ✔（squash）。早期移植在个人仓完成，历史**刻意没带过来**（PR #2523）。
- org 的 `rust` 分支 09-22 → 09-30 合并了 **509 个 PR**（gh API 取，未逐条复核）。顺序：导入 → 修复 / 补缺口 → perf 与拆大文件 → 发布与 CI。
- 切换后前 20 条提交里约 5 条是行为修复，其中 2 条明确是移植遗漏（`1ff9d86e`、`45e8f497`）。

## 五、TS 与对拍装置的去向

- TS 在切换提交里整体删除（约 1093 个文件）；基准改为 **v0.9.8 release 二进制**。
- 切换次日 `f70e8ae2` ✔（#3178，2026-09-30）删掉 battery 驱动与 45 个 parity 脚本（含 8.6 MB 已提交的运行输出），之后 `583b6031` 删掉其余。
- 现在只剩 `scripts/review_*_parity.py` 4 个；CI 下载 **sha256 固定**的 v0.9.8 二进制做对比，差异写进 receipt（`.github/workflows/ci.yml:348-380`）。

## 子 agent 的判断（针对 HTTP/WS 服务端平台）

- **直接可用**：共享脚本的 mock LLM HTTP 服务（会话级队列、记录请求）；按键名做 JSON 归一化；CI 对 sha 固定的旧版二进制做对比。
- **要改造**：tmux 帧比对 → HTTP 响应与 WS 事件流的录制比对；EXPECTED-FAIL lane。
- **只能借鉴**：PR 模板里的 parity 证据门禁。
- **不适合**：ANSI 逐字节比；把运行输出提交进仓；6000 行单一驱动脚本；squash 丢历史。
- **风险**：① 全绿 ≠ 一致，宽松正则（如 `\b[0-9,]{4,}\b`）会掩盖真实差异 ② 允许差异散在注释里，难审计；装置切换后一天就删了 ③ 并发与后台模型请求会打乱脚本队列，必须会话级路由。
