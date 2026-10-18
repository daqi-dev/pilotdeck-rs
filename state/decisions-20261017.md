# 决策 2026-10-17 · 人批 #134 登记范围 + 开发重启

## 人的原话（2026-10-17，会话）

- 「批准 #134 的登记范围」
- 背景（同会话）：CLAUDE.md 被 codex 官僚化改写导致文档循环 → 人定「开发优先」重启；授权回收 4 个 codex worktree 及旧构建产物。
- 通道指示：「llm center 是 gpt-6.1-sol 和 gpt-6-astra」「组织和干活的智能体各种组合可以更自由一点，探索最多快好省的搭配」。

## 授权内容（AI 归纳，待人复核语义无损）

native_archive_manifest / native_archive_entries / native_archive_artifact 三 method 在以下场景**Rust 安全拒绝（PATH_NOT_ALLOWED / ARCHIVE_ARTIFACT_NOT_FOUND，不转发）而 TS 基线成功跟随**，作为已接受差异登记进 `specs/gateway-parity/accepted-diffs.md`：

1. 静态根外链接（含合法外部存储布局：用户把 projects/或 transcript 根软链到别处——行为回归属同一语义家族，登记文本显式列出）
2. 检查后路径替换（TOCTOU 竞态）
3. containment 通过后的非 regular file（FIFO/device）与超限读取（评审 r2 并入：TS_IO_REQUIRED 不再回退）

**披露边界**（登记文本必须写明）：「根外拒绝」指路径解析层，不防硬链接/bind mount；注册目录枚举与 stat 元数据跟随链接（不读内容）不在本次安全保证内；artifact 与 transcript 的拒绝错误码不同（按 method 分别写）。

## 原则（一次拍板 → 一条原则）

**安全差异登记原则**：凡「TS 成功跟随根外路径 = 泄漏面」的场景，Rust 安全拒绝可登记为已接受差异；登记必须按 method 逐条、含机器判据与披露边界，禁止概括性忽略整个 method。

## 断言（能机器检查的落成负例）

`checked_path_replacements_cannot_read_outside_or_fall_back_to_ts` 同族测试：根外内容不得出现在任何本地应答中；拒绝场景上游观测点录帧里不得出现该 request 的转发。

## 执行链

#142 双家评审（GLM+Claude 无 P0/P1）→ r2 便宜修（未测量错误本地拒绝 + transcript 上限）→ rebase main → CI + 真实复算 → accepted-diffs 三条登记（两家评审）→ 合并 #142 → #46 A4 十项全本地化 → 关 #134/#46（收尾自查后）。

---

# 人定 2026-10-17 · 待批事项暂缓，保持现状

## 人的原话（2026-10-17，会话）
- 「因为这些感觉我暂时无法判断他们对重构速度和效果的影响，如果都不阻碍继续重构的话，可以都先放着保持现状，等之后有更充分信息了再判断吗？」
- 协调者列出各项阻塞节点，并提议「记录这个决定 + 把 #19 决策卡入库」，人回复：「同意」（同意的是这两项动作，不是对节点清单逐条背书）。

## 范围
暂缓四项：#19 deadline 差异登记（`state/decisions-20261017-deadline-diff.md`）· #150 Draft 处置 · #144 P0 候选处置 · check_ruler 升 A4 语义（`state/decisions-20261017-167-ruler.md` 卡 2）。都保持现状，不执行任何选项，READY 中保留为 human。

**D4 更正**：人当时看到的清单里有 D4，但核实后 D4 不是待批项——2026-10-15 已人批 D4→B（`state/decisions-20261014.md`「D4 第二步 / D6 拍板」；`docs/plan/005_phase-a-session-domain.md:13`）。剩下的是实施：A5.4 登记 A19-HOST 逐格期望（帧 / durable / registry 三面），两家评审后冻结；修复在 A7。本次暂缓不撤回、也不重开这项批准。

## 各项到哪个节点会阻塞（届时再提给人）
| 事项 | 阻塞节点 |
|---|---|
| #19 | 关 #19 / 阶段 C 53/53 标完成 |
| #150 | 未发现依赖 |
| #144 | 未发现硬依赖；P0 候选在账上未清 |
| check_ruler 升 A4 | 未发现依赖（每次重新校准多构建一个 A1 二进制） |

没有阻塞节点的三项（#150、#144、check_ruler 升 A4）：每到一个里程碑收尾自查时复查一次，有新信息就带去问人；人主动出现时也可以顺带提醒。

## 原则（一次拍板 → 一条原则）
**信息不足的人批可暂缓**：人判断不了影响的待批项，只要不阻塞就绪开发，就保持现状、保留在 READY 的 human 档；协调者不反复追问，到阻塞节点（关单、阶段或里程碑标完成）或里程碑复查时，带着当时的新信息重新提给人。

## 断言
每个 human 档的 READY 条目都标明「人定暂缓（10-17）」和对应的阻塞节点；协调者在大步骤收尾自查时核对，涉及的节点不得在未提给人之前标完成。

