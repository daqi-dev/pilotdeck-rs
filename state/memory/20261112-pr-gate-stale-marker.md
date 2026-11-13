# PR 换头后旧结果块必须删 marker 行

**事件**：2026-11-12 两例。① PR 3047 r3 贴块时，正文还留着 r1 的 `<!-- pr-gate:v1 head=bc47115a4 …>` 旧块——check-pr-gate 逐块校验，旧块 head ≠ 当前 head 且补丁指纹不同 → gate FAIL；删掉该 marker 行（块正文可留作历史）即 PASS。② 1623 r8 评审后 PR 2957 正文积累了 r7 全量/r8 双家多块，头已变多次，全部待清。

**规则**：`scripts/check-pr-gate.py` 对正文里**每一个** pr-gate marker 逐块校验：块 head 必须等于 PR 当前 head（不等时仅 scope=full 且补丁指纹相同才沿用）；任一块无效即整闸 FAIL。贴新块前先 grep 正文所有 `pr-gate:v1` 行，凡 head 是旧头的删 marker 行再验；增量块（base=上轮 head）只要 head=当前 PR head 即合法。**How to apply**：`gh pr view <N> --json body --jq .body | grep 'pr-gate:v1'` 列出全部块 → 旧头块 `sed` 删行 → 本地 body-file 校验后再 edit。
