# 2026-11-06 · #1716 裁决：取消偏离 A7-HOST-POST-CANCEL-EXIT-001-R3 在 #590 四格矩阵按「窄读」

## 卡

- **卡在哪**：`A7-HOST-POST-CANCEL-EXIT-001-R3` 末句写「四格矩阵（#590）中涉及取消的场景按本偏离判定」，但没说
  适用到哪几格。窄读 = 只覆盖宿主为 Rust 的格（③④）；广读 = 四格全管，把格②里四条取消场景也算进豁免。
  两种读法让 #590 格②的 sidecar 侧信号分别是 **23 条**（窄读）与 **19–21 条**（广读），直接影响账本计数。
- **谁受影响**：#590 四格账本的计数与结论、`specs/gateway-parity/diffs/` 下更正条目 R5 的「范围」节；
  不影响任何判分语义、阈值、冻结字节或产品代码。
- **能否撤回**：可撤回。本卡只记录一次口径选择；改判广读只需新开一条更正条目，不改既有文件。
- **动不动尺子 / 意向**：都不动。不改 `.42cog/intent.md`，不改 `accepted-diffs.md`、`FROZEN.md`、`specs/parity/README.md`。

## 人的裁决（原文）

- 决策单：https://github.com/daqi-dev/pilotdeck-rs/issues/1716
- 评论：https://github.com/daqi-dev/pilotdeck-rs/issues/1716#issuecomment-6675645168
- 时间：`2026-11-06T02:58:40Z`，作者 `zhengdaqi`
- 正文（全文两字）：**「按窄读」**

本卡只是把这条仓外批文在仓内留一份可追溯的记录，不新增解释、不扩写语义。原文与评论是唯一权威。

## 按此裁决的口径

1. `A7-HOST-POST-CANCEL-EXIT-001` 只覆盖**宿主为 Rust** 的格：③ Rust 宿主 × TS sidecar、④ Rust 宿主 × Rust sidecar。
2. 格① TS/TS、格② TS/Rust **不适用**；格②里那几条取消场景仍是「TS 宿主眼里观测到的 sidecar 侧差异」，
   按 sidecar 侧信号记（逐条名称与推导见 R5「范围」节，本卡不复制）。
3. 因此 #590 格②的 sidecar 侧信号为 **23 条（22 条纯换位 + `deadline` 契约不符），不因本条降低**；
   作为对照的广读数字 19–21 不改变这个计数。
4. 记录落点：`specs/gateway-parity/diffs/A7-HOST-POST-CANCEL-EXIT-001-R5.md`（更正条目，`original` 指向 R3）、
   `docs/research/008_phase-a-session-domain/four-cell-matrix.md` §六.8。

## 复核状态

**人已在 #1716 亲自评论裁决**，不是 AI 代批，无需再标「待人复核」。缺的是仓内留档，本卡补上。
