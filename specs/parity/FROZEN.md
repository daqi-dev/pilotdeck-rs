# 冻结记录

| 文件 | sha256（前 16 位） | 冻结时间 | 说明 |
|---|---|---|---|
| `README.md` | `f8958a3280a95df7…` | 2026-10-14（人过目） | 比什么 · 四项对照 · 阈值程序（k = 5、S 规则、\|S\| ≥ 45、分母）· 差异分诊 |
| `accepted-diffs.md` | `653ccb1001d1b09a…` | 2026-10-14（人过目） | 表头；此后只增不删，AI 追加的条目状态为「待批」，追加不改表头 |
| `acceptance.md` | `8b92036e134de36d…` | 2026-10-14 #35 重登记（**AI 代批，待人复核**；人批方案 B） | k = 5、S = 53（无排除）、Rust 配对规则、批准记录 |
| `normalization.md` | `5a0c10b70a89a42a…` | 2026-10-14（**AI 代批，待人复核**） | 两侧同路径硬前提、路径对齐、ID / 时间、seq、请求体 |
| `scripts/parity/bind.mjs` | `1b4a38037cf526c6…` | 2026-10-14（**AI 代批，待人复核**） | 比较器 / 驱动 / 对照 |
| `scripts/parity/check_frozen.py` | `0cab2febdccbc371…` | 2026-10-14（**AI 代批，待人复核**） | 比较器 / 驱动 / 对照 |
| `scripts/parity/check_ruler.py` | `73ae7c49d5dcc07f…` | 2026-10-14（**AI 代批，待人复核**） | 比较器 / 驱动 / 对照 |
| `scripts/parity/controls/README.md` | `13845e46011a890e…` | 2026-10-14（**AI 代批，待人复核**） | 比较器 / 驱动 / 对照 |
| `scripts/parity/controls/review_r1.py` | `cfb45493dfbe84a1…` | 2026-10-14（**AI 代批，待人复核**） | 比较器 / 驱动 / 对照 |
| `scripts/parity/controls/run_all.py` | `2f27c71cd9b88005…` | 2026-10-14 #35 重登记（**AI 代批，待人复核**；人批方案 B） | 比较器 / 驱动 / 对照 |
| `scripts/parity/g03_compare.py` | `17e13a845aa5d8ef…` | 2026-10-14（**AI 代批，待人复核**） | 比较器 / 驱动 / 对照 |
| `scripts/parity/round.py` | `16746b22a573a1f0…` | 2026-10-14（**AI 代批，待人复核**） | 比较器 / 驱动 / 对照 |
| `scripts/parity/self_compare.py` | `9489d43055c43f5e…` | 2026-10-14（**AI 代批，待人复核**） | 比较器 / 驱动 / 对照 |
| `multimodal.md` | `e8a7b4c4025a2a3c…` | 2026-10-14 #35 新增（**AI 代批，待人复核**） | 原版多态场景登记（cancel_during_tool：TS 20 遍 17/3 两类）与每日重采流程 |
| `scripts/parity/score.py` | `4f7083ee901108c1…` | 2026-10-14 #35 新增（**AI 代批，待人复核**） | 计分：普通第 i 对第 i；多态整类匹配；日期 / 同源 / 不重叠前提 |
| `scripts/parity/controls/multimodal.py` | `bd2908b8db9d499a…` | 2026-10-14 #35 新增（**AI 代批，待人复核**） | 多态与日期前提的负例 / 正例 |

校验：`python3 scripts/parity/check_frozen.py`（全部登记文件的 sha256 前缀）。**冻结后改任何一份都要另开 issue、两家评审同意、写明理由**（`CLAUDE.md` §五）。
