# 冻结记录

| 文件 | sha256（前 16 位） | 冻结时间 | 说明 |
|---|---|---|---|
| `README.md` | `f8958a3280a95df7…` | 2026-10-14（人过目） | 比什么 · 四项对照 · 阈值程序（k = 5、S 规则、\|S\| ≥ 45、分母）· 差异分诊 |
| `accepted-diffs.md` | `653ccb1001d1b09a…` | 2026-10-14（人过目） | 表头；此后只增不删，AI 追加的条目状态为「待批」，追加不改表头 |
| `acceptance.md` | 待 Issue #1 | — | k、m 的具体数字与 S 集合；**先交人批再冻结** |
| `normalization.md` | 待 Issue #1 | — | 路径对齐、ID 一致映射、seq 保留、请求体规则；**先交人批再冻结** |
| `scripts/parity/` 全部比较器与驱动代码 | 待 Issue #1 | — | 与规约一起冻结，`scripts/parity/check_frozen.py` 校验 |

校验（Issue #1 之前）：`cd specs/parity && shasum -a 256 README.md accepted-diffs.md`，前 16 位与上表一致才算没被改过。
