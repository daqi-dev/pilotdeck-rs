# UI server 原版多态登记（DRAFT · B2 第 3 个 PR · 第 6 轮）

沿用人已批准的阶段 C #35 方案 B / 1B：原版多态仍在守护分母内，每个被测捕获在**每个独立单元**内必须与 **TS 参考池某一确认类的全体成员**逐一比较通过。类别、成员和准入只用 TS 确定；不比较频率、不要求被测侧覆盖所有类，不从被测侧补类。此文登记 B2 的精确字段边界；不是阶段 C 冻结规约的修改，也不是本 B2 草稿已冻结的声明。

## 两个独立登记单元

场景 `chat`：两个固定 session 串行完成 mock 对话。连接为 `ws.chat.2#0` 与 `ws.chat.3#0`，均是对应 case 成功 `/ws` 握手后的第一条连接。事件按每个连接 **u2c 方向 projects_updated 的出现次数**定位，以下第一次/第二次从 1 计数；代码与诊断路径下标从 0 计数。

| 单元 | 精确语义路径 | 单元内约束 | 单元之间 |
|---|---|---|---|
| U1 | 两连接**第一次** `projects_updated.body.changeType` | 两个字符串必须同值，作为一个广播的联合取值定类 | 与 U2 分别定类、独立组合 |
| U2 | 两连接**第二次** `projects_updated.body.changedFile` | 两个字符串必须同值，作为一个广播的联合取值定类 | 与 U1 分别定类、独立组合 |

对 `K ∈ {ws.chat.2#0, ws.chat.3#0}`，U1 的 by-type 路径为 `ws.by_type."K".projects_updated[0].body.changeType`，U2 为 `ws.by_type."K".projects_updated[1].body.changedFile`。全序别名为 `ws."K"[i].body.changeType` / `ws."K"[j].body.changedFile`，其中 i/j 只由该连接的第一次/第二次下行该 type 定位。live3 实际物理下标：U1 两连接分别为 13/4，U2 为 18/18；实现不硬编码物理下标。左右相同物理下标的精确叶子差异才可进入类判定。每单元两个叶子、四个诊断别名，总计八个别名。

四叶子必须存在且为字符串，对应事件必须完整单帧 text / fin=true，by-type 与全序 body/方向一致；缺字段/null/分片或缺广播均不豁免。两连接混类也不能成为参考池类，即使混类值重复出现两遍仍拒收。**第一次 changedFile、第二次 changeType、全部 watchProvider、其他事件/字段/帧数/顺序继续严格比较**；既有时钟与 ID 归一规则不变。

## live3 20 遍 TS 池实测类别

第 5 轮联合登记前提有误，本轮按用户更正撤销。两个变化属于两次独立调度周期：第一次事件 changedFile 恒为 session 1 的 `.jsonl`，changeType 在 change/add 之间；第二次事件 changeType 恒为 add，changedFile 在 session 2 的两种路径之间。源码只保证同次广播的类型与路径同源，不保证两次广播之间相关。因此 U1/U2 可以独立组合，不要求组合本身曾在池中出现。

以下类别仅由 `_tmp/20261014-b2-live3-ref{1..20}` 确认。U2 文件名前缀为 `exp008-fixture/chats/web:33333333-3333-4333-8333-000000000002`；实现不硬编码这些字符串或成员。

| 单元 / 类编号 | 两连接同值 | 参考池成员 | 数量 |
|---|---|---|---:|
| U1 / 0 | `change` | 1,2,3,4,5,6,10,11,12,13,14,15,16,17,18,19,20 | 17 |
| U1 / 1 | `add` | 7,8,9 | 3 |
| U2 / 0 | 后缀 `.jsonl.projections.json` | 1,2,3,4,6,7,8,9,10,11,12,13,14,15,16,18,19,20 | 18 |
| U2 / 1 | 后缀 `.jsonl` | 5,17 | 2 |

