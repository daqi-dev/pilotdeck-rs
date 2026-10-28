# 决策卡：#745 recreate 双写入者——不对齐 TS，加写入者隔离（2026-10-28 23:1x）

**分级：自决（依据人定规则）+ 设计细节自决·待复核（AI 代批，待人复核）**

## 卡在哪
#745（A7.2-pr6a-5，关键路径）r1 DeepSeek P0：会话 recreate 时旧 turn 仍在写，新 handle 同时写同一 transcript。fix-r1 用真实 TS / Node v22.22.0 两种交错实测（`_build/wt-745/_tmp/20261026-issue-745/fix-r1/writer-{after-publish,before-read}/raw/`）：**TS 本身同样损坏**——旧 writer 写 seq 1–7、新 writer 写 6–11，之后 resume 报 `Session event sequence must increase strictly: received 6 after 6.`，该会话历史再也打不开。

## 依据
`.42cog/intent.md`「什么算 bug（2026-10-14 人定）」：基线丢数据 = bug，默认修，修了记进已接受差异。本格属第 ① 条（丢数据），故**不对齐 TS 的损坏行为**；这一点不是 AI 判断题。

## 用户可感知差异
| 场景 | TS 表现 | Rust（本卡后） | 用户感知 |
|---|---|---|---|
| turn 进行中触发会话重建（reload / 换配置），旧 turn 很快结束 | 新旧同写，序号重复；下次打开会话报错、历史丢失 | 新 handle 等旧 writer 释放后再加载，序号连续 | 重建稍慢（等旧 turn 收尾）；历史完好 |
| 同上，但旧 turn 迟迟不结束 | 同上（损坏） | 有界等待后重建失败，给明确错误码；文件序号不受损，稍后可正常 resume | 看到「会话忙，稍后重试」类错误，而不是静默损坏 |
| 无在途 turn 时重建 | 正常 | 正常（不变） | 无差异 |

## 能否撤回
可撤回（只影响 recreate 路径的时序与错误；回退即删 fence）。不动尺子、不动意向。

## 设计细节（自决·待复核）
1. **独占写入者租约**：同一 transcript 路径同时只允许一个 writer；新 storage 在读取（load）前取得租约。
2. **先中止再等待**：recreate 触发旧 turn 的既有中止 / dispose 路径，再等旧 writer 释放；**有界等待**，复用仓内既有 dispose / shutdown 截止常量，不新造魔数（若无可复用者，须写依据）。
3. **超时**：recreate 失败、新 handle 不发布、返回专用错误码；不回收他人租约、不强写。
4. **差异登记**：写进对应 accepted-diffs（类别「基线 bug：丢数据」，依据本卡，批准状态「依人定规则修复，待人复核」）。

## 原则（可复用）
**TS 基线自身丢数据时不做 bug 兼容；Rust 修复并登记差异，用户感知写成对比表。** 机检：本格两个交错用例断言 Rust 的序号严格递增且 resume 成功，TS 金样保留原始损坏观测作对照。
