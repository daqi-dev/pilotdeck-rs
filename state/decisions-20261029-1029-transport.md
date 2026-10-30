# 决策卡：D1-P04（#1029）允许对 SDK transport 做最小增量扩展（2026-10-29 12:2x）

**分级：自决（可撤回、在意向范围内、有预注册判据：191 格 TS 金样逐字节回放）**

## 卡在哪
D1 规划（`_tmp/20261028-issue-123-d1/plan/PACKAGES.md`）把 P04 的 token / argv / transport 定为「只读复用」。Codex 实测 TS settings 的真实 wire：① 请求里 `clientVersion` 是 `"0.1.0"`，而 P02 transport 固定写 `"pilotdeck-cli"`；② transport 参数走 `serde_json::Value`，保不住设置值里的孤立 surrogate，默认嵌套深度也会拒掉合法深对象。不改 transport，191 格回放首格即失败。

## 选项
- **A（采用）**：应用 Codex 已备的最小增量 `transport-extension.patch`——旧 `request` 入口与默认版本**不变**（P02 / P03 行为零变化），新增「可指定 clientVersion + 直接序列化 RawValue」的入口，只由 settings 使用；不加依赖。
- B：坚持只读，P04 只做到前置路径、完整回放留待 P05（P05 本来就串行持有 transport）——P04 无法验收、拖后。

## 能否撤回 / 动不动尺子
可撤回（新入口独立）。不动对拍判分；规划里的「只读」是为避免与并行包冲突，P03 已交付且不碰 transport、P05 未开工，无冲突。

## 原则
**包边界是为了避免并行冲突；没有并行持有人时，按实测需要做最小增量、旧入口零变化，并在下一包任务书里点名。**
