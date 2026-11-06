# #1900 条目 3：gateway-parity 权限开关不修，改登记已知限制

日期：2026-11-06 · 决策分级：自决 · **AI 代批，待人复核** · 关联：#1900 / #1525 / #1930

## 卡在哪

#1900 条目 3 要求给 gateway-parity 补一个与 Rust 侧 `PDRS_REQUIRE_PERMISSION_FAULTS`
等价的「权限面必须真跑到」开关；该开关只能落在 `scripts/gateway-parity/common.py`。

## 谁受影响

- 形式宿主（cb8 / mbp2 / macmini-01 与 CI）的对拍结论：今天都跑非特权用户，
  不受影响。
- 未来任何以 root / `CAP_DAC_OVERRIDE` 运行正式对拍的人：拿到的仍是
  `PASS_WITH_SKIPS` / exit 1，不是失败。
- 不被影响：Rust 侧用例（开关在 `src/pilotdeck-gateway/tests/common/permission_faults.rs`，
  本单已按条目 4 收紧）。

## 能否撤回

能。README 段落是纯文档，删掉即回到原状；没有改任何装置字节。

## 动不动尺子 / 意向

- 不动尺子的**字节**：`common.py` 一行未改（`check_frozen` PASS，30 条登记）。
- 若真加开关，会动**判分语义**（把一种 `PASS_WITH_SKIPS` 变成 `FAIL`），
  属于 CLAUDE.md「冻结尺子的变更」里需要明确授权的那一类。

## 选项与后果

| 选项 | 后果 |
| --- | --- |
| A 本单直接改 `common.py` 加开关 | 冻结装置字节变、判分语义变，需要独立 issue + 两家评审 + A1 二进制真实数据复算 + 重新登记；本单没有这个授权，且会把 P1 单拖成冻结变更单 |
| B 不修，写进 `scripts/gateway-parity/README.md` 已知限制 + 另开跟进单（**采纳**） | #1900 完成判据第 5 条明确允许这条路；缺口留痕、形式宿主覆盖方式写清；冻结字节不动，本单可正常合入 |
| C 什么都不写 | 缺口继续隐身，违反 #1900 判据 5 |

## 建议与结论

选 B。理由：当前行为是 `PASS_WITH_SKIPS` / exit 1 + 报告单列 skipped，已在
`DEGENERATE_PROBES` 白名单化，不构成假 PASS；而修它要付冻结变更的全套成本，
与本单「#1525 收尾 5 项」的范围不匹配。跟进单 #1930 已开，写「前置于下一次
`gateway-parity` 冻结登记窗口，或首次以 root 运行形式宿主对拍之前」。

可复用原则：**「不修」也必须留下机检得到或至少检索得到的痕迹** —— 缺口写进
被冻结装置旁边的 README，并开单承载后续，不让它只活在某次对话里。
