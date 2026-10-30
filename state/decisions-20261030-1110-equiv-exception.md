# #1110（M6-pr3）等价门槛的两条有意差异（21:5x，AI 代批，待人复核）

- **卡在哪**：M6 等价门槛（人批）= 同一组变异在新旧 runner 上逐 case 判定一致。#1110 迁移 turn-submit 时，同 HEAD 样本比对 `equivalent=false`，差异两行：
  1. `baseline`：旧 INVALID（cargo exit=0）→ 新 PASS。旧 runner 写死期待 submit 4 / host 10 个测试，当前 HEAD 实际 7 / 12（后来的单子加了测试而旧 runner 没更新），旧正控在 main 上本来就恒 INVALID。
  2. `permission-check-skipped`：旧 INVALID（exit 101，被上面过期 baseline 支配）→ 新 KILLED（命名测试 + 专属断言实际命中）。
  无丢 case、无 test / assertion / schema 差异。
- **谁受影响**：只影响 turn-submit 负控的判定；方向是「旧尺子坏了（恒 INVALID）→ 新尺子能判」，不是放宽。
- **能否撤回**：可以（不合 #1110 / 回退迁移即恢复旧 runner）。
- **动不动尺子 / 意向**：不改判分语义；等价门槛本身不改，只对「旧 runner 在 main 上已坏」这一类差异给出例外口径。
- **决定**：自决 · 待复核——认这两行为有意差异，条件：① 在 PR 正文单列，附旧 runner 在 origin/main 上同样 INVALID 的原始输出（证明不是迁移引入）；② 宿主全量（57 + 22）新旧各跑一次后，除这两行（及任何同样能证明「旧 runner 在 main 已坏」的行，逐行列证据）外必须 equivalent；③ 不得为求 true 过滤行或改旧 summary。
- **原则**：等价门槛比较的是「判定」，当旧 runner 在 main 上已经判不出（恒 INVALID / 期待值过期），差异方向是从 INVALID 变为可判，可作为有意差异列出，但必须逐行附「旧 runner 在 main 上同样坏」的证据；反方向（KILLED → INVALID / SURVIVED → KILLED）一律不算例外。
