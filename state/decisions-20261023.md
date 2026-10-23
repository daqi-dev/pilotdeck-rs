# 2026-10-23 决策

## #508 MCP 工具结果自动读图越出 cwd（人定，2026-10-23）
- **决定**：选 A，向安全偏离 TS。读图前先规范化真实路径（realpath），只允许 cwd 及其子目录（另可配置白名单）；拒绝符号链接越界。
- **来源**：用户本人在协调者会话中的指示（原话见 `_tmp/20261023-loop/card-mcp-image-read.md` 末尾）。
- **原则**：TS 有数据外泄 / 越权读的缺陷时，Rust 向安全偏离并登记，不复制缺陷（与 #496 同一原则）；这条人批也印证了 pr10g 路径穿越、#484 根边界两项 AI 代批的方向。
- **执行**：派 codex（wt-issue-508-mcp-image）；差异账本登记 A6.5-MCP-IMAGE-READ-SANDBOX-001，批准记录写「人批 2026-10-23」。

## 10-23 自决 · 待复核（AI 代批，待人复核；卡在 _tmp，路径如下）
- **V8 栈深判据**（#536 #539）：不追 V8 精确栈深；重构保持 PR 前深度行为，现实范围与 TS 一致、病态深度有界不崩溃，剩余差异逐条登记。卡 `_tmp/20261023-loop/card-v8-stack-depth.md`。原则：不可复刻的运行时常数不做逐字对拍，改判「有界 + 登记」。
- **EdgeClaw 条目路径穿越**（pr10g，#537）：拒绝越出 memory 根的 id，向安全偏离并登记 A6.6-EDGECLAW-ENTRY-PATH-CONTAIN-001。卡 `_tmp/20261023-loop/card-edgeclaw-entry-traversal.md`。
- **tower-service 直接依赖**（#546）：锁文件已有同版本、不新增包，按 sha1 卡先例自决。卡 `_tmp/20261023-loop/card-tower-service.md`。
- **#541 skills 自指链接**、**#484 插件防环与根边界**：TS 自身不终止 / 越界，Rust 有界化并登记（A6.8-SKILLS-TREEFILES-001、A6.3-PLUGIN-MARKDOWN-CYCLE-001 / ROOT-001）。

## 10-23 待人定（说明卡）
- #454 web_fetch 大页内存：`_tmp/20261023-loop/decision-454-webfetch-memory.md`（建议 C）。
- #505 32 条已知未对齐：`_tmp/20261023-loop/decision-505-diffs.md`（分档）。
- 状态文件精简：`_tmp/20261023-loop/proposal-state-files.md`。
