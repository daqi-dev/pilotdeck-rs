# 冻结登记与差异账本改为「一事件一文件」（人定 D，10-30）

> **人定 D（2026-10-30 12:54，issue #1085 评论「D」）。** 人 10-30 指示：由协调者起草。分级：**暂不执行 → 人批**（冻结尺子结构变更，CLAUDE.md §五「冻结尺子的变更」）。批准后按冻结程序执行：独立 issue、两家非作者评审、真实数据复算、重新登记。

## 卡在哪

- `specs/gateway-parity/FROZEN.md`：10-13 以来 **110 次提交**改它（另两份 FROZEN 合计 34 次）。文件里 92 段「重登记」叙述 + 26 行哈希都追加在同一处，两个在途单只要都改了被登记的装置就必然冲突。架构调研实测：FROZEN.md 96 个交叠对、`accepted-diffs.md` 92 个，合计约占全仓交叠的一半。
- 现场：今天 #1033（#1023）rebase 时，协调者手工合 `specs/ui-server-parity/FROZEN.md` 的哈希段与两段重登记；#209 链 FROZEN 当日 +4 次。
- 已有先例：gateway 差异账本 10-2x 已改为 `specs/gateway-parity/diffs/<ID>.md` 一条一文件（原 `accepted-diffs.md` 原字节冻结、`check_frozen.py --base` 强制只新增），此后 diffs 侧再无冲突。`specs/parity/` 与 `specs/ui-server-parity/` 的 `accepted-diffs.md` 仍在末尾追加（10-13 以来 14 次）。

## 谁受影响

- 不影响产品行为、不影响对拍判分。
- 影响：所有改冻结装置的 PR（对拍、负控尺子、B2 定标）的登记写法；三个 `check_frozen.py` 的读取方式；评审核对冻结登记的方式。

## 能否撤回

可以：旧文件原字节保留不删，新结构只是新增；撤回 = 停用新目录、恢复在旧文件末尾追加。

## 动不动尺子 / 意向

- **判分语义不动**（比较器、阈值、分母、场景、定标输入都不改）。
- 动的是**登记的存放形态**，属冻结尺子结构变更，所以须人批。

## 选项与后果

| 选项 | 做法 | 后果 |
|---|---|---|
| A 维持现状 | 继续在三份 FROZEN.md 与两份 accepted-diffs.md 末尾追加 | 交叠继续是全仓第一冲突源；每次 rebase 手工合哈希，容易合错（哈希错 = check_frozen 红，或更糟：合对了文字合错了归属） |
| B 只拆账本 | `specs/parity/`、`specs/ui-server-parity/` 照 gateway 先例加 `diffs/<ID>.md`，原 `accepted-diffs.md` 原字节冻结 | 去掉约 14 次 / 半月的账本冲突；FROZEN.md 不变，冲突大头仍在 |
| C 只拆 FROZEN | 哈希清单改由 `check_frozen.py --write` 从装置文件生成（冲突时重新生成即可，不再手合）；每次「重登记」叙述改为 `specs/<x>/frozen/<日期>-<ID>.md` 一事件一文件（含定标证据路径、两家评审、Approved-By / Approval-Basis）；现 FROZEN.md 的叙述段原字节保留为历史 | 去掉 FROZEN.md 的绝大部分冲突；check_frozen 需新增「生成清单与提交清单一致」与「每个哈希变化都有对应事件文件」两条机检 |
| **D = B + C（建议）** | 两件一起做，分两个 PR（先 B 后 C） | 冲突源基本清零；C 的机检把「改了装置却没登记」从评审人肉变成机器挡 |

## 建议

选 **D**，分两个 PR：

1. **PR-1（账本）**：parity / ui-server 两套 `diffs/` 目录 + README 模板 + check_frozen `--base` 只新增检查（直接复用 gateway 实现）。
2. **PR-2（FROZEN）**：三套 `check_frozen.py` 增加 `--write`（只写哈希清单区块）与「事件文件覆盖每个哈希变化」检查；FROZEN.md 叙述段冻结为历史。

执行：交给即将到位的 M6 专职 Agent 之外的单独 worker（这是尺子，不和负控 runner 重构混在一个 PR）；两家非作者评审；**真实数据复算**——在当前 main 上分别用旧、新 check_frozen 跑三套装置，输出的哈希集合与 PASS / FAIL 结论必须逐项一致，并附「改一个装置文件但不加事件文件 → 新检查红」负例。

## 抽出的原则

共享的「登记」文件一律一事件一文件，汇总清单由工具生成，不手合。可机检：check_frozen 拒绝「哈希变化但无对应事件文件」。
