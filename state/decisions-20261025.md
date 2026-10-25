# 决策卡 · 2026-10-25

## 卡 · #589/#632 取消退出慢消费方的 P2 降级（AI 代批，待人复核）

- **卡在哪**：#589/#632 的修复分支 `claude/issue-589-cancel-delivered` 的 r2 增量评审里，GLM 与 opus 两家独立指出，`host/execution.rs` 取消退出的一次性 catch-up 只覆盖单步应用。多步回调、`mpsc::channel(1)` 背压、连接缓冲帧这三种情况下，取消期限到点后仍会丢掉合法 lifecycle，结果是判错（TS 无限等待后报 success）。协调者当时按「可能影响对拍结论」上调为 P1，派 DeepSeek r3 根治。r3 在设计推敲中输出被截断，没有产出。这已是同一处取消退出逻辑的第 3 次修复尝试。
- **谁受影响**：只有「用户已取消、且消费方停顿超过 5 s」的回合；#590 四格矩阵里如有这类取消场景，会受影响。
- **能否撤回**：能。r2 的修改是严格改进，有测试覆盖；降级只影响本 PR 能否合入，不改变判分语义。
- **动不动尺子 / 意向**：不动。期限值 5 s 不变，R1 负控不变；不新增豁免。
- **选项与后果**：
  - A. 继续在本 PR 根治：需要在期限到点后做有界收尾，同时不违反 R1「唯一绝对期限」，设计难度高，已失败一轮；会拖住 #589 主线（取消退出不再假成功）合入。
  - B. 降为 P2c 跟进（#643，前置于 #590 取消场景）：保留 r2 的单步补救和测试，在账本 R2 中如实写明覆盖不到的情况，本 PR 只修便宜的小项。
- **决定**：B。理由：这几种情况属于已登记偏离 A7-HOST-POST-CANCEL-EXIT-001（TS 取消后无限等待，Rust 有界）的边界，方向是 fail-closed，不产生假成功；#589 的 P1（取消退出导致的假成功）已修并有负控。
- **可复用原则**：同一处修复第 3 次仍不收敛时，先回到账本核对它是否已是登记偏离的边界；是的话降级为前置于具体节点的跟进单，并写明理由，不在同一 PR 里继续加码。
- **状态**：AI 代批，待人复核。**后续**：2026-10-25 人定「#643 选 A」，本卡的降级结论获人确认，见下方「人定 · 2026-10-25 · #643」。

## 卡 · #649-3 工具结果落盘目录在共享 /tmp 下的加固（AI 代批，待人复核）

- **卡在哪**：`GatewayToolResultArtifactStore` 的默认根目录在 TMPDIR/TMP/TEMP 都未设置时是 `/tmp/pilotdeck-tool-results`，所有本机用户共用。写入用 `create_dir_all` 加 `OpenOptions::create+truncate`，没有 `O_NOFOLLOW`；路径名可预测（`<session>/<turn>/<call>.txt`）。本机其他用户可以预先放置目录或符号链接，让工具结果写进别处、截断别处的文件，或者读到工具输出。TS（`src/gateway/client/GatewayToolResultArtifactStore.ts:32` 起）写法相同，这是 TS 原有行为；macOS 下 `os.tmpdir()` 通常是用户私有目录，Linux 下常为共享的 `/tmp`。
- **谁受影响**：Linux 多用户主机上运行 gateway、且没设置 TMPDIR 的用户；单用户机与 macOS 默认情况基本不受影响。
- **能否撤回**：能。只改写入方式与目录权限，不改目录名，不改落盘内容。
- **动不动尺子 / 意向**：不动尺子。对 TS 是向安全方向偏离，须在差异账本新增条目，状态写「未批准，非已接受差异」。
- **选项与后果**：
  - A. 保持与 TS 一致，只把风险登记进账本。零改动，风险留着。
  - B. 加固：根目录与各级子目录用 0700 创建；创建后核对属主是当前 euid，且不是符号链接，不符就拒写并报错；文件用 `O_NOFOLLOW`、0600 打开。写入成功时行为不变。
  - C. 与 dump 一样改成独占创建加随机名：改动大，而且结果文件的路径会被回传给模型，不可预测的名字会改变对外可见的路径。
