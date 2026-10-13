# 冻结记录（人过目后生效）

| 文件 | sha256（前 16 位） | 冻结时间 | 说明 |
|---|---|---|---|
| `README.md` | `f5f67586bbfa5656…` | **待人过目** | 一、二、四节来自人已拍的板；三节（阈值程序、分母、S 下限）待人定 |
| `accepted-diffs.md` | `653ccb1001d1b09a…` | **待人过目** | 表头；此后只增不删，AI 追加的条目状态为「待批」 |
| `acceptance.md` | 待 Issue #1 | — | k、m 的具体数字与 S 集合；**先交人批再冻结** |
| `normalization.md` | 待 Issue #1 | — | 路径对齐、ID 一致映射、seq 保留、请求体规则；**先交人批再冻结** |
| `scripts/parity/` 全部比较器与驱动代码 | 待 Issue #1 | — | 与规约一起冻结，`scripts/parity/check_frozen.py` 校验 |

校验（Issue #1 之前）：`cd specs/parity && shasum -a 256 README.md accepted-diffs.md`，前 16 位与上表一致才算没被改过。
