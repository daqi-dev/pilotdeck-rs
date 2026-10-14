# exp013 · F2 流式 / 滚动 / 搜索黑盒断言在旧前端上的标定

- **假设**：4 条与框架无关的黑盒断言（25Hz×60s 每个 delta 都在 DOM 且底部跟随；阅读时不跳、恢复后跟随；两类搜索命中）能在旧前端（基线 `ui/` 完整构建）上通过，从而可作为 F5 聊天面的闸门。
- **做法**：协调者沙箱外、本机 Chrome、worktree 临时 profile，`node scripts/fe-parity/run.mjs --suite streaming-scroll --frontends old`，共 7 次实跑（old1–old7），每次失败都先取证再修。
- **过程（每轮挖出的一层）**：old1 会话页未挂载（mock 缺 `GET /api/modules/runtime`）→ old2 阅读不跳 200/400（夹具第二轮缺 user text 边界）→ old3/old4 仍 200（取证：mock 两轮帧号重叠）→ old5 「delta 丢失」（第一轮帧号不在 innerText）→ old6 同 → **old7 取证坐实**：第一轮 200 个段落全部 `inDOM: true`，所在行 `content-visibility: auto`，滚出视口后 **innerText 只数到 200、textContent 有 400**。最终断言改用 textContent 判断「每个 delta 都在 DOM」，语义不变。
- **结论**：**old7 旧前端 4/4 PASS**；新前端（探针页）NOT_RUN / NOT_IMPLEMENTED。两条搜索断言跟随基线可观察行为（命中 + DOM 可见），基线「不滚入视口」疑为原版缺陷，见 issue #70。
- **改变了哪个取舍**：断言计数从 innerText 改为 textContent（基线虚拟化 / content-visibility 下 innerText 不代表 DOM 包含关系）。
- **边界**：只在旧前端验证；新前端要到 F5 才有聊天面。原始输出在 worktree `_tmp/fe-parity/coordinator-stream-old{1..7}/`。