- **决定**：B。理由：与 #621 / A6.4-MODEL-DUMP-SECURE-001 对 dump 的处理同一方向（安全类偏离，执行修复并登记，待人批）；B 不改路径名，对外可见行为只在遭遇恶意预置时不同（此时 TS 会写错地方，Rust 改为报错）。
- **可复用原则**：落盘到可能共享的临时目录时，凡路径可预测的，至少做到「目录 0700 且核对属主、文件不跟随符号链接」；需要偏离 TS 时，按安全类偏离登记并标待人批。
- **状态**：AI 代批，待人复核。实现另派，先等 #650（同改 projector.rs）合入。

## 人定 · 2026-10-25 · #643 取消退出到点后的有界收尾：选 A
- **原话**：「#643 选 A」
- **决定**：维持现状（选项 A）。取消被观察到后 5 s 期限到点时仍有未交付的合法 lifecycle、因而报错（fail-closed，不会假成功）的情况，登记为已接受的已知偏离，归入 A7-HOST-POST-CANCEL-EXIT-001（TS 取消后无限等待，Rust 有界）。R1 的「唯一绝对期限，不重算」不变。**受影响情况的机制与清单以 `specs/gateway-parity/diffs/A7-HOST-POST-CANCEL-EXIT-001-R3.md` 为唯一正本**，本卡不另列，避免两处不同步。
- **落地**：账本更正 `specs/gateway-parity/diffs/A7-HOST-POST-CANCEL-EXIT-001-R3.md`；#590 四格矩阵中涉及「取消 + 慢消费方」的场景按此偏离判定；#643 随本记录合入后关闭。选项分析见 #643 评论（`_tmp/20261025-claude-loop/issue-643-options.md`）。
- **可复用原则**：有界期限只防对端拖延；消费方自身导致的延迟造成的 fail-closed 结果，登记为已知偏离，不为它重算期限。

## 人定 · 2026-10-25 · #590 A7.2-pr6b 四格矩阵：授权修改冻结装置
- **原话**：「#590 A7.2-pr6b 四格矩阵——授权修改冻结装置（事后请给我更详细解释）」
- **决定**：授权按冻结程序（CLAUDE.md 第五节：独立 issue、理由、两家评审、真实数据复算、重新登记哈希），在实现 native 格（Rust host + TS/Rust sidecar）的执行与审计所必需的范围内，修改 `scripts/gateway-parity/session_domain.py` 及其配套冻结件。
- **范围**：默认只新增 native 执行与审计，不改已有判分语义（阈值、分母、已登记差异的判定方式）。人随后补充（原话）：「#590 授权修改冻结装置，如果实现中发现必须改判分语义，那就按照你的判断来修改。」故实现中确有必要时，判分语义的修改也由协调者判断决定，条件如下：
  - 改动理由只来自 TS 基线与场景本身，**不看 Rust 结果定阈值，不为变绿改标准**（CLAUDE.md 第五节）；
  - 每一处语义改动单独写决策卡，标「按人授权，AI 判断」，写清改前改后、理由与受影响场景；
  - 照常走冻结程序：两家非作者评审、真实数据复算（TS 自比与对照仍须通过）、重新登记哈希；
  - 交付时向人逐条列出所有语义改动。
- **前置**：#589（A7.2-pr6a LiveSession 生产闭环）完成后开工。
- **待办**：向人提供更详细的解释（人要求「事后」），在 #590 开工或交付时一并给出。

## 人定 · 真实 provider 实效分两层（2026-10-25 16:5x）
- **卡在哪**：各包 RESULT 的「真实 provider 端点实效：待人工验收」越积越多（#586 permission / hooks / steer，#589 端口接线），都要等人手动在真实 provider 上点。
- **人定**：同意分两层，尽量由 AI 判断。第一层是 AI 实测：用现有 key（只从 `.env.local` 读）在隔离 home 下实跑各包写明的验收步骤，留原始记录，自行判定通过与否，标「AI 已实测，待人签字」。第二层是人签字：人只看报告签字；UI 体验、文案观感这类需要人眼判断的，单独列给人。
- **护栏**：不碰 `~/.pilotdeck` 和用户正在运行的 PilotDeck；提示词与夹具不含密钥、个人信息或未公开内容；外发只发往已配置的 provider（llm-center）；单次冒烟的请求数与 token 上限写成脚本常量并随原始记录留档；原始记录写进 `_tmp/<日期>-<包名>-smoke/`，独占创建、不覆盖；与评审闸门共用的 key 按其并发上限排队。
- **不改变的**：AI 实测不改变阶段完成状态，人签字后才算实效通过；AI 不代人签字。
- **能否撤回**：能，恢复 CLAUDE.md §三原文即可。
- **原则**：能用真实 provider 跑出来的结论，不留给人手动点；人的时间只花在签字和机器判不了的体验上。
## 人批 · 待批清单逐条决定（2026-10-25 17:14）
人在协调者会话里逐条勾选「pdrs 待批清单」（`_tmp/20261025-claude-loop/pending-review.html`，条目由 `_tmp/20261025-claude-loop/pending-diffs.json` 收集）后提交决定，原话「以下决定仅供参考，如果你觉得有更合理的决定，也可以沟通」。下方逐字保留人的提交原文，作为这批批准的持久指针。gateway-parity 条目另新增 `-R<n>.md` 指回本节。

