# exp013 · F2 流式 / 滚动 / 搜索黑盒断言在旧前端上的标定

- **假设**：4 条与框架无关的黑盒断言（25Hz×60s 每个 delta 都在 DOM 且底部跟随；阅读时不跳、恢复后跟随；两类搜索进入视口）能在旧前端（基线 `ui/` 完整构建）上通过，从而可作为 F5 聊天面的闸门。
- **做法**：协调者沙箱外、本机 Chrome、worktree 临时 profile，`node scripts/fe-parity/run.mjs --suite streaming-scroll --frontends old`，已完成 7 次实跑（old1–old7）；第 8 轮代码已修，old8 待协调者运行，每次失败都先取证再修。
- **过程（每轮挖出的一层）**：old1 会话页未挂载（mock 缺 `GET /api/modules/runtime`）→ old2 阅读不跳 200/400（夹具第二轮缺 user text 边界）→ old3/old4 仍 200（取证：mock 两轮帧号重叠）→ old5 「delta 丢失」（第一轮帧号不在 innerText）→ old6 同 → **old7 取证坐实**：第一轮 200 个段落全部 `inDOM: true`，所在行 `content-visibility: auto`，滚出视口后 **innerText 只数到 200、textContent 有 400**。最终断言改用 textContent 判断「每个 delta 都在 DOM」，语义不变。
- **结论（第 8 轮更正）**：old7 的 **4/4 PASS 只对应当时放松后的 legacy 断言**，不能证明搜索定位已通过。撤回“基线不滚入视口、疑为原版缺陷”：更对口的 `ui/e2e/streaming-search.spec.mjs:21–27` 要求 `toBeInViewport`，`coordinator-baseline-old2/old-{staffdeck,minimal,replacement}.json` 三份原件均 passed；issue #70 已关闭为不成立。
- **第 8 轮修改**：恢复 `toBeInViewport`，将距底 >50px 放回 `readUp` 之前；对照 transcript 的顶部起点和完整壳额外的滚动跟随/阅读锚点，用真实 Home 建立稳定阅读前置状态，并将命中移到 Answer 21（仍为 24 轮，后留两轮空间），先断言目标在视口外，再搜索定位；尾部目标对齐基线，也让后续流式行留在阅读区域附近。完整壳与 transcript 的容器、行窗口、尺寸、触发方式对照见 [runner README](../../../scripts/fe-parity/README.md#第-8-轮恢复搜索定位双文本判定双协议线路)。这修正夹具前置状态；精确浏览器帧上的失败原因及修复有效性仍待 old8，不宣称已证锚点竞争，也不代替从底部直接搜索的验收。
- **双文本判据**：保留 textContent 的 0000–0399，另要求 innerText 恰好为 0200–0399，两者有序且各一次。old7 原件满足两项，离线自检增加第二轮隐藏/空文本/漏号/重号/乱序反例。
- **双线路判据**：四项测试都在 legacy 和 timeline 下运行，当前分母 8。timeline 含 blockId、turnId 所属的 timeline 坐标、offset、streamBoundary、assistant_block 的 isFinal 最终投影；自检执行冻结 allocator/projector/bridge/history mapper 逐帧比较，并在最终快照之前检查每个增量前缀。原生 projector 不发 message_end 的限制已明确记录，边界单独对比 allocator 与 bridge 契约。
- **边界**：新前端（探针页）NOT_RUN / NOT_IMPLEMENTED，要到 F5 才有聊天面。第 8 轮只完成离线自检；协调者使用新目录 `coordinator-stream-old8`，须 8/8 PASS、0 skip 才放行。原始输出在 worktree `_tmp/fe-parity/coordinator-stream-old{1..7}/`。
