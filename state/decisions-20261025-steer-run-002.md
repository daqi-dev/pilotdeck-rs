# 决策 · 2026-10-25 · abort 落在 steer drain 与 apply 之间（A6.9-STEER-RUN-002）

## 人定 · 选 B：保留 Rust 丢弃，批准差异（2026-10-25 22:28，#713 评论）
- **卡在哪**：#708 生命周期矩阵 A-S2×E4 前半实测：abort 落在 steer 已 drain、尚未 apply 之间时，TS 仍完成 claim → durable → ack 并发出 `steer_applied`（modelCalls=0）；Rust 在 durable 前检查到取消，拒绝并转 discard，不落账。
- **人的原话（逐字，来自 #713 评论）**：选择按照建议 B：用户已发出取消，再把取消前排队的 steer 写进历史容易让人误以为模型看过它（modelCalls=0）；但会让「取消前已排队的 steer」在 Rust 里丢失，需在 https://github.com/daqi-dev/pilotdeck-rs/issues/686 的「明示丢弃」里一并提示。
- **决定**：差异条目 `specs/gateway-parity/diffs/A6.9-STEER-RUN-002.md`（随 #714 入库）批准状态为 **人批（2026-10-25）**；Rust 行为不改。
- **连带要求**：#686「明示丢弃」须覆盖这类被丢弃的 steer（取消前已排队、未 apply），提示形状有用例锁住。
- **能否撤回**：能；改为对齐 TS 需另开单。
- **原则**：取消语义优先于「已排队即应用」——用户取消后不再把未被模型看过的输入写进历史，但丢弃必须对用户可见。