- **批准 85 条**：所列条目的批准状态由「未批准 / AI 代批，待人复核」改为**人批（2026-10-25）**。批注「regress 接入暂停：或者换到更优的实现」另开跟进，评估替代实现。
- **需讨论 8 条**：协调者逐条回复给人，结论另记。
- **应修代码 9 条**：不再作为差异接受，改为修代码，逐条开 issue 进 READY。

<details><summary>人的提交原文（逐字）</summary>

```text
pdrs 待批清单 · 我的决定
【批准】85 条
- R5 · 冻结记录的人批字段可写「AI 代批，待人复核」（state/decisions-20261014.md:156）
- A6.7-pr4：hooks matcher 消费型嵌套量词回溯不设预算（state/decisions-20261020.md:21）
- regress 接入暂停（执行更正）（state/decisions-20261022.md:72）：或者换到更优的实现
- A6.6-EDGECLAW-ENTRY-PATH-CONTAIN-001（specs/gateway-parity/accepted-diffs.md:704）
- A6.6-EDGECLAW-IMPORT-SWAP-ROLLBACK-001（specs/gateway-parity/accepted-diffs.md:892）
- 10-23 · EdgeClaw 条目路径穿越（state/decisions-20261023.md:11）
- A6.3-PLUGIN-MARKDOWN-ROOT-001（specs/gateway-parity/diffs/A6.3-PLUGIN-MARKDOWN-ROOT-001-R3.md）
- A6.4-MODEL-DUMP-SECURE-001（specs/gateway-parity/diffs/A6.4-MODEL-DUMP-SECURE-001.md）
- A6.9-PERMISSION-RUN-001（specs/gateway-parity/diffs/A6.9-PERMISSION-RUN-001-R2.md）
- A6.9-TOOL-RESULT-DIR-SECURE-001（specs/gateway-parity/diffs/A6.9-TOOL-RESULT-DIR-SECURE-001-R1.md）
- A6.9-STEER-RUN-001（specs/gateway-parity/diffs/A6.9-STEER-RUN-001.md）
- #649-3 工具结果落盘目录在共享 /tmp 下的加固（state/decisions-20261025.md:16）
- R1 · B2 比较器：消息 / @chat 时间顺序（state/decisions-20261014.md:141）
- R2 · YAML 解析忠实移植 npm yaml 2.9.0（state/decisions-20261014.md:142）
- R3 · system_clock 停止裁剪，只入仓 Node 实测回归（state/decisions-20261014.md:143）
- R4 · 尺子装置完整性的威胁模型（state/decisions-20261014.md:155）
- R6 · A5.1 依赖账本定位为规划索引（state/decisions-20261014.md:157）
- R7 · YAML 深嵌套阈值（state/decisions-20261014.md:158）
- R8 · F2 基线 spec 失效登记（state/decisions-20261014.md:159）
- R9 · F2 失效级联（state/decisions-20261014.md:160）
- R10 · 账本校验器的防护边界（state/decisions-20261014.md:166）
- R11 · YAML 资源判据改按 RSS 增量（state/decisions-20261014.md:167）
- R12 · 被测进程的 HOME 隔离（state/decisions-20261014.md:168）
- R15 · session_model_get 仍属 (a) 类本地化（state/decisions-20261014.md:211）
- R16 · 产品入口账本的有限实现（state/decisions-20261016.md:3）
- R19 · 非良构 UTF-16 排序的精确宿主边界（state/decisions-20261016.md:30）
- R21 · #112 真实 sidecar 绝对 deadline 与读面结算（state/decisions-20261016.md:42）
- R24 · 外审超时的有界诊断（state/decisions-20261016.md:60）
- R25 · #65 保留 P1 并先补调度证据（state/decisions-20261016.md:66）
- R26 · #113 批准原件的可移植登记（state/decisions-20261016.md:76）
- R31 · #52 与 #85A 组合输入加固的准备范围（state/decisions-20261016.md:105）
- R32 · #56.2 先限定资源域补事实（state/decisions-20261016.md:116）
- R34 · 动态 Snapshot 数值复算差异另列未修（state/decisions-20261016.md:132）
- R37 · 逐子项主次归类与等级轴分离（state/decisions-20261016.md:162）
- R40 · #65 只实施测试共同提交域原型（state/decisions-20261016.md:188）
- R41 · 类型计数谓词与 R37 措辞纠正（state/decisions-20261016.md:198）
- R42 · 独立改级后的 R37 保护基线刷新（state/decisions-20261016.md:204）
- R43 · 快照外 #145 单独登记（state/decisions-20261016.md:212）
- R46 · 用已核新执行器恢复 #138 边界的有界诊断（state/decisions-20261016-followup.md:3）
- R47 · 取得健康证据后先做四个实路径负控（state/decisions-20261016-followup.md:11）
- R50 · #65 TS 真实结果到 RPC 提交资格的窄映射（state/decisions-20261016-followup.md:33）
- R52 · 资源单位与并发执行边界消歧（state/decisions-20261016-followup.md:42）
- #167 卡 1 · 代理轮用哪个二进制（state/decisions-20261017-167-ruler.md:3）
- #134 授权内容（AI 归纳，待人复核语义无损）（state/decisions-20261017.md:9）
- Claude 作者的 PR 怎么过第 2/3 步（#185）（state/decisions-20261017.md:60）
- ui-server #94 两条验收债与定标资源门（specs/ui-server-parity/accepted-diffs.md:56）
- GLM 试点结论（#226）（state/decisions-20261017.md:93）
- B2 定标装置磁盘上限对齐现行人定（state/decisions-20261017.md:97）
- D0 跨流顺序不可观察场景按流比较（state/decisions-20261017.md:136）
- codex 通道额度耗尽（至 10-25）——实现与评审改派（state/decisions-20261017.md:143）
- 闸门复跑只剩 P2 时归入跟进单（state/decisions-20261017.md:165）
- A6.4-pr5：anthropic 流的 jsonrepair 缺口（state/decisions-20261020.md:14）
- CLAUDE.md 精简（state/decisions-20261020.md:42）
- A6.6-pr8b：compaction「重入被拒」验收改挂 A7.1（state/decisions-20261020.md:56）
- pr9 同源遗漏的范围（state/decisions-20261021.md:3）
- gateway 差异登记账本放错（state/decisions-20261021.md:69）
- #386 unsafe 例外改为独立 crate，gateway 恢复 forbid（state/decisions-20261021.md:79）
- #386 守卫的构建配置检查改为「一律不许存在」（state/decisions-20261021.md:89）
- #421-surrogate-p1（state/decisions-20261022.md:2）
- A7.2-ZERO-RETENTION-TIMER-001（specs/gateway-parity/accepted-diffs.md:426）
- A7.2-COORDINATOR-SINK-OWNERSHIP-001（specs/gateway-parity/accepted-diffs.md:460）
- A7.5-REPLACEMENT-ACTIVE-GETTER-PANIC-001（specs/gateway-parity/accepted-diffs.md:492）
- D2-CHATSEARCH-REGEX-BACKTRACK-001（specs/gateway-parity/accepted-diffs.md:503）
- D2-CHATSEARCH-REGEX-GIANTQUANT-001（specs/gateway-parity/accepted-diffs.md:513）
- A6.7-HOOK-OUTPUT-001（specs/gateway-parity/accepted-diffs.md:537）
- A7.4-HTTPSTORE-AUTH-BEFORE-BODY-001（specs/gateway-parity/accepted-diffs.md:559）
- A7.4-HTTPSTORE-BODY-LIMIT-001（specs/gateway-parity/accepted-diffs.md:562）
- A7.4-HTTPSTORE-TIMEOUTS-001（specs/gateway-parity/accepted-diffs.md:565）
- A7.4-HTTPSTORE-MAX-CONNECTIONS-001（specs/gateway-parity/accepted-diffs.md:571）
- A7.4-HTTPSTORE-KEY-ORDER-001（specs/gateway-parity/accepted-diffs.md:577）
- A7.4-HTTPSTORE-CLIENT-ERROR-CLASS-001（specs/gateway-parity/accepted-diffs.md:580）
- A7.2-READ-LASTSEQ-DEPTH-001（specs/gateway-parity/accepted-diffs.md:674）
- A6.6-EDGECLAW-SNAPSHOT-CPSYNC-ERROR-001（specs/gateway-parity/accepted-diffs.md:731）
- A6.3-PLUGIN-MARKDOWN-CYCLE-001（specs/gateway-parity/accepted-diffs.md:742）
- A6.5-MCP-SCHEME-001（specs/gateway-parity/accepted-diffs.md:827）
- A6.5-MCP-TLS-CAUSE-001（specs/gateway-parity/accepted-diffs.md:836）
- 10-23 · V8 栈深判据（state/decisions-20261023.md:10）
- 10-23 · tower-service 直接依赖（state/decisions-20261023.md:12）
- 10-23 · #541 skills 自指链接、#484 插件防环与根边界（state/decisions-20261023.md:13）
- #547 两块按「同一处修 3 次不过」挂起拆分（state/decisions-20261023.md:49）
- A6.8-SKILLS-TREEFILES-002（specs/gateway-parity/diffs/A6.8-SKILLS-TREEFILES-002-R1.md）
- A6.6-MEMORY-PROVIDER-OPTIONS-001（specs/gateway-parity/diffs/A6.6-MEMORY-PROVIDER-OPTIONS-001.md）
- A6.9-INTERACTION-EVENTS-001（specs/gateway-parity/diffs/A6.9-INTERACTION-EVENTS-001.md）
- A6.9-TOOL-RESULT-COLLISION-001（specs/gateway-parity/diffs/A6.9-TOOL-RESULT-COLLISION-001.md）
- A7.4-HTTPSTORE-CLIENT-BODY-UNITS-001（specs/gateway-parity/diffs/A7.4-HTTPSTORE-CLIENT-BODY-UNITS-001-R2.md）
【需讨论】8 条
- A7.2-COORDINATOR-DELIVERY-001（specs/gateway-parity/accepted-diffs.md:449）
- A6.5-MCP-TLS-TRUST-001（specs/gateway-parity/accepted-diffs.md:818）
- A6.6-EDGECLAW-WRITE-INODE-001（specs/gateway-parity/diffs/A6.6-EDGECLAW-WRITE-INODE-001-R1.md）：意思是：批准这种情况下Rust拒写，但是要记录差异
- A6.7-HOOK-TLS-TRUST-001（specs/gateway-parity/diffs/A6.7-HOOK-TLS-TRUST-001-R1.md）：意思是：私有 CA 下 Rust应该也成功
- A6.9-HOOKS-RUN-001（specs/gateway-parity/diffs/A6.9-HOOKS-RUN-001.md）：意思是：会延续到下一轮
- A7.2-INTERACTION-CONSUME-001（specs/gateway-parity/diffs/A7.2-INTERACTION-CONSUME-001-R1.md）
- A7.4-HTTPSTORE-431-408-SHAPE-001（specs/gateway-parity/accepted-diffs.md:574）
- A7.4-TOOLBUDGET-SPILL-PARTIAL-FILE-001（specs/gateway-parity/diffs/A7.4-TOOLBUDGET-SPILL-PARTIAL-FILE-001.md）
【应修代码】9 条
- parity #1–#3 · deadline 场景多写 gateway_submit_failed（specs/parity/accepted-diffs.md:9）
- A6.5-MCP-PROXY-HTTPS-001（specs/gateway-parity/accepted-diffs.md:910）
- A6.5-MCP-PROXY-SOCKS-001（specs/gateway-parity/accepted-diffs.md:924）
- A6.5-MCP-PROXY-FALLBACK-001（specs/gateway-parity/accepted-diffs.md:935）
- 已知未对齐（待 pr9c）· web_fetch HTML 解析差异 4 类（specs/gateway-parity/accepted-diffs.md:64）
- A7.3-INTERACTION-BOUNDARY-001（specs/gateway-parity/accepted-diffs.md:413）
- A7.4-HTTPSTORE-KEEPALIVE-001（specs/gateway-parity/accepted-diffs.md:568）
- A6.6-EDGECLAW-ENTRY-IDS-SHAPE-001（specs/gateway-parity/accepted-diffs.md:713）
- A6.5-MCP-CONNECT-HOSTNAME-001（specs/gateway-parity/accepted-diffs.md:850）
未决 0 条
```

