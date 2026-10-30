# 决策卡：#991 统一入口异常分类不改冻结装置判分语义（2026-10-29 04:3x）

**分级：自决（可撤回、在意向范围内、有预注册判据）**

## 卡在哪
#991 把 gateway-parity 十个入口的自举 / 校验 / 异常分类收成 `_guard.run_entry`，作者 RESULT §5 自报：意外异常路径的退出码与 JSON 契约变了（session_domain 普通异常 BLOCKED/2→FAIL/1；run_round BLOCKED/1→FAIL/1 且 `error`→`failure`；check_frozen / compare / build_proxy / controls traceback→FAIL/1 JSON）。r2 GLM 判 P1：与 FROZEN.md fix-r10「run_round 历史 BLOCKED/1 边界仅列出、未扩改」相反，属判分语义变更。

## 依据
CLAUDE.md「冻结尺子的变更」：改判分语义须明确授权，否则登记阻塞转独立工作；「装置自身崩溃」报成 FAIL（= Rust 与 TS 不一致）会让下游把装置故障读成对拍失败。#991 的目标是结构根治（消除逐字同步），不是改语义。

## 选项
- **A（采用）**：`run_entry` 接受逐入口策略参数，**逐字保留每个入口原有的异常→退出码 / JSON 映射**；结构仍是唯一实现、AST 机检不变。纯重构，无需授权。
- B：统一成新分类 → 判分语义变更，须人授权（暂不执行）。如需，另开提案单。

## 能否撤回
可（策略表一处改动）。不动尺子语义、不动意向。

## 原则
**结构重构不顺带改冻结装置的失败语义；要统一语义另起提案、走授权。** 机检：test_safety 对每个入口的「普通异常 / 取消 / 超时 / 权限」四类逃逸，断言退出码与 JSON 键与 main（重构前）逐字相同。