两个单元各有 20 遍池、至少两类、每类至少两遍。池全部 190 对原始严格比较仍为 109 PASS / 81 FAIL，348 条诊断仅含八个登记别名：changeType 每别名 51 次，changedFile 每别名 36 次。按单元判定时仅把另一单元精确差异交给另一单元：U1 为 139 PASS / 51 FAIL，U2 为 154 PASS / 36 FAIL；各自类内全 PASS、类间全 FAIL。

归一后池没有登记外差异。原始体另有 36 个时钟叶子随采集变化：两连接的两次事件均有 projectListRevision、timestamp、projects[0].lastActivity，每个已出现 session 的 createdAt、created_at、lastActivity、updated_at（第一次 1 个 session，第二次 2 个）。这些只走第 3 轮既有时钟规则，类型和相对排名仍受比较，不属于本次多态单元。原始证据见 `_tmp/b2-round5-raw-events.json`；本轮完整矩阵及分类见 `_tmp/b2-round6-ruler.json` 的 TS_calibration。

## 基线源码依据

下列路径相对只读基线 `/Users/da/ws/pilotdeck-rs/resources/PilotDeck`（cc7c4e6）：

- `ui/server/index.js:186–201`：watch root 为 PILOT_HOME/projects；忽略项包括 `.tmp`，没有忽略 `.jsonl` 或 `.projections.json`。
- `src/session/storage/ProjectSessionStorage.ts:448–456`：projection 路径是 transcript 路径后追加 `.projections.json`，两者同目录。
- `src/session/persistence/JsonlSessionPersistence.ts:22–29`：appendFile 写日志；`src/session/projection/checkpoint/JsonFileSessionProjectionCheckpointStore.ts:36–44`：projection 写临时文件后 rename。两种目标都能形成 watcher add/change 通知。
- `ui/server/index.js:360–370`：ignoreInitial=true，稳定等待 100ms、轮询 50ms；`:373–378` 将文件和目录通知交给 scheduler，不按这两个文件名排序。
- `ui/server/projectUpdateScheduler.js:27–34`：每次 schedule 覆盖 pending 并重置 debounce；`:10–15` 整体取出当时通知，scan 后 publish；`:19–22` 处理扫描期间累积的下一次通知。因此是每个 debounce/scan 周期最终保留的通知决定该次广播，不是“第一个文件赢”。
- `ui/server/index.js:373–375,349–350`：类型、路径一起作为通知传入 scheduler；`:332–343` 解构同一通知，`:338` 写 changeType，`:339` 写 changedFile，`:340` 写 provider，`:342–344` 把同一个序列化消息发给所有 OPEN 客户端。这保留**每个单元内两连接同值**的要求；**不能用同次广播同源来推断两次周期的相关性**，因此不能继续约束成第 5 轮的三个联合组合。

## 准入与判法