</details>

### 人定 · 需讨论 3 条（2026-10-25 17:24）
协调者补充场景说明后，人回复「① A ② A ③ A」：
- **① A6.9-HOOKS-RUN-001 选 A**：一轮结束时没处理完的 hook 事项保持丢弃，不延续到下一轮（防上一轮的「允许」误放行这一轮的工具调用）；同时要明示「上一轮有 N 个待处理项已作废」——另开实现单。
- **② A7.2-INTERACTION-CONSUME-001 选 A**：先不批，#589 消费包合入后把最终差异整理成一句话再请人批。
- **③ A7.4-TOOLBUDGET-SPILL-PARTIAL-FILE-001 选 A**：批准 Rust 写失败时删掉半截文件、重试重写（成功路径字节与 TS 一致）。条目在 #637 分支，随该 PR 把批准记录写进条目文件。
## 人定 · 提速五条与拆包、合成 PR（2026-10-25 17:53）
人在协调者会话里回复「1 同意 2 同意 3 同意 4 同意 5 同意；Split packages to about 500 lines instead of 900；Keep merging stacked packages into combined PRs」。对应协调者的提案：
1. **只修 P2 的轮次轻复核**：P2 修复由一家非作者复核增量 diff 即可（默认 DeepSeek 官方通道）；修 P0/P1 的轮次仍需两家。合并前最后一个结果块须覆盖当前 head（或经补丁指纹沿用）。落 CLAUDE.md「合并闸门」④。
2. **交付前非作者预审**：实现方交付后、正式评审前，先由 DeepSeek 官方通道（与作者不同谱系时）快速审一遍 diff，实现方修完再交；正式两家评审照旧。
3. **一键收货脚本**：协调者用脚本完成 apply → 按实际实现方署名提交 → 推送 → 宿主全量 lib → 两家评审 → PR 正文草稿。
4. **DeepSeek 实现也走官方 key**：dsh 新增官方通道 patch（`DS_ORI_PARA_API_KEY`，模型 `deepseek-flash`），实现并发不再受 llm-center 50 RPM 限制。
5. **worktree 上限 16 → 24**：落 CLAUDE.md 资源上限与 `scripts/wt-status.sh`。
- **拆包**：单包约 500 行（原约 900 行）。
- **合成 PR**：叠包在前序合入后，可把已各自评审收敛的后续包 rebase 合成一个 PR，对 main 全量再审一次后入队；风险与护栏见本卡下方。
- **护栏（合成 PR）**：只合并已各自评审收敛的包；保留各包独立提交（REBASE 入队后可按提交回滚）；合成后 diff 超过约 1500 行则不合并；任一包出 P0/P1 只回退该包，其余包可另开 PR。
- **能否撤回**：均可撤回，恢复本卡之前的 CLAUDE.md 与脚本常量即可。

