# ui/server 已接受差异（DRAFT）

**尚无已接受的行为差异。** 本文件未冻结，未来每条追加必须写类别、完整字段路径、依据、批准者和对照；不得为让 DUT 变绿而临时加豁免。

当前仅有 normalization.md 中列出的录制/传输表示归一：精确白名单路径上的 ID、时钟和经过独立验证的随机凭证；连接 framing 头；合法 WS 随机 mask/challenge；SSE read 分块边界；SQLite 物理页布局。它们仍受独立完整性、引用关系、body/ETag/凭证验证等约束，不批准丢失确定字段或事件。

SSE added latency ≤250ms 与绝对事件到达 ≤45s 是待 TS k=5 定标、人批的草稿门限，见 normalization.md；不是冻结的宽松额度。SSE id、retry、event/data 全序，WS seq/逐帧全序，以及 Set-Cookie 同名多值始终是硬门。当前 StaffDeck 501 用例只证明无 host 分支，不能证明真实续传语义。

S2 coverage.json 里的 244 条首批豁免只是未执行路由的范围登记，不能转成行为等价或 Rust 接管许可。Rust 豁免须人工；登记 case 仍须实际由 Rust 执行。任何新的不稳定 TS 行为须在独立定标/评审阶段报告，不能在本次比较器中自动丢弃、聚合或排序使其通过。

## 第 2 轮 TS 多态候选登记：仅证据，不批准豁免

来源：五个独立真实 direct run，`_tmp/20261014-b2-live-ts{1..5}/direct/recording.jsonl`。下表都在同一 connection 的原始 downstream-read/downstream-write 全序上统计，包含双向 close；会话 1/2 是 driver 的两个固定输入。比较器仍然严格比较原数组/内容，没有启动“匹配任意已见变体”的接受逻辑。独立评审和批准者仍为 null。

| 候选 | 完整路径/规则 | TS1 / TS2 / TS3 / TS4 / TS5 证据 | 本轮处理 |
|---|---|---|---|
| 双会话 activity 广播交错 | `ws."ws.chat.{2,3}#0"[*].body.activity.{sessionId,runId,completedRunId}`；`ws.sequence`、`ws.structure` | activity 会话顺序两连接均为 `1212 / 2121 / 2112 / 1221 / 1221` | 驱动逐个发送；等 complete + 两连接同 run idle，再等其后的 project watcher 更新。旧数据不重排，仍 FAIL |
| 文件监听与 close 竞争 | `ws."ws.chat.2#0"` 长度及 `ws.by_type."ws.chat.2#0".{loading_progress,projects_updated}`；第二连接同理 | 连接 2 长度 `18/15/15/18/15`，连接 3 `18/16/15/18/15`；projects_updated 计数 `1/0/0/1/0`（两连接），第二连接 TS2 还收到 1 个 loading_progress | 显式等待 watcher，保留广播内容、changedFile、projectListRevision 与时间字段，不新增忽略/clock 豁免。新驱动尚待真实重跑 |
| 会话/快照列表次序 | `http[readonly.{projects,sessions}#0].body...sessions[*].id`；`ws."ws.watch.5#0"[*].body.activities[*]` | HTTP sessions 为 `21/12/12/12/12`；snapshot activity 为 `12/21/12/21/21` | 固定输入/落盘见证预分配实体记号，数组仍有序。串行驱动意在固定完成次序，旧列表翻转 FAIL |
| PTY 输出消息边界 | `ws."ws.watch.{6,7}#0"[*].body.data`、`ws.by_type."ws.watch.{6,7}#0".output[*].body.data` | 连接 6 output 数 `9/10/8/8/9`；连接 7 `17/18/16/16/17`。逐连接按原顺序拼接 data 的 SHA256 五遍相同：连接 6 前缀 `674bc1add2d092dc`，连接 7 `836d7a70ce6a5cc8` | 阶段 C 式“字节相同但合法分帧不同”候选；没有用拼接结果替代现有逐消息门，因此仍 FAIL。驱动改显式 prompt 屏障，但无法承诺内核 onData 分帧固定 |
| 原版 URL 提取顺序 | `ws.by_type."ws.watch.6#0".auth_url[*].body.url` | TS1/2/3/5 先 `https://support.apple.com/kb/HT208050` 后带 `.bash-3.2$` 的 URL；TS4 顺序相反，两值均实际发出 | 不能视为传输分块而丢掉；仍 FAIL。新 fixture 禁 bash rc、固定 prompt 并抑制 macOS deprecation banner，需实录确认效果 |

原版机制依据：`ui/server/index.js:326`–343 的 debounce 文件扫描、broadcast；`ui/server/index.js:2900` 起按 PTY onData 回调处理输出，2908 累积 URL buffer，2915–2936 提取/去重并发出 auth_url。这些不同输出在多个 TS run 中可复现，但“见过”本身不构成行为豁免。若后续要接受 PTY 合法分帧多态，必须单独定义阶段/顺序/字节完整性和删字节、重复、重排、跨 input/resize/close 屏障的差异对照，再经独立评审；本轮没有实施该放宽。

## 第 3 轮状态补充（DRAFT）

前述第 2 轮表格是当轮历史结论。本轮按用户明确指定的规则，已实施 `/shell` 方向内完整 output 字节流与非 output 序列比较，分块边界不作硬门；七条 watcher 时钟路径保留相对排名，细则与来源见 normalization.md 第 3 轮。未接受行为内容差异。普通 `/ws` 的全序、auth_url 内容与方向内顺序、resize 值/次数、changedFile 内容仍是硬门。