## 卡 · Claude 作者的 PR 怎么过第 2/3 步（#185，自决 · 待复核，AI 代批）
- **卡在哪**：`run-review-roles.py` 只有 `claude -p` 执行器，而 #185 作者是 Claude Code；合并闸门要求 simplify 与 pr-ready 由非作者模型做。
- **谁受影响**：所有 Claude 作者的 PR；不动尺子与意向。**能否撤回**：能。
- **选项**：A 照跑 run-review-roles 只为 pr-gate 结果块，不计独立；三角色+四视角并进 codex 与 GLM 两家的评审任务书；B 用 waiver 跳过（不对，有代码）；C 先给 run-review-roles 加非 Claude 执行器再审（拖队列）。
- **建议 / 执行**：A。PR 描述注明结果块是同谱系、不计独立评审。
- **原则**：同谱系跑出的审计只算「做了」，不算「独立审过」；独立性由两家非作者评审承担，任务书须覆盖被替代的角色。
- **断言**：Claude 作者 PR 的两家非作者评审任务书里必须出现 sibling/concurrency/boundary 与四视角字样（任务书：`_tmp/20261017-issue-185/review.md`）。

## 人定：编排提速四项（2026-10-17 晚，用户在 pilotdeck-rs-69 会话：「1、2、3、4 都做吧」）
- **卡在哪**：今晚 4 小时只合并 1 个产品 PR；用户感觉慢。实测主因是工具单占约一半时间（#198 已堵），其次是评审轮数，并发少排第三。
- **四项**：① #198 只做产品代码（已合并）；② 协调者从 #127/#128/D0–D3 拆出互相独立的产品单进 READY，并行派活；③ 资源上限放宽：worktree 5→8、`_build+_tmp` 46→80 GiB（对拍串行不变）；④ `run-review-roles.py` 默认 GLM：四角色各起一个 GLM 并发审，GLM 经 Claude Code 外壳接智谱 Anthropic 兼容接口（#200 r2 起；r1 的 opencode+sandbox 方案因评审连续 P0 弃用）。
- **原则**：并行只对**互相独立的产品单**有意义；加并发前先保证 READY 里有可并行的单。资源上限按机器实测放宽，对拍串行这条因假差异实证保留。
- **断言**：CLAUDE.md「资源上限」写 8 / 80 GiB；`scripts/test-review-roles.py`（CI 跑）覆盖：zai/anthropic 两通道 PASS 过闸；密钥只走环境变量不进命令行不落盘；越权工具→TOOLS_VIOLATION、init 模型不符→ERROR、缺 VERDICT→PARTIAL、超时→TIMEOUT 均被闸门拒收；readonly 非 tools 拒收。
## 卡 · codex 额度用尽后的实现与评审通道（2026-10-18，自决 · 待复核，AI 代批）
- **卡在哪**：codex CLI 报「usage limit，Oct 22nd 2:43 PM 后再试」（_tmp/20261018-a68-*/codex.err），A6.8 两条 worker 中途停止；codex 同时是 GLM/DeepSeek 之外的第三家评审。
- **谁受影响**：所有 impl 单与以 codex 为评审一家的单，至 10-22。**能否撤回**：能（额度恢复即回切）。
- **选项**：A 实现改 `claude -p --model claude-opus-5-5`（worktree 内、dontAsk + 显式工具白名单），评审由 GLM + DeepSeek 两家非作者承担；B 实现改 opencode GLM build agent（GLM 就不能再当评审，只剩 DeepSeek 一家）；C 停 impl 到 10-22。
- **建议 / 执行**：A。Claude 作为作者时不审自己；评审 = GLM（opencode plan）+ DeepSeek（review-deepseek.py，默认关思考）；四角色 run-review-roles --runner zai 照常。
- **原则**：实现通道换人时，同步重排评审，保证两家非作者；通道不可用不降级为单评审。
- **回切（10-18）**：10-18 用户告知 codex 额度恢复，已回切。守护会话 pilotdeck-rs-69 实测 `codex exec --sandbox read-only` 返回 CODEX-OK。新开的 impl 单改回由 codex 实现；已用 claude 开工的 A6.8-pr3、D1-pr2、A6.2-pr3 跑完当前这轮，不中途换手。评审照旧两家非作者：codex 实现的单由 Claude 与 GLM/DeepSeek 中任两家评审。

## 卡 · 模型分工（2026-10-18，人定，经守护会话 pilotdeck-rs-69 转达）
- **评审**：默认 GLM + DeepSeek。GLM 走 `run-review-roles.py --runner zai`，或 `claude -p` 接智谱；DeepSeek 走 `review-deepseek.py`，默认关思考。只有难单（并发、协议、冻结装置、安全边界）才请 Claude 当评审。
- **实现**：codex、GLM、DeepSeek 轮换。
  - GLM：`claude -p` 接智谱，给写权限，在 worktree 里跑。
  - DeepSeek：没有工具通道，只出候选 patch，由持有人机械应用，适合小而明确的单。
  - 难单仍优先 codex 或 Claude。
- **约束不变**：谁写谁不审，必须两家非作者。
  - GLM 实现时：评审 = DeepSeek + codex/Claude；
  - DeepSeek 实现时：评审 = GLM + 另一家；
  - codex 实现时：评审 = GLM + DeepSeek（难单可换 Claude）。
- **试点**：GLM 和 DeepSeek 都还没真写过产品代码，先各拿一张小而明确的单试一次，看质量再决定要不要常态化。试点结论记回本卡。
- **原则**：通道分工按难度和作者排除来定，每单开工时在 triage 或任务书里写明「作者 / 评审两家」。