## 卡 · A5.4 第 2 包（#85 #52）加固范围收口（2026-10-25 19:24，自决 · 待复核）
- **卡在哪**：冻结装置未登记扫描（scripts/gateway-parity）连续 6 轮被评审打回，每轮都是一种新的「仓库内刻意构造的解释器级绕过」：扩展模块 → 符号链接 → 嵌套 node_modules / 大写后缀 / 不可读目录 → unchecked-hash pyc → 同名包或扩展模块遮蔽 common → 不带 -I 时标准库遮蔽。每轮都按 P0/P1 修了，并用 A1 二进制做了 A2 真实复算（r3–r8 机器项全过）。
- **谁受影响**：A5.4 冻结前置（#85 #52），以及 #590 四格矩阵的时间线。
- **依据**：人批 R4「尺子装置完整性的威胁模型」（state/decisions-20261014.md，人 2026-10-25 在待批清单批准）：防意外漂移与仓库内改动，**不防本机恶意者**。
- **决定**：本包修完 fix-r7（测试不写真实装置树、入口断言 `-I`、模块清单派生）后收口。此后评审再报「刻意构造」类解释器级绕过（文件内容专为欺骗 CPython 或 Node 的加载器而构造），登记为已知边界，不在本包修；意外漂移、常规仓库内改动（误放文件、漏登记、未跟踪配置）仍按 P0/P1 必修。已知同类问题在姊妹装置的另开 #694。
- **能否撤回**：能；人若认为需要防刻意构造，另开单逐类加固。
- **原则**：先确认威胁模型，再决定一个类别值不值得追；连续 3 轮出现新的绕过类别时，回到威胁模型做一次判断，不要无限追下去。