串行 live2 中仅剩第二次 watcher 的 `changedFile`：TS1/5 是第二会话 `.jsonl`，TS2/3/4 是对应 `.jsonl.projections.json`。六对各 4 条诊断（两个 chat 连接 × 全序/by_type），4/10 PASS；没有批准者或豁免，独立评审仍为 null。


## 第 4 轮：changedFile 精确多态登记

沿用人已批的 1B 原则，将 chat 两连接第二次 projects_updated.body.changedFile 登记于 [multimodal.md](multimodal.md)。它仍计入守护；只改该精确字段的类匹配流程，不删除字段或场景、不批准 watcher 内容豁免。历史 2/3 分组仅是依据；新池 >=20、每类 >=2、同源同 UTC 日且不与全新 live3 重叠，由 TS 独立重算类别，再要求每遍命中某类全体成员。此后以 multimodal.md 的完整准入条件为准，DRAFT 不变。

## 第 5 轮：20 遍池确认的联合路径集合

原始证据更正：变化是两连接第一次 `projects_updated.body.changeType` 与第二次 `projects_updated.body.changedFile`，不在同一帧。按 [multimodal.md](multimodal.md) 将四叶子作为一个联合单元；池仅确认 15/2/3 三类，不能逐字段取笛卡尔积。只新增第一次 changeType 的两个精确叶子，其余字段边界不变。既有 20+5+1 离线重放中 TS2 为池外第四组合，必须 FAIL；proxy 另有握手 Date/额外 close 差异，也保留 FAIL。没有从计分样本补类，没有新增其他行为豁免；DRAFT/独立评审 null 不变。

## 第 6 轮：更正为两个独立事件单元（替代第 5 轮联合约束）

按用户更正及 [multimodal.md](multimodal.md)，U1 为两连接第一次 projects_updated.changeType（17/3），U2 为第二次 projects_updated.changedFile（18/2）。两次调度周期分别定类，每单元池 >=20、每类 >=2、必须命中该类全体成员；同次广播两连接仍须同值。U1/U2 可独立组合，首次 add + 第二次 .jsonl 是合法正例，不要求该联合组合在池里出现。第 5 轮“第四组合必须 FAIL”的结论撤销。其他字段边界不变，proxy 的 Date/额外 close 仍未登记；DRAFT 与独立评审 null 不变。

## #59 / B3 前置审批规则更正（待定标）

前文「Rust 豁免须人工」由 acceptance.md 的静态清单与 FROZEN 同款 `Approved-By` / `Approval-Basis` 门落实：具名人或精确的「AI 代批，待人复核（协调者）」均需三份真实依据。旧的任意 `approver` 字符串不再能批准 Rust 接管豁免。这是 #59 明确授权的装置修复，不登记任何 TS/Rust 行为差异；已有用例的实际 Rust 处理证明仍须成立。合并后重新定标并由两家独立评审确认。

## PR #93 第 2 轮：Rust 豁免必须具名人批（待定标）

协调者裁定更正上一节：Rust 接管路由的豁免只接受具名人及三份真实依据；AI/代批/模型名/协调者、零宽或不可见字符均拒绝。FROZEN 冻结记录继续按 R5 接受精确的「AI 代批，待人复核（协调者）」及依据，两种用途由共用函数参数区分。未登记新的行为差异；具体硬门见 acceptance.md。

## PR #93 第 3 轮：Rust 豁免审批人白名单（待定标）

类别：审批装置修复；依据：Claude 第 2 轮 P1 与协调者本轮裁定。上一节的黑名单不能阻止全角、拆分角色或未列出的模型名，现由固定 `human-approvers.txt` 白名单替代，初始仅公开的仓库所有者 GitHub 用户名 `zhengdaqi`。审批人与白名单均经 NFKC + casefold + 去除全部空白及不可见字符，非空精确相等才通过；白名单外具名人和 AI 身份拒绝，`zhengdaqi` 零宽变体规范化后相等则预期通过。三份存在、realpath 互异且在允许范围内的依据仍是硬门。白名单进入装置清单与 FROZEN/capture 哈希，修改须评审；FROZEN 本身继续原 R5 审批规则。未登记行为差异，仍待重新定标与独立评审。

## #94 两条验收债与定标资源门（待定标）

类别：装置修复与资源准入对齐；依据：#94（PR #93 第 3 轮 P2）、B2-94 两家评审（GLM、DeepSeek）及确认轮、`state/decisions-20261017.md`「卡 · B2 定标装置磁盘上限对齐现行人定」。批准状态：AI 代批，待人复核（协调者）；定标报告与评审路径以 FROZEN.md 的 `Approval-Basis:` 为准。
1. **94.1**：`human-approvers.txt` 中的注释行、空行与首尾空白按 acceptance.md 规则处理；行内 `#`（含全角、小号＃）使整份白名单作废（fail-closed）。
2. **94.2**：`pnpm-workspace.yaml` 改为手写子集解析，不再依赖宿主 PyYAML（版本不在装置哈希内）。分隔只认空格和 tab；NBSP、U+3000 等 Unicode 空白属于标量内容，在本子集内一律拒绝。子集内接受的输入，结果与 PyYAML 6.0.3 的 safe_load 一致（单测逐例对照）；拒绝属于有意收紧。
3. **资源门**：`calibrate.py` 的 `CAP_BYTES` 从 10-14 冻结时的保守值 30 GB，对齐 CLAUDE.md 2026-10-17 人定的 80 GiB。这只是启动门，不进判分路径。

不登记任何 TS/Rust 行为差异；比较器与归一化规则未动。合并前用同日 20 遍参考池 + 5 遍计分 + 1 遍 proxy 重新定标，并由两家独立评审确认。
