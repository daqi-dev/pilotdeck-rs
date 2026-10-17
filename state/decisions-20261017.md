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

# 人定 2026-10-17 · 五项待批事项暂缓，保持现状

## 人的原话（2026-10-17，会话）
- 「因为这些感觉我暂时无法判断他们对重构速度和效果的影响，如果都不阻碍继续重构的话，可以都先放着保持现状，等之后有更充分信息了再判断吗？」
- 协调者列出各项的阻塞节点后，人回复：「同意」。

## 范围
#19 deadline 差异登记（`state/decisions-20261017-deadline-diff.md`）· D4 评分口径（exp014）· #150 Draft 处置 · #144 P0 候选处置 · check_ruler 升 A4 语义（`state/decisions-20261017-167-ruler.md` 卡 2）。五项都保持现状，不执行任何选项，READY 中保留为 human。

## 各项到哪个节点会阻塞（届时再提给人，平时不追问）
| 事项 | 阻塞节点 |
|---|---|
| #19 | 关 #19 / 阶段 C 53/53 标完成 |
| D4 | A5.4 定标最终冻结；A7、A8 阶段验收标完成（不阻塞写代码） |
| #150 | 无依赖 |
| #144 | 无硬依赖；P0 候选在账上未清 |
| check_ruler 升 A4 | 无（每次重新校准多构建一个 A1 二进制） |

## 原则（一次拍板 → 一条原则）
**信息不足的人批可暂缓**：人判断不了影响的待批项，只要不阻塞就绪开发，就保持现状、保留在 READY 的 human 档；协调者不反复追问，只在该项真正成为阻塞节点（关单、阶段或里程碑标完成）时，带着当时的新信息重新提给人。

## 断言
每个 human 档的 READY 条目都标明「人定暂缓（10-17）」和对应的阻塞节点；协调者在大步骤收尾自查时核对，涉及的节点不得在未提给人之前标完成。