1. 显式 `--reference-pool TS_DIR...`，至少 **20 遍独立、真实、完整停止**的 direct 捕获。两个单元各使用全池，不挑子集。目录 resolve 后不重复；run_id、recorder PID、原始 recording.jsonl SHA256 不重复。只改元数据的复制不能计数。各轮自比必须过完整健康门；拒绝 synthetic。
2. 池与计分 k=5/proxy 必须有相同完整 apparatus_sha256、source_sha256、字节相等的 fixture_path、同一 **UTC 日**。起止都落在该日，所有时间区间不重叠，身份与原始哈希不重复。UTC+8 每天 08:00 是日界；跨日重采，不能修改元数据补救。
3. run.py 起止记录 source_sha256：实际 Node、实际 recorder/proxy 共用二进制、TS src/dist/ui/server 全文件排序路径+内容摘要、根 package.json/pnpm-lock.yaml/ui/package.json。起止或轮次之间不同拒绝。依赖安装须保持不变；账本不声称枚举 node_modules 全部字节。旧 live2 不补账本。
4. 在读取任何计分 TS/proxy 内容前，先做全池 190 对原比较，再做两个单元各自的 190 对比较。**比较 U1 时仅转交 U2 精确路径差异；比较 U2 时仅转交 U1**；目标单元及全部未登记项、完整健康门、覆盖分母、SSE 时延、落盘内容继续严格比较。每单元独立确认类内全 PASS、类间全 FAIL、PASS 关系传递；至少两类、每类至少两遍。任一单元单例/单类/非传递即 **REFUSED**，不合并连通分量。
5. 池任意一对有全部八个精确别名之外的差异即 **REFUSED**；单元内两连接不同值也 REFUSED。不能把 HTTP、WS 顺序/帧数/其他字段、SSE 或落盘噪声吸收到类里。未新增归一。
6. 新 TS 每遍与 proxy 每遍均和池每位成员比较，保留完整严格报告，并按上述规则分别算 U1/U2 的全成员比较。**每个单元都必须完整命中该单元某一类的全部成员**；只有部分命中、单元内两连接混类或类外值均 **FAIL**。两个单元选中的类可以独立组合；例如 U1=add + U2=.jsonl 合法，即使它们的全体成员集合没有交集。最终配对只将八个别名交给单元判定，未登记差异继续 FAIL；原始登记差异保存在 registered_differences。
7. 报告 TS_calibration 保留全池身份/哈希和原始严格 190 对，`units.U1/U2` 各保留 classes、values、完整成员和各自 190 对。multimodal 各侧保留原始严格 comparisons，`units.U1/U2` 各保留 comparisons、matched_classes、class_matches（matched_members/unmatched_members）。两个单元均通过才接受捕获。独立评审仍 null，未创建 FROZEN.md。

compare.py：PASS/FAIL/REFUSED 退出码 0/1/2。check_ruler.py：当前装置的有效 k=5+proxy 全过仍为 PENDING_REVIEW/2，失败 FAIL/1，池拒收 REFUSED/2。无池仅可显式 `--paired-only` 做严格配对诊断，calibration_eligible=false；旧 `--replay-of` 仅历史重放，不与池同用。

## 对照与证据边界

controls/multimodal.py 使用合成 wire/HTTP/SSE/快照、20 成员和完整 190 对判定；只有身份层局部 mock，不改归档 metadata 冒充真实池，生产 CLI 无绕过开关。要求：

- U1、U2 单元内两连接取不同类 → FAIL；各单元类外值 → FAIL。
- **U1=add（全部 3 位）+ U2=.jsonl（全部 2 位）→ PASS**。合成池原始严格 20 次比较全部 FAIL，独立单元各完整命中，正好覆盖 TS2 类型组合。
- 第一次 changedFile、第二次 changeType、未知字段、缺失/null/错误类型/分片/视图不一致及捕获不健康 → FAIL。
- 部分成员用真实 SSE arrival：池内 0/100ms 都在 250ms 门内，被测 300ms 只命中偏移 100ms 的一位；U1 其余 16 位、U2 其余 17 位超界，两个单元均 FAIL。门限不变。

独立测试覆盖不足 20、复制捕获/同 ID、源/装置/路径变更、跨日/时间重叠、单例/单类/非传递与不合格池不得读取计分捕获，以及精确单元路径和广播同值。命令及结果见 `_tmp/RESULT.md`「第 6 轮」。

## 既有采集的明确离线重放

本轮不重采。规约与比较代码变化导致当前 apparatus_sha256 与 live3 采集时不同；默认入口仍拒收不匹配。沿用 `--replay-pool-of ORIGINAL_RULER.json`，必须同时给完整 reference-pool，不能与 paired-only 或旧 replay-of 同用。原报告可以因池差异为 REFUSED，但必须为 calibration 模式、k=5、完整有效装置哈希表。每个真实捕获仍逐字匹配该表，继续执行源/同日/身份/健康门；不改 metadata。

报告保留原报告路径/SHA256、新旧完整装置哈希、变化文件；所有重放配对 calibration_eligible=false、comparison_mode=historical-replay。ruler 全部通过时 REPLAY_ONLY/2，独立 compare 可 PASS/0，但均不是当前装置新采集定标。proxy 有任何登记外差异仍 FAIL。DRAFT、独立评审 null、不冻结不变；全池及全成员比较全部重算。
