# ui/server 归一化口径（DRAFT）

实现：`scripts/ui-server-parity/policy.py` 与 `compare.py`。以下是完整路径白名单；不按“同名 key”递归擦除，不全局替换 UUID/时间/PID/目录。`*` **只匹配一个数组下标**，不匹配对象任意 key；对象字面 key 含点号/星号时用 JSON 引号表示，不能冒充多段路径；缺字段不补值，null 保留，ID 的原始类型保留在映射标签中。对象键顺序无语义；数组与事件顺序有语义。

## WS 顺序与活动基准

录制全局 `.seq` 连续；同一 `.connection` 的 `.seq` 严格递增（可有其他连接/HTTP 插入造成的空洞）。seq 是写锁排序的证据，不直接比较两侧绝对数字。不要求不同 task 的 arrival_ns 相对 seq 单调，也不依据时钟重新排序帧。

选取 `ws_frame.leg=downstream-read` 的上行和 `downstream-write` 的下行作为客户端边界；握手 case + 同 case 出现序号建立连接对应，其他两条腿不作为额外应用事件。除下述第 3 轮 `/shell` 专用流规则外，逐帧比 dir/opcode/fin/rsv/masked 与内容，mask key 的随机字节不比，但 wire 必须正确解码且客户端上行 mask。比较该连接完整数组后，再按 JSON `.type`（没有时用 `.kind`）比结构与确定字段。无 type、非 JSON、binary、continuation、ping、pong、close 都按原始 payload 比；本草稿不对分片中的局部 JSON 作 volatile 归一；全序门之后再按同方向 continuation 重组完整消息做 type 结构比较。孤立 continuation、新 data 打断未结束消息、未结束分片皆 FAIL。

特殊活动字段仅限 `ws.session-activity.body.activity.revision`、`ws.session-activity-snapshot.body.activities[*].revision`。原值必须与**本侧带外** `(用户, 会话, revision)` 及完整活动内容一致，之后映射为该会话带外真实 revision 的有序序号。全局 revision 的数值空洞不减掉、不从一个用户的最大值估全局计数；snapshot 和 activity 同时被 DUT 加同一个常数仍会缺失带外成员，命中 activity.out-of-band。当前六场景在对话完成后才请求 watch snapshot，故它的完整列表（顺序也保留）须等于直连 after-scenarios anchor；未来新增活动并发期间 snapshot 用例必须先另写时间范围/见证口径。

## SSE 事件与延迟

从完整 response body 重组 UTF-8 流，支持 BOM、CRLF/CR/LF，按终止空行 dispatch。保留每个事件的 event（默认 message）、继承的 id、retry、所有 data 行（按换行连接）、未知扩展字段；注释不作为应用事件。不得丢事件、并事件、重新排序；末尾未结束行/事件失败，即使两侧截断相同也失败。id 是续传语义，不当随机字段擦掉。

事件到达时刻 = **该事件终止空行最后一个字节所在的 response chunk 的 arrival_ns**。每事件延迟 = 到达时刻 − 同一 http_request.arrival_ns，不使用两侧启动时钟绝对值。chunk 的 offset/时间负责定位事件到齐；chunk 数量/大小/边界不作等价硬门。一次 read 含两个独立事件仍拆成两个 dispatch。

草稿预注册上界：每事件绝对延迟 45,000 ms；proxy 第 i 个事件至多比 direct 第 i 个迟 250 ms。direct/direct 校准采用对称绝对差 ≤250 ms。未依据 DUT 结果调宽，尚待真实 TS k=5 与人批。整流缓冲对照保留完全相同字节，只把首事件的到达时刻移到尾事件，必须命中 sse.delay；差异对照的事件间隔超过此上界。短于 250 ms 的缓冲只能落在当前允许延迟内，不能据此宣称没有内部缓冲。

## HTTP 与派生头字段

HTTP 记录按 `.case + 第几次请求` 对应；无 case 的装置就绪探测按 method/path 独立编号。请求 method/path/query/body/headers/trailers 与响应 status/body/headers/trailers 都比。

- `http.response.headers.x-pilotdeck-handler[*]`：先验证与处理侧覆盖门，再剥离；没有这条规则就无法把 TS reference 与 Rust handler 比。
- `http.{request,response}.headers.{connection,keep-alive,transfer-encoding,content-length}[*]`：仅传输 framing，不比字符串；body 完整性和 Content-Length 实际字节长度必须成立。其他头名称不区分大小写，同名值保持有序列表，尤其 Set-Cookie 不折叠成 dict。请求 X-PilotDeck-Handler 与全部 trailers 保持 literal 多值列表，不套用响应头归一。
- `http.response.headers.date[*]`：必须能解析为 HTTP Date，替为时钟标记；请求同名头仍 literal。
- WS 101 的 `http.request.headers.sec-websocket-key[*]`：合法 16 字节 base64；`http.response.headers.sec-websocket-accept[*]` 必须等于本请求 challenge 的 SHA1/base64。验证后归一，其他握手头 literal。
- `http.response.headers.etag[*]`：同一 wire body 时完全 literal；不同 wire body 时各自必须是匹配原始长度/SHA1 的 Express ETag，再保留 strong/weak。不能给两个不同 body 挂同一个失效 ETag 就 PASS。
- 成功 `readonly.projects` 的 `http.response.headers.x-projects-revision[*]`：ASCII 正整数，处于本次独立采集的 run-meta wall_start_ms 到 wall_end_ms+1000 范围内才替时钟标记；其他路由同名头 literal。
- `http.request.path=/api/uploads/<uploadId>[/...]` 中 ID 段：upload 分类映射；`/api/sessions/<sessionId>/messages` 中 URL 解码后的 ID 段：session 映射。其他 path 与所有 query 项 literal。路径差异通过同一路径采集解决，禁止全局删路径。

ID 在同一侧、同一分类的所有白名单位置共用映射；跨侧标签必须有一致的重复/别名关系。`identity.consistency` 单列重复引用变化，同时原 channel 的差异也保留。数值 ID 与字符串 ID 不混为一谈。

真实 auth token 的 `http.{auth.register,auth.login}.response.body.token` 不盲擦随机字符串：先用本侧 auth.db 的 jwt_secret 验 HS256 签名，header=HS256/JWT，claims.userId/username 对应返回用户，iat 处于本轮 wall 区间，exp−iat=604800 秒。然后只归一 `.claims.iat` 与 `.claims.exp`，映射 `.claims.userId`，其他 header/claims literal。最小 synthetic 夹具可用 token 分类标签，但 check_ruler 禁止 synthetic 输入充当校准。

## 落盘格式与特殊路径

停自有服务后遍历 home/project，记录相对路径、kind、权限、数据；文件 mtime 不属于快照 schema。JSON/JSONL 结构化比，text literal，binary 比长度/SHA256，symlink 比目标，SQLite 比完整 schema/逻辑 rows 并 integrity_check。未知文件/列/字段保持 literal。

- `disk.entries[*].path=home/.tmp/chat-uploads/<uploadId>[/...]`：仅该确切目录中的 uploadId 段映射；对应 metadata.json 使用下表 @upload，`.attachments[*].path` 中固定 fixture_path 下的 `/home/.tmp/chat-uploads/<uploadId>/files/<name>` 使用同一映射。其余目录名/文件名不擦。
- @chat 仅指 `home/projects/exp008-fixture/chats/<session>.jsonl` 的完整数据数组；@projection 仅指同名 `.jsonl.projections.json`；@registry 仅指 `home/runs/registry.json`；@upload 仅指上述上传目录的 metadata.json。这些是文件 schema 选择器，不是任意文件通配豁免。
- `home/auth.db.data.tables.users.rows[*].password_hash`：只有 bcrypt cost=12、经已安装的 TS bcrypt 对录制注册密码验证成功，才替 VERIFIED 标记。
- `home/auth.db.data.tables.app_config.rows[*].value`：只有 jwt_secret 行对应合法 128 个十六进制字符的 64 字节 secret，在签名验证后替 VERIFIED；其他配置值 literal。
- `home/auth.db.data.tables.vapid_keys.rows[*].{public_key,private_key}`：Node 内置 ECDH 验证 P-256 私钥能导出该公钥后，才分别替 VERIFIED 标记。
- `@registry.records[*].ownerPid`：必须等于本侧 audit 的自有 gateway 进程 PID 后替 GATEWAY-PID:VERIFIED；其他 PID literal。
- `@registry.events` 的 key 结构固定为 `<projectKey>\0<sessionKey>\0<runId>`；必须有对应 registry.records 三元组。仅 key 的 session/run 段映射，project 段 literal；转成按映射 key 排序的 `{key,entries}` 数组后比较每条事件序列，不丢 events。

就绪探测 `_probe:GET:/health` 的 `body.instanceId` 按 instance 分类映射，`body.timestamp`/`body.startedAt` 归一时钟，`body.pid` 必须等于 audit 中自有 UI PID 才替 VERIFIED；其他 health 字段 literal。

上传 `http.{upload.create,upload.content,upload.read}.body.expiresAt`、`sse.upload.events.{upload_started,upload_progress,upload_completed}.data.expiresAt` 与 `@upload.expiresAt`：必须等于本侧真实 backend metadata.json 的原始 expiresAt，然后保留原类型，改为 expiresAt−createdAt 的 TTL 毫秒数。metadata 的 uploadId 须等于目录 ID，createdAt 须在本轮 wall_start/end 内，TTL 须正；不把改了一年的过期时间擦成 TIME。后端 metadata 和 wire 一起改也会保留 TTL 差异。

下表列出所有常规 ID/时间规则。时间只在所列位置按文末第 7 轮逐路径格式、单位、采集 wall 区间验证后替同类型 TIME 标记；已登记成对时钟另保留排名和先后约束；该规则不把确定的 TTL、计数、revision、大小、状态或未知 timestamp 擦掉。SQL created_at/last_login 使用数据库时间格式，null 保留。不新增基于被测 DUT 的路径。

| 通道/用例或文件 | 完整 data/body 字段路径 | 规则 |
|---|---|---|
| HTTP response `_probe:GET:/health` | `instanceId` | ID 分类 `instance` |
| HTTP response `_probe:GET:/health` | `startedAt` | 同类型 TIME |
| HTTP response `_probe:GET:/health` | `timestamp` | 同类型 TIME |
| HTTP response `auth.login` | `token` | ID 分类 `token` |
| HTTP response `auth.login` | `user.id` | ID 分类 `user` |
| HTTP response `auth.register` | `token` | ID 分类 `token` |
| HTTP response `auth.register` | `user.id` | ID 分类 `user` |
| HTTP response `auth.user` | `user.id` | ID 分类 `user` |
| HTTP response `readonly.messages` | `messages.*.entryId` | ID 分类 `entry` |
| HTTP response `readonly.messages` | `messages.*.id` | ID 分类 `entry` |
| HTTP response `readonly.messages` | `messages.*.runId` | ID 分类 `run` |
| HTTP response `readonly.messages` | `messages.*.sessionId` | ID 分类 `session` |
| HTTP response `readonly.messages` | `messages.*.turnId` | ID 分类 `turn` |
| HTTP response `readonly.messages` | `stream.messages.*.id` | ID 分类 `run` |
| HTTP response `readonly.messages` | `stream.messages.*.runId` | ID 分类 `run` |
| HTTP response `readonly.messages` | `stream.messages.*.sessionId` | ID 分类 `session` |
| HTTP response `readonly.messages` | `stream.runId` | ID 分类 `run` |
| HTTP response `readonly.messages` | `messages.*.timestamp` | 同类型 TIME |
| HTTP response `readonly.messages` | `stream.messages.*.timestamp` | 同类型 TIME |
| HTTP response `readonly.projects` | `*.sessions.*.id` | ID 分类 `session` |
| HTTP response `readonly.projects` | `*.lastActivity` | 同类型 TIME |
| HTTP response `readonly.projects` | `*.sessions.*.createdAt` | 同类型 TIME |
| HTTP response `readonly.projects` | `*.sessions.*.created_at` | 同类型 TIME |
| HTTP response `readonly.projects` | `*.sessions.*.lastActivity` | 同类型 TIME |
| HTTP response `readonly.projects` | `*.sessions.*.updated_at` | 同类型 TIME |
| HTTP response `readonly.sessions` | `sessions.*.id` | ID 分类 `session` |
| HTTP response `readonly.sessions` | `sessions.*.createdAt` | 同类型 TIME |
| HTTP response `readonly.sessions` | `sessions.*.created_at` | 同类型 TIME |
| HTTP response `readonly.sessions` | `sessions.*.lastActivity` | 同类型 TIME |
| HTTP response `readonly.sessions` | `sessions.*.updated_at` | 同类型 TIME |
| HTTP response `shadow.files` | `items.*.mtimeMs` | 同类型 TIME |
| HTTP response `upload.content` | `attachments.*.attachmentId` | ID 分类 `attachment` |
| HTTP response `upload.content` | `uploadId` | ID 分类 `upload` |
| HTTP response `upload.content` | `expiresAt` | 原类型 + 经 backend 见证的 TTL ms |
| HTTP response `upload.create` | `contentUrl` | ID 分类 `upload-url` |
| HTTP response `upload.create` | `eventsUrl` | ID 分类 `upload-url` |
| HTTP response `upload.create` | `uploadId` | ID 分类 `upload` |
| HTTP response `upload.create` | `expiresAt` | 原类型 + 经 backend 见证的 TTL ms |
| HTTP response `upload.read` | `attachments.*.attachmentId` | ID 分类 `attachment` |
| HTTP response `upload.read` | `uploadId` | ID 分类 `upload` |
| HTTP response `upload.read` | `expiresAt` | 原类型 + 经 backend 见证的 TTL ms |
| WS JSON `complete` | `blockId` | ID 分类 `block` |
| WS JSON `complete` | `id` | ID 分类 `run` |
| WS JSON `complete` | `runId` | ID 分类 `run` |
| WS JSON `complete` | `sessionId` | ID 分类 `session` |
| WS JSON `complete` | `timeline.id` | ID 分类 `timeline` |
| WS JSON `complete` | `timeline.turnId` | ID 分类 `turn` |
| WS JSON `complete` | `timestamp` | 同类型 TIME |
| WS JSON `config:reloaded` | `timestamp` | 同类型 TIME |
| WS JSON `input-queue-state` | `items.*.id` | ID 分类 `run` |
| WS JSON `input-queue-state` | `items.*.runId` | ID 分类 `run` |
| WS JSON `input-queue-state` | `sessionId` | ID 分类 `session` |
| WS JSON `model-selection-changed` | `runId` | ID 分类 `run` |
| WS JSON `model-selection-changed` | `sessionId` | ID 分类 `session` |
| WS JSON `pilotdeck-command` | `options.sessionId` | ID 分类 `session` |
| WS JSON `session-activity` | `activity.completedRunId` | ID 分类 `run` |
| WS JSON `session-activity` | `activity.runId` | ID 分类 `run` |
| WS JSON `session-activity` | `activity.sessionId` | ID 分类 `session` |
| WS JSON `session-activity-snapshot` | `activities.*.completedRunId` | ID 分类 `run` |
| WS JSON `session-activity-snapshot` | `activities.*.runId` | ID 分类 `run` |
| WS JSON `session-activity-snapshot` | `activities.*.sessionId` | ID 分类 `session` |
| WS JSON `session-input-accepted` | `runId` | ID 分类 `run` |
| WS JSON `session-input-accepted` | `sessionId` | ID 分类 `session` |
| WS JSON `status` | `id` | ID 分类 `run` |
| WS JSON `status` | `runId` | ID 分类 `run` |
| WS JSON `status` | `sessionId` | ID 分类 `session` |
| WS JSON `status` | `timestamp` | 同类型 TIME |
| WS JSON `stream_delta` | `blockId` | ID 分类 `block` |
| WS JSON `stream_delta` | `id` | ID 分类 `run` |
| WS JSON `stream_delta` | `runId` | ID 分类 `run` |
| WS JSON `stream_delta` | `sessionId` | ID 分类 `session` |
| WS JSON `stream_delta` | `timeline.id` | ID 分类 `timeline` |
| WS JSON `stream_delta` | `timeline.turnId` | ID 分类 `turn` |
| WS JSON `stream_delta` | `timestamp` | 同类型 TIME |
| WS JSON `text` | `blockId` | ID 分类 `block` |
| WS JSON `text` | `id` | ID 分类 `run` |
| WS JSON `text` | `runId` | ID 分类 `run` |
| WS JSON `text` | `sessionId` | ID 分类 `session` |
| WS JSON `text` | `timeline.id` | ID 分类 `timeline` |
| WS JSON `text` | `timeline.turnId` | ID 分类 `turn` |
| WS JSON `text` | `timestamp` | 同类型 TIME |
| WS JSON `unwatch-session` | `sessionId` | ID 分类 `session` |
| WS JSON `watch-session` | `sessionId` | ID 分类 `session` |
| DISK data `@chat` | `*.detail.runId` | ID 分类 `run` |
| DISK data `@chat` | `*.entryId` | ID 分类 `entry` |
| DISK data `@chat` | `*.message.content.*.blockId` | ID 分类 `block` |
| DISK data `@chat` | `*.message.content.*.timeline.id` | ID 分类 `timeline` |
| DISK data `@chat` | `*.message.content.*.timeline.turnId` | ID 分类 `turn` |
| DISK data `@chat` | `*.operationId` | ID 分类 `operation` |
| DISK data `@chat` | `*.parentEntryId` | ID 分类 `entry` |
| DISK data `@chat` | `*.result.finalMessage.content.*.blockId` | ID 分类 `block` |
| DISK data `@chat` | `*.result.finalMessage.content.*.timeline.id` | ID 分类 `timeline` |
| DISK data `@chat` | `*.result.finalMessage.content.*.timeline.turnId` | ID 分类 `turn` |
| DISK data `@chat` | `*.result.sessionId` | ID 分类 `session` |
| DISK data `@chat` | `*.result.turnId` | ID 分类 `turn` |
| DISK data `@chat` | `*.sessionId` | ID 分类 `session` |
| DISK data `@chat` | `*.turnId` | ID 分类 `turn` |
| DISK data `@chat` | `*.createdAt` | 同类型 TIME |
| DISK data `@chat` | `*.metadata.updatedAt` | 同类型 TIME |
| DISK data `@chat` | `*.result.completedAt` | 同类型 TIME |
| DISK data `@chat` | `*.result.startedAt` | 同类型 TIME |
| DISK data `@projection` | `anchor.entryId` | ID 分类 `entry` |
| DISK data `@projection` | `anchor.parentEntryId` | ID 分类 `entry` |
| DISK data `@projection` | `anchor.turnId` | ID 分类 `turn` |
| DISK data `@projection` | `projections."agent-transcript.conversation".state.completedTurnIds.*` | ID 分类 `turn` |
| DISK data `@projection` | `projections."agent-transcript.conversation".state.entries.*.entry.entryId` | ID 分类 `entry` |
| DISK data `@projection` | `projections."agent-transcript.conversation".state.entries.*.entry.message.content.*.blockId` | ID 分类 `block` |
| DISK data `@projection` | `projections."agent-transcript.conversation".state.entries.*.entry.message.content.*.timeline.id` | ID 分类 `timeline` |
| DISK data `@projection` | `projections."agent-transcript.conversation".state.entries.*.entry.message.content.*.timeline.turnId` | ID 分类 `turn` |
| DISK data `@projection` | `projections."agent-transcript.conversation".state.entries.*.entry.parentEntryId` | ID 分类 `entry` |
| DISK data `@projection` | `projections."agent-transcript.conversation".state.entries.*.entry.result.finalMessage.content.*.blockId` | ID 分类 `block` |
| DISK data `@projection` | `projections."agent-transcript.conversation".state.entries.*.entry.result.finalMessage.content.*.timeline.id` | ID 分类 `timeline` |
| DISK data `@projection` | `projections."agent-transcript.conversation".state.entries.*.entry.result.finalMessage.content.*.timeline.turnId` | ID 分类 `turn` |
| DISK data `@projection` | `projections."agent-transcript.conversation".state.entries.*.entry.result.sessionId` | ID 分类 `session` |
| DISK data `@projection` | `projections."agent-transcript.conversation".state.entries.*.entry.result.turnId` | ID 分类 `turn` |
| DISK data `@projection` | `projections."agent-transcript.conversation".state.entries.*.entry.sessionId` | ID 分类 `session` |
| DISK data `@projection` | `projections."agent-transcript.conversation".state.entries.*.entry.turnId` | ID 分类 `turn` |
| DISK data `@projection` | `projections."web-history.agent-statuses".state.*.detail.runId` | ID 分类 `run` |
| DISK data `@projection` | `projections."web-history.agent-statuses".state.*.entryId` | ID 分类 `entry` |
| DISK data `@projection` | `projections."web-history.agent-statuses".state.*.parentEntryId` | ID 分类 `entry` |
| DISK data `@projection` | `projections."web-history.agent-statuses".state.*.sessionId` | ID 分类 `session` |
| DISK data `@projection` | `projections."web-history.agent-statuses".state.*.turnId` | ID 分类 `turn` |
| DISK data `@projection` | `projections."web-history.incomplete-turns".state.completedTurnIds.*` | ID 分类 `turn` |
| DISK data `@projection` | `projections."web-history.incomplete-turns".state.durableMessageTurnIds.*` | ID 分类 `turn` |
| DISK data `@projection` | `sessionId` | ID 分类 `session` |
| DISK data `@projection` | `anchor.createdAt` | 同类型 TIME |
| DISK data `@projection` | `projections."agent-transcript.conversation".state.entries.*.entry.createdAt` | 同类型 TIME |
| DISK data `@projection` | `projections."agent-transcript.conversation".state.entries.*.entry.result.completedAt` | 同类型 TIME |
| DISK data `@projection` | `projections."agent-transcript.conversation".state.entries.*.entry.result.startedAt` | 同类型 TIME |
| DISK data `@projection` | `projections."agent-transcript.metadata".state.updatedAt` | 同类型 TIME |
| DISK data `@projection` | `projections."web-history.agent-statuses".state.*.createdAt` | 同类型 TIME |
| DISK data `@registry` | `events.*.entries.*.event.runId` | ID 分类 `run` |
| DISK data `@registry` | `records.*.result.runId` | ID 分类 `run` |
| DISK data `@registry` | `records.*.runId` | ID 分类 `run` |
| DISK data `@registry` | `records.*.sessionKey` | ID 分类 `session` |
| DISK data `@registry` | `records.*.acceptedAt` | 同类型 TIME |
| DISK data `@registry` | `records.*.updatedAt` | 同类型 TIME |
| DISK data `@upload` | `attachments.*.attachmentId` | ID 分类 `attachment` |
| DISK data `@upload` | `uploadId` | ID 分类 `upload` |
| DISK data `@upload` | `createdAt` | 同类型 TIME |
| DISK data `@upload` | `expiresAt` | 原类型 + 经 backend 见证的 TTL ms |
| DISK data `@upload` | `updatedAt` | 同类型 TIME |
| DISK data `home/auth.db` | `tables.users.rows.*.id` | ID 分类 `user` |
| DISK data `home/auth.db` | `tables.app_config.rows.*.created_at` | 同类型 TIME |
| DISK data `home/auth.db` | `tables.users.rows.*.created_at` | 同类型 TIME |
| DISK data `home/auth.db` | `tables.users.rows.*.last_login` | 同类型 TIME |
| DISK data `home/auth.db` | `tables.vapid_keys.rows.*.created_at` | 同类型 TIME |
| SSE `upload.events` 的 `upload_started/upload_progress/upload_completed` | `attachments.*.attachmentId` | ID 分类 `attachment` |
| SSE `upload.events` 的 `upload_started/upload_progress/upload_completed` | `uploadId` | ID 分类 `upload` |
| SSE `upload.events` 的 `upload_started/upload_progress/upload_completed` | `expiresAt` | 原类型 + 经 backend 见证的 TTL ms |

## 第 2 轮：真实 TS k=5 发现的 ID 引用规则（2026-10-14，仍为 DRAFT）

证据仅取 `_tmp/20261014-b2-live-ts{1..5}/direct` 和原 `20261014-b2-live-ruler.json` 的十对 differences；未使用 proxy 的差异制定规则。新增完整路径如下（`@registry.events` 指已验证原始三元组 key 后得到的 `{key,entries}` 数组）。

| 通道 / 上下文 | 新增完整 body/data 字段路径 | 处理 / 对照 | TS 生成及传递依据 |
|---|---|---|---|
| HTTP `readonly.messages` | `messages.*.blockId` | UUID 根映射，后缀 literal；collapse/suffix/kind 均 FAIL | A → B → D |
| HTTP `readonly.messages` | `messages.*.timeline.id` | 与 blockId 共用 UUID 根，后缀 literal；collapse/suffix/kind 均 FAIL | A → B → C → D |
| HTTP `readonly.messages` | `messages.*.timeline.turnId` | 与 runId/turnId 共用 UUID；collapse FAIL | E → C → D |
| HTTP `readonly.messages` | `stream.messages.*.blockId` | UUID 根映射，后缀 literal；collapse/suffix/kind 均 FAIL | A → B → F → G |
| HTTP `readonly.messages` | `stream.messages.*.timeline.id` | 与 blockId 共用 UUID 根，后缀 literal；collapse/suffix/kind 均 FAIL | A → B → C → F → G |
| HTTP `readonly.messages` | `stream.messages.*.timeline.turnId` | 与 runId/turnId 共用 UUID；collapse FAIL | E → C → F → G |
| DISK `@registry` | `events.*.entries.*.event.blockId` | UUID 根映射，后缀 literal；collapse/suffix/kind 均 FAIL | A → B → H |
| DISK `@registry` | `events.*.entries.*.event.timeline.id` | 与 blockId 共用 UUID 根，后缀 literal；collapse/suffix/kind 均 FAIL | A → B → C → H |
| DISK `@registry` | `events.*.entries.*.event.timeline.turnId` | 与 runId/turnId 共用 UUID；collapse FAIL | E → C → H |

基线路径均相对 `/Users/da/ws/pilotdeck-rs/resources/PilotDeck/`，本轮只读源文件：

- **A** `src/agent/loop/AgentLoop.ts:838`：`createModelMessageAssemblerState(randomUUID())`，模型响应的根确为随机 UUID。
- **B** `src/model/streaming/assembleModelMessage.ts:53`–54：用 responseId、kind、同类块计数构成 `${responseId}:${kind}:${index}`。只允许根重命名，种类和计数是语义。
- **C** `src/agent/stream/TurnTimeline.ts:19`、45–50、66–71：position 保存传入的 turnId/id，实时 delta 与持久化 block 复用同一坐标。
- **D** `ui/server/routes/messages.js:194`–202：历史响应保留 blockId/timeline/entryId，并把 turnId 同时暴露为 runId。
- **E** `ui/server/pilotdeck-bridge.js:2658`–2660：未传 runId 时 `randomUUID()`；驱动 command 不传 runId。
- **F** `ui/server/pilotdeck-bridge.js:1207`、1267–1279：gateway event 的 timeline/runId/blockId 原样进入 WS frame。
- **G** `ui/server/routes/messages.js:74`–75：stream.messages 从相同 gateway events 经 `toFrames` 构造。
- **H** `src/gateway/run/RunRegistry.ts:236`–249：event 经 `structuredClone` 追加并持久化，没有另造 block/timeline ID。

**共有映射的收紧。** 上表及原表中已白名单的 ID 字段，合法小写 UUID v4 全值统一进入一个 `uuid` 命名空间；不再让 `run`/`turn` 或 `block`/`timeline` 的独立编号掩盖引用断裂。同一 UUID 在 HTTP、WS、chat、projection、registry 中恒为同一记号，不同 UUID 的映射单射。只对 `block`/`timeline` 分类解析 `<uuid>:<kind>:<index>`，后缀逐字保留；未知非 UUID 块标识保留原值。`policy.py` 的 UUID 格式检查只在完整字段路径命中后执行，未白名单字段中的 UUID 仍逐字比较。

原有 `WS {status,stream_delta,text,complete}.id` 与 `HTTP readonly.messages.stream.messages.*.id` 的 `run` 分类映射也收紧：`{status,stream_delta,text,complete}_<uuid>` 保留前缀，只重命名 UUID。依据为 `ui/server/pilotdeck-message.js:17`–18、30 的 `generateMessageId(fields.kind)`。不能把消息种类前缀也擦掉。现有 chat/projection 的 entryId/parentEntryId 与 HTTP 历史 status.id 的 UUID 引用共用该空间；entryId 的随机生成依据是 `src/session/events/SessionRuntime.ts:42`、58–66。

**稳定分配次序不是排序豁免。** 两个输入 session 是驱动写死的 `web:33333333-3333-4333-8333-000000000001`、`...0002`。先按该固定输入次序分配会话记号，再读取各自 `home/projects/exp008-fixture/chats/<session>.jsonl` 中**既有 @chat 完整 ID 白名单**，按原事件数组顺序建立 UUID 字典。它只预先给实体命名，不排序被比较的 HTTP sessions、activity/snapshot、registry records 或 WS 数组。原 TS1 与 TS2 的会话列表翻转现在在列表本身报 FAIL，而不会把别处正确的同会话引用报为编号漂移。未知文件/路径不参与预分配；文件名、所有原始数组、事件内容仍比较。

对照入口 `controls/identity_paths.py` 逐个遍历 113 个实际 ID 路径，生成 153 条差异阳性对照：先在 DISK turnId 与 WS runId 建立两个独立实体见证；同一路径合法双射重命名 PASS，把第二实体指向第一实体 FAIL；20 个 block/timeline 路径另测后缀序号和种类变化 FAIL。新增九条各有自己的完整路径对照记录；这些是直接调用生产递归 dispatcher/diff 的路径级对照，原有 28 条端到端对照继续独立执行。`test_compare.py` 另有真实录制格式的 WS↔落盘重命名/引用合并对照、跨 HTTP/WS/DISK 命名空间对照、未知路径保留、固定 sessions 数组翻转/合并对照。

**重放与定标分离。** 默认 compare/check_ruler 仍要求归档 `apparatus_sha256` 等于当前代码/规约，不接受过期录制。显式 `check_ruler.py --replay-of <原 ruler.json>` 校验旧归档与原报告的完整装置哈希、五个 run ID/PID 及顺序，再执行当前比较器；输出原报告 SHA256、旧/新装置哈希与变化文件，`comparison_mode=historical-replay`、`calibration_eligible=false`。不改 run-meta、不改录制、不冒充 synthetic、不关闭其他完整性/带外/落盘验证。即使十对全过也只能 `REPLAY_ONLY`，新驱动仍必须重新真实采集 k=5。

## 第 3 轮：串行 live2 的 PTY 字节流与 watcher 时钟（2026-10-14，DRAFT）

本轮规则来自用户指定的 TS direct k=5：`_tmp/20261014-b2-live2-ts{1..5}/direct`；未用 proxy 选择规则。它替代上文的 `/shell` 逐帧硬门，其他 `/ws` 连接保持原全序、by_type 和内容比较。基线路径以下均相对 `/Users/da/ws/pilotdeck-rs/resources/PilotDeck/`，全部只读。

### PTY：连接内按方向比较，不丢输出字节

1. **范围与完整性。** 必须由成功且经过 challenge 校验的 HTTP 101 握手 `request.path == /shell` 选择，不能用 case 名或消息同名字段选择。live2 对应 `ws.watch.6#0`、`ws.watch.7#0`。仍先校验每条原始 WS wire、长度、FIN/RSV/opcode、方向、客户端 mask、连续全局 seq、continuation 链、双向正常 close 和 ws_end。WS 消息分片先按连接与方向重组；半条、孤立 continuation、坏 mask 仍 FAIL。
2. **输出流。** 只对上述连接中完整文本消息的 `body.type == output`、完整字段 `body.data` 应用：envelope 必须恰好含 `type` 与字符串 `data`，未知字段/类型失败，不能在聚合中丢掉。按本连接、同方向原 seq 次序，将每段 data 编码为 UTF-8 后拼成完整字节流，逐字节比较（报告用无损 base64 `ws.<connection>.<direction>.output_b64`）。ANSI、CR/LF、Unicode、echo、提示符、重连缓存与错误提示的内容全部保留。不 trim、不去重、不排序、不跨连接拼接。修改一个字节、少一段、多一段、同方向输出换序或方向变化均 FAIL。
3. **分块仅诊断。** output 消息数量、边界、各消息占多少个 WS wire frame 只写入 `pty_chunk_diagnostics.<side>.<connection>.<direction>[*].{bytes,end_offset,wire_frames}`，不进入硬门；原始录制完整保留。依据：`ui/node_modules/node-pty/lib/unixTerminal.js:92`–95 从 PTY 文件描述符建 `tty.ReadStream` 并设置文本解码；`lib/terminal.js:90`–93 将流的 data 回调转为 onData；`ui/server/index.js:2900`–2908、2949–2953 每次 onData 直接发一条 output。因此 OS 读取缓冲/调度决定 read 回调边界，进而决定这类应用 WS 消息的分块，不是应用输出记录边界。与 SSE chunk 元数据只负责诊断/到齐定位而不比较边界同理。重连依据 `ui/server/index.js:2726`–2737：提示和缓存段逐条发送，内容仍纳入重连连接的完整流。
4. **方向内的非 output 序列。** `resize` 每次的全部 body、值、次数、顺序和 frame 元数据仍保留在该方向 `frames` 序列中；init/input/auth_url/binary/ping/pong/close 等非 output 也全部保留，同方向相互顺序仍比。不比较两个方向之间的相对交错，也不以 output 分块在非 output 帧之间的插入位置作硬门。依据：`ui/server/index.js:2994`–2999 收到 resize 后调用 terminalSessions.resize；输出来自独立 onData 回调（2900）；客户端的上行 resize 与下行 output 没有统一应用序列号。resize 改值、少一次、多一次、换序均 FAIL。`auth_url` 两条换序仍 FAIL，未知 `/ws` 连接的 output 分块仍按旧硬门比较。

这是输出字节流与方向序列的等价规则，不声称验证每段输出相对 input/resize 的因果延迟。两侧各自的原始生命周期完整性仍独立验证，不能以跨方向不比为由接受缺 close 或坏 wire。

### Watcher：完整字段路径与相对时序

新增上下文仅为 WS `body.type == projects_updated`，通过完整路径 lookup 归一以下七条；没有同名 key 全局擦除。

| 完整 body 字段路径 | 时钟来源与基线路径:行号 | 归一后保留 |
|---|---|---|
| `projects.*.sessions.*.createdAt` | `ui/server/projects.js:106`–108，session.createdAt 或 lastModified/Date.now 转 ISO | 原类型、null/缺字段、同广播时间排名 |
| `projects.*.sessions.*.created_at` | `ui/server/projects.js:109`–111，同一创建时钟的兼容别名 | 同上，保留与 createdAt 的等值关系 |
| `projects.*.sessions.*.lastActivity` | `ui/server/projects.js:115`–117，session.lastModified 转 ISO 或 null | 同上，保留创建≤活动关系 |
| `projects.*.sessions.*.updated_at` | `ui/server/projects.js:112`–114，同一修改时钟的兼容别名 | 同上，保留与 lastActivity 的等值关系 |
| `projects.*.lastActivity` | `ui/server/projects.js:247`–250、263–270，取会话最新活动，空项目使用创建时间 | 同上，保留与会话时钟的大小/相等关系 |
| `projectListRevision` | `ui/server/projects.js:68`–79，`Math.max(Date.now(), previous + 1)`；`index.js:336` 透传 | 数值类型、同广播排名；不当活动 revision 计数归一 |
| `timestamp` | `ui/server/index.js:337`，广播时 `new Date().toISOString()` | 字符串类型、同广播排名 |

虽然用户重点列出四个 session 字段，逐对原 differences 也显示后三个完整路径上的真实时钟差异；它们均有直接生成依据，按相同口径列入，未忽略任何非时钟字段。

实现对每条广播中命中上述完整路径的非 null 时间解析为同一 UTC 毫秒轴，使用排序后**不同时间值**的 ordinal 替代绝对墙钟，每个原字段替换为 `{clock: <TIME:原类型>, order: ordinal}`。只对时钟值建排名，不排序任何项目/会话/消息数组，不改变字段集合。这样保留每个实体内以及该广播跨实体的 `< / = / >` 关系，包括同毫秒相等、别名相等。每侧另独立拒绝同 session 的任一创建时钟晚于任一活动/修改时钟，因此两侧同时交换也 FAIL。非法值（含缺时区的 watcher 字符串）FAIL；null 保留，缺字段不补。未知上下文/路径仍 literal。

跨实体顺序有实际语义：`src/session/storage/SessionList.ts:46` 按 lastModified 降序排列会话，`ui/server/projects.js:377`–379 按项目 lastActivity 降序排列项目。因此本规则保留整个广播的时钟偏序和原列表顺序；跨实体交换整套时间，即使各实体仍 created≤lastActivity，也 FAIL。持续时间没有新设阈值；本轮不更改其他既有 HTTP/DISK 时钟规则或 SSE 延迟界限。

### 对照与剩余字段

`controls/streams_and_clocks.py` 使用完整 schema-1 双侧归档，经生产 `compare_sides`、wire 解码和递归 dispatcher 执行 27 条端到端对照（2 条等价 PASS、25 条差异/同坏 FAIL）。覆盖重新分块/双向交错/Unicode/ANSI、output 的 WS continuation 重组，以及上述输出、resize、auth_url、mask、未知 envelope、同实体/跨实体时序、等值、类型、null/缺失、未知字段和 changedFile。与既有 181 条合并为 208 条；这些仍不是新真实采集或独立评审。

`projects_updated.changedFile` 保持 literal。live2 TS1/TS5 第二次广播为第二会话 `.jsonl`，TS2/TS3/TS4 为 `.jsonl.projections.json`。本轮没有批准把它删除、折叠或映射；来源 `ui/server/index.js:332`–340 将 debounce 调度器给出的 filePath 原样转相对路径。剩余诊断如实 FAIL。

分片边界对照另验证：同方向 ping 从一个非 output 分片消息中间移到它之前必须 FAIL；非 output 使用原始帧序列，不按消息完成顺序重新排列控制帧。


## 第 7 轮：严格时钟表示、采集区间与 HTTP / 磁盘成对时序（2026-10-14，DRAFT）

本节收紧并取代上文“同类型 TIME”的宽松解释，也将第 3 轮 watcher 排名推广至 HTTP `readonly.projects`、`readonly.sessions` 和下列成对路径。不能用 `fromisoformat` 能解析或数值落在 0～2^53 作为时间合法的充分条件。`policy.CLOCK_FORMATS` 按通道、上下文、完整字段路径登记；新增时钟白名单没有对应格式时导入即失败。以下基线路径均相对 `/Users/da/ws/pilotdeck-rs/resources/PilotDeck/`，仅只读回源，无 DUT 推导。

- **ISO ms UTC**：字符串必须完整匹配 `[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}\.[0-9]{3}Z`，并通过真实日历解析。空格分隔、缺时区、`+00:00`、缺毫秒、额外精度、非法日期及数值替代均拒绝。对应 JS `toISOString()`，精度单位 ms。
- **SQL s UTC**：字符串必须完整匹配 `[0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2}:[0-9]{2}`，通过真实日历解析后按 UTC 解释。SQLite `CURRENT_TIMESTAMP` 的秒精度是此路径的生产格式，不允许 ISO 字符串替代。精度单位 s。
- **epoch ms**：有限、正、低于 2^53 的 JSON 数值且值为整数；bool 不算数值。**mtime ms** 单独允许有限小数，以保留 Node `stat.mtimeMs` 的亚毫秒精度。当前白名单没有 epoch 秒数路径，绝不按数值大小自动猜测 ms/s。
- **本次 wall 范围**：所有常规时钟转为 UTC 毫秒后，必须在本侧 `run-meta.wall_start_ms ≤ t ≤ wall_end_ms + 1000` 内，和 `x-projects-revision` 相同。SQL 的下界先按源精度向下截到整秒；这是 CURRENT_TIMESTAMP 的表示精度，不是额外时间容差。ISO 和数值不截断下界。缺失/null 仍按原 schema 保留，未知路径 literal。
- **到期时间是明确例外**：`expiresAt` 本来在未来，不能要求它处于采集区间。所有已登记 `expiresAt` 必须先满足 ISO ms UTC，再和本侧 backend metadata 原值严格相等并保留 TTL；backend `createdAt` 严格限制在采集 start/end，`updatedAt` 使用常规 wall 门。后端和 wire 同改 expiry 仍暴露 TTL 差异。没有增加新的 expiry 豁免。

### 每条时钟路径的基线格式与生成依据

下表每行均对应 `CLOCK_FORMATS` 中一条完整路径。投影名字中的引号表示含点号的字面对象 key。

| 通道 / 上下文 | 完整 body/data 路径 | 格式 / 单位 | 基线生成处与传递处（路径:行号） |
|---|---|---|---|
| `disk / @chat` | `*.createdAt` | ISO ms UTC | `src/session/events/SessionRuntime.ts:57–65（this.now().toISOString）` |
| `disk / @chat` | `*.metadata.updatedAt` | ISO ms UTC | `src/session/metadata/SessionMetadataStore.ts:23,37,46,51,58,62,92` |
| `disk / @chat` | `*.result.completedAt` | ISO ms UTC | `src/agent/loop/AgentLoop.ts:3036（this.now().toISOString）` |
| `disk / @chat` | `*.result.startedAt` | ISO ms UTC | `src/agent/loop/AgentLoop.ts:366（this.now().toISOString）` |
| `disk / @projection` | `anchor.createdAt` | ISO ms UTC | `src/session/events/SessionRuntime.ts:57–65（this.now().toISOString） → src/session/projection/checkpoint/SessionProjectionCheckpointStore.ts:127` |
| `disk / @projection` | `projections."agent-transcript.conversation".state.entries.*.entry.createdAt` | ISO ms UTC | `src/session/events/SessionRuntime.ts:57–65（this.now().toISOString） → src/session/projection/AgentTranscriptProjections.ts:94,102,322（原 entry / metadata 投影）` |
| `disk / @projection` | `projections."agent-transcript.conversation".state.entries.*.entry.result.completedAt` | ISO ms UTC | `src/agent/loop/AgentLoop.ts:3036（this.now().toISOString） → src/session/projection/AgentTranscriptProjections.ts:94,102,322（原 entry / metadata 投影）` |
| `disk / @projection` | `projections."agent-transcript.conversation".state.entries.*.entry.result.startedAt` | ISO ms UTC | `src/agent/loop/AgentLoop.ts:366（this.now().toISOString） → src/session/projection/AgentTranscriptProjections.ts:94,102,322（原 entry / metadata 投影）` |
| `disk / @projection` | `projections."agent-transcript.metadata".state.updatedAt` | ISO ms UTC | `src/session/metadata/SessionMetadataStore.ts:23,37,46,51,58,62,92 → src/session/projection/AgentTranscriptProjections.ts:94,102,322（原 entry / metadata 投影）` |
| `disk / @projection` | `projections."web-history.agent-statuses".state.*.createdAt` | ISO ms UTC | `src/session/events/SessionRuntime.ts:57–65（this.now().toISOString） → src/session/projection/WebHistoryProjections.ts:124–126` |
| `disk / @registry` | `records.*.acceptedAt` | ISO ms UTC | `src/gateway/run/RunRegistry.ts:220–228` |
| `disk / @registry` | `records.*.updatedAt` | ISO ms UTC | `src/gateway/run/RunRegistry.ts:228,252,296` |
| `disk / @upload` | `createdAt` | ISO ms UTC | `src/gateway/dialog/UploadStore.ts:86` |
| `disk / @upload` | `expiresAt` | ISO ms UTC | `src/gateway/dialog/UploadStore.ts:88` |
| `disk / @upload` | `updatedAt` | ISO ms UTC | `src/gateway/dialog/UploadStore.ts:87,324` |
| `disk / home/auth.db` | `tables.app_config.rows.*.created_at` | SQL s UTC | `ui/server/database/db.js:68（CURRENT_TIMESTAMP）` |
| `disk / home/auth.db` | `tables.users.rows.*.created_at` | SQL s UTC | `ui/server/database/init.sql:9（CURRENT_TIMESTAMP）` |
| `disk / home/auth.db` | `tables.users.rows.*.last_login` | SQL s UTC | `ui/server/database/db.js:207（CURRENT_TIMESTAMP）` |
| `disk / home/auth.db` | `tables.vapid_keys.rows.*.created_at` | SQL s UTC | `ui/server/database/init.sql:67（CURRENT_TIMESTAMP）` |
| `http / _probe:GET:/health` | `startedAt` | ISO ms UTC | `ui/server/index.js:23 → :400 → :534` |
| `http / _probe:GET:/health` | `timestamp` | ISO ms UTC | `ui/server/index.js:531` |
| `http / readonly.messages` | `messages.*.timestamp` | ISO ms UTC | `src/session/events/SessionRuntime.ts:57–65 → ui/server/routes/messages.js:197` |
| `http / readonly.messages` | `stream.messages.*.timestamp` | ISO ms UTC | `ui/server/pilotdeck-message.js:32 → ui/server/pilotdeck-bridge.js:1267–1279 → ui/server/routes/messages.js:74–75` |
| `http / readonly.projects` | `*.lastActivity` | epoch ms | `ui/server/projects.js:247–250,266–270` |
| `http / readonly.projects` | `*.sessions.*.createdAt` | ISO ms UTC | `ui/server/projects.js:106–108；HTTP projects 映射 :244，HTTP sessions 映射 :411，WS index.js:335` |
| `http / readonly.projects` | `*.sessions.*.created_at` | ISO ms UTC | `ui/server/projects.js:109–111；HTTP projects 映射 :244，HTTP sessions 映射 :411，WS index.js:335` |
| `http / readonly.projects` | `*.sessions.*.lastActivity` | ISO ms UTC | `ui/server/projects.js:115–117；HTTP projects 映射 :244，HTTP sessions 映射 :411，WS index.js:335` |
| `http / readonly.projects` | `*.sessions.*.updated_at` | ISO ms UTC | `ui/server/projects.js:112–114；HTTP projects 映射 :244，HTTP sessions 映射 :411，WS index.js:335` |
| `http / readonly.sessions` | `sessions.*.createdAt` | ISO ms UTC | `ui/server/projects.js:106–108；HTTP projects 映射 :244，HTTP sessions 映射 :411，WS index.js:335` |
| `http / readonly.sessions` | `sessions.*.created_at` | ISO ms UTC | `ui/server/projects.js:109–111；HTTP projects 映射 :244，HTTP sessions 映射 :411，WS index.js:335` |
| `http / readonly.sessions` | `sessions.*.lastActivity` | ISO ms UTC | `ui/server/projects.js:115–117；HTTP projects 映射 :244，HTTP sessions 映射 :411，WS index.js:335` |
| `http / readonly.sessions` | `sessions.*.updated_at` | ISO ms UTC | `ui/server/projects.js:112–114；HTTP projects 映射 :244，HTTP sessions 映射 :411，WS index.js:335` |
| `http / shadow.files` | `items.*.mtimeMs` | mtime ms（允许小数） | `src/gateway/dialog/projectFiles.ts:99（info.mtimeMs）` |
| `http / upload.content` | `expiresAt` | ISO ms UTC | `src/gateway/dialog/UploadStore.ts:88 → ui/server/routes/uploads.js:209` |
| `http / upload.create` | `expiresAt` | ISO ms UTC | `src/gateway/dialog/UploadStore.ts:88 → ui/server/routes/uploads.js:209` |
| `http / upload.read` | `expiresAt` | ISO ms UTC | `src/gateway/dialog/UploadStore.ts:88 → ui/server/routes/uploads.js:209` |
| `sse / upload.events:upload_completed` | `expiresAt` | ISO ms UTC | `src/gateway/dialog/UploadStore.ts:88 → ui/server/routes/uploads.js:209 → :217（SSE）` |
| `sse / upload.events:upload_progress` | `expiresAt` | ISO ms UTC | `src/gateway/dialog/UploadStore.ts:88 → ui/server/routes/uploads.js:209 → :217（SSE）` |
| `sse / upload.events:upload_started` | `expiresAt` | ISO ms UTC | `src/gateway/dialog/UploadStore.ts:88 → ui/server/routes/uploads.js:209 → :217（SSE）` |
| `ws / complete` | `timestamp` | ISO ms UTC | `ui/server/pilotdeck-message.js:32（toISOString 默认值）→ ui/server/pilotdeck-bridge.js:1267–1279` |
| `ws / config:reloaded` | `timestamp` | ISO ms UTC | `ui/server/services/pilotdeckConfigWatcher.js:58,67,78,91,99；ui/server/routes/config.js:527,756,995,1038 → ui/server/index.js:299` |
| `ws / projects_updated` | `projectListRevision` | epoch ms | `ui/server/projects.js:68–79 → ui/server/index.js:336` |
| `ws / projects_updated` | `projects.*.lastActivity` | epoch ms | `ui/server/projects.js:247–250,266–270` |
| `ws / projects_updated` | `projects.*.sessions.*.createdAt` | ISO ms UTC | `ui/server/projects.js:106–108；HTTP projects 映射 :244，HTTP sessions 映射 :411，WS index.js:335` |
| `ws / projects_updated` | `projects.*.sessions.*.created_at` | ISO ms UTC | `ui/server/projects.js:109–111；HTTP projects 映射 :244，HTTP sessions 映射 :411，WS index.js:335` |
| `ws / projects_updated` | `projects.*.sessions.*.lastActivity` | ISO ms UTC | `ui/server/projects.js:115–117；HTTP projects 映射 :244，HTTP sessions 映射 :411，WS index.js:335` |
| `ws / projects_updated` | `projects.*.sessions.*.updated_at` | ISO ms UTC | `ui/server/projects.js:112–114；HTTP projects 映射 :244，HTTP sessions 映射 :411，WS index.js:335` |
| `ws / projects_updated` | `timestamp` | ISO ms UTC | `ui/server/index.js:337` |
| `ws / status` | `timestamp` | ISO ms UTC | `ui/server/pilotdeck-message.js:32（toISOString 默认值）→ ui/server/pilotdeck-bridge.js:1267–1279` |
| `ws / stream_delta` | `timestamp` | ISO ms UTC | `ui/server/pilotdeck-message.js:32（toISOString 默认值）→ ui/server/pilotdeck-bridge.js:1267–1279` |
| `ws / text` | `timestamp` | ISO ms UTC | `ui/server/pilotdeck-message.js:32（toISOString 默认值）→ ui/server/pilotdeck-bridge.js:1267–1279` |


### 排名与独立先后校验

HTTP `readonly.projects`、`readonly.sessions` 与 WS `projects_updated` 均在整个响应/广播内，对所有已登记、非 null、校验成功的时钟建立同一 UTC 毫秒轴的不同值排名；在原字段保留 `{clock: <TIME:原类型>, order: ordinal}`。该排名包括跨会话顺序、等值、兼容别名、项目数值时钟与会话字符串时钟的关系。只替换叶子，不排序项目/会话列表、不增删字段。任一同会话 `createdAt/created_at > lastActivity/updated_at` 独立命中 `normalization.field`，即使两侧同坏也拒绝。

其他已登记毫秒语义对在各自完整对象路径内保留排名/等值，并独立执行左列 ≤ 右列；SQL 秒精度对见下方量化说明：

| 通道 / 上下文 | 完整对象路径 | 先 ≤ 后 |
|---|---|---|
| HTTP `_probe:GET:/health` | 根 | `startedAt ≤ timestamp` |
| DISK `@upload` | 根 | `createdAt ≤ updatedAt` |
| DISK `@chat` | `*.result` | `startedAt ≤ completedAt` |
| DISK `@projection` | `projections."agent-transcript.conversation".state.entries.*.entry.result` | `startedAt ≤ completedAt` |
| DISK `@registry` | `records.*` | `acceptedAt ≤ updatedAt` |
| DISK `home/auth.db` | `tables.users.rows.*` | `created_at ≤ last_login` |

chat 的事件 `createdAt` 是写日志时间，`metadata.updatedAt` 是此前构造 metadata 的时间，不把它们误当作实体创建/活动对；result 的 started/completed 才是生命周期对。也不拿所有日志时钟跨事件做无依据的全局排名。上述语义对的每条路径都有同坏反转单测。

第 7 轮对照在既有 `controls/streams_and_clocks.py` 增加 HTTP projects/sessions 等价、格式、单位、互换、超区间、跨实体、别名排名及同坏双侧用例；每次重写响应都同步 Express ETag、Content-Length 和 recorder 字节/offset。格式、单位、互换、超区间必须命中 `normalization.field`；顺序/等值改变命中 `http.body`。原 synthetic fixture 的上传与 watcher 时间也按回源格式修为 ISO Z，不能靠放宽生产解析保住旧合成样本。


**SQL 源精度复核。** `users.created_at`（init.sql:9）与 `last_login`（db.js:207）来自两次独立的 `CURRENT_TIMESTAMP`，不是同一时钟的兼容别名。live3 ref1/ref2 分别出现跨秒/同秒；将这些量化结果的“等于/小于”当成跨采集必须相等的排名会误报。该精确 SQL 对只执行严格秒格式、按秒量化的 wall 门和 `created_at ≤ last_login`，不把量化相等当成别名等值硬门。反转仍独立 FAIL，两侧同坏同样 FAIL。HTTP/WS 的 ISO 毫秒字段、别名与跨实体排名不受此条影响。此边界回源于两个 SQL 生成位置，未放宽任何表示/单位/区间检查，未新增允许格式。

## 第 9 轮：消息、日志与 projection 时间见证（2026-10-14，DRAFT）

**协调者裁定：自决·待复核。** 第 8 轮反例仍成立：`SessionRuntime.ts:41,57–65,172–179` 使用 wall clock，先取时间再串行 enqueue；`JsonlSessionPersistence.ts:20–35` 保证追加顺序，`SessionEventLogValidation.ts:27–77,97–133` 不保证 wall 时间单调。本节明确增加环境前提 **“采集期间 wall clock 不回拨”**，不把它改写为源码无条件承诺。适用域是本尺子的完成态固定场景；DRAFT、独立评审待完成、未冻结状态不变。

### 环境准入：回拨是 REFUSED，不计分

最小改动放在 B2 `run.py:record_side`，共用实现位于 `common.py:ClockMonitor/validate_clock_check`。监测从创建 fixture 之前开始，覆盖服务、driver、退出清理、磁盘快照与采集后源码哈希检查；退出时将完整记录写入该侧 `run-meta.json.clock_regression_check`。无论采集成功或抛错均结束监测、保存元数据；若时钟检查不满足，整个采集标为 **REFUSED / exit 2**，不把环境故障作为 DUT FAIL。

- schema 1 记录 `interval_ns=250000000`（250 ms 定期采样）、`max_gap_ns=2000000000`（2 s 最大可接受覆盖间隔）、`status`、`samples`，失败另有 `reason`。首尾样本分别为 `start/end`，中间样本为 `periodic`。
- 每次按 `monotonic_ns → time_ns → monotonic_ns` 取样，记录 `mono_before_ns/wall_ns/mono_after_ns`；使用 bracket 的 offset 区间 `[wall-after, wall-before]`，避免把两次读时钟的正常调用延迟直接当回拨。
- 任一样本 wall 值小于上一样本，或本样本 offset 上界小于此前 offset 下界的最高值，均拒绝；后者能捕获“wall 仍向前，但比 monotonic 少走一截”的回拨。无额外时间容差，采样 bracket 重叠仍可通过。此门采取保守策略：持续负向时钟校正也可能被拒绝，不给 DUT 计错。
- monotonic 顺序错误、样本格式错误、采样 bracket/相邻覆盖间隔超过 2 s、缺首尾或 wall 起止与 metadata 不匹配，同样 REFUSED。比较器重算所有样本，不仅信任写入的 `status=PASS`；已标 REFUSED 的记录不能恢复成有效采集。
- 有限频率的采样不能证明两个采样点之间从未发生过立即回拨又恢复的瞬态。记录的含义是“环境前提的采样监测通过”，不是源码单调保证或连续时钟证明；不对这种监测盲区作额外保证。

**旧归档政策。** 默认采集/比较及 TS 池准入必须有合法监测记录（synthetic 测试也提供记录，没有生产旁路）。只有现有显式 `historical-replay` 身份绑定允许缺失：两侧报告标注 `clock_regression_check: {left: "absent", right: "absent"}`，池的每份 capture 也标注 `clock_regression_check="absent"`；`calibration_eligible=false`。已存在的坏记录在 historical-replay 也必须 REFUSED。原报告 / apparatus 哈希绑定不改，不能用 replay 缺记录例外给新装置定标。旧档的消息、日志非递减检查仍照常执行；通过只说明该批时间值满足裁定，不能补造当年的监测证据。

### 本侧独立校验（两侧同坏亦 FAIL）

所有见证来自 `capture()` 重新读取的**本侧真实磁盘**，不是 `snapshot.json` 缓存，也不是对侧值。既有归档快照一致性检查保留。以下规则先于时钟叶子的 `<TIME:type>` 替换执行，失败为 `normalization.field`，诊断包含侧别、路径和具体契约。

| 路径 | 额外约束 | 回源依据（路径相对 baseline） |
|---|---|---|
| `http / readonly.messages / messages.*.timestamp` | 每个历史消息时间必须原值等于本侧 `@chat` 某事件 `createdAt`；响应消息顺序非递减，允许相等 | `src/web/server/readSessionMessages.ts:96–115,529,866–887` 从持久化 entry 取时间并 flatten；`ui/server/routes/messages.js:197` 原样映射 |
| `disk / @chat / *.createdAt` | 每个 jsonl 独立按 sequence 非递减；sequence 为正整数且在文件内严格递增，不重排文件内容 | `SessionRuntime.ts:57–65,172–179` / `JsonlSessionPersistence.ts:20–35`，加上上述显式环境前提 |
| `disk / @projection / anchor.createdAt` | 在对应 jsonl 中，按 `asOfSequence` 与 anchor 的 sequence、entryId、parentEntryId、turnId、type 找到唯一事件，并严格相等 | `src/session/projection/checkpoint/SessionProjectionCheckpointStore.ts:110–127` |
| `disk / @projection / projections."agent-transcript.conversation".state.entries.*.entry.createdAt` | 按 sequence、entryId、parentEntryId、turnId、type、sessionId 找到对应日志唯一事件，严格相等；不能晚于该 projection 的 asOfSequence 或顶层 checkpoint | `src/session/projection/AgentTranscriptProjections.ts:94–103` 保留原 entry |
| `disk / @projection / projections."web-history.agent-statuses".state.*.createdAt` | 同上按事件身份见证，不能拿任意另一个合法日志时间替代 | `src/session/projection/WebHistoryProjections.ts:124–126` 保留原 entry |
| `disk / @projection / projections."agent-transcript.metadata".state.updatedAt` | 等于截至该 projection 的 asOfSequence，日志 `session_metadata.metadata.updatedAt` 最后一次对应赋值；不是仅做集合成员判断 | `src/session/projection/AgentTranscriptProjections.ts:322,577–584` 按 sequence merge metadata；accepted_input 分支只合入 modelSelection |

消息与日志均**不新增跨采集固定排名**；同毫秒并列可以随采集变化。messages 见证按本侧事件时间集合，不升级为未裁定的每消息身份映射。projection 见证按对应文件和事件身份，拒绝无对应日志文件的 projection。

**incomplete-turn 的明确读取时钟分支。** `readSessionMessages.ts:137–138,391–410` 在读取时创建特殊 status，`:396` 取 `now()`；`routes/messages.js:185–205,301–308` 将它映射到历史消息数组。比较器仅在以下源码生成形态同时匹配时，把具体 `http[readonly.messages#N].body.messages[i].timestamp` 登记到报告 `reading_clock_paths`：`kind="status"`、`provider="pilotdeck"`、非空 sessionId、id 以该 sessionId 加 `-incomplete-turn-status-` 开头且后缀非空、text 严格等于源码固定中文提示，且没有 entryId/sequence/turnId/runId。这一精确路径只验 ISO 毫秒格式、单位和 wall 区间，排除出历史事件见证及历史顺序序列；其余消息（包括普通 status）仍执行历史规则。原消息保留在响应比较中，未删除或替换其内容；如果它只在一侧出现，结构差异仍 FAIL。对照覆盖该分支和带 entryId 的伪装 status，不能靠“本批恰巧没出现”取得豁免。

### 维持格式、单位、wall 区间的逐路径依据

这些路径继续使用第 7 轮严格格式及单位定义，不新增跨采集相等/不等排名，也不把读取时刻误作持久化事件时间。本节只裁定下列路径，未知路径仍 literal。

| 通道 / 上下文 | 完整路径 | 为何维持这些校验 |
|---|---|---|
| `http / readonly.messages` | `stream.messages.*.timestamp` | `routes/messages.js:74–75 → pilotdeck-bridge.js:1267–1279 → pilotdeck-message.js:32` 在 toFrames 创建帧时独立 `new Date().toISOString()`，不是日志别名；live3 ref9 的读取帧确有跨毫秒分布，固定并列排名无源契约 |
| `ws / complete` | `timestamp` | `pilotdeck-message.js:32` / bridge 的 createNormalizedMessage 默认生成发帧时间；无持久化时间副本关系，毫秒量化并列取决于调度 |
| `ws / status` | `timestamp` | 同一 createNormalizedMessage 发帧时独立生成，非事件 createdAt 别名 |
| `ws / stream_delta` | `timestamp` | 同一 createNormalizedMessage 发帧时独立生成，非事件 createdAt 别名 |
| `ws / text` | `timestamp` | 同一 createNormalizedMessage 发帧时独立生成，非事件 createdAt 别名 |
| `ws / config:reloaded` | `timestamp` | `ui/server/services/pilotdeckConfigWatcher.js:58,67,78,91,99` 与 `ui/server/routes/config.js:527,756,995,1038` 在通知产生时取时钟，没有持久化原值见证 |
| `disk / home/auth.db` | `tables.app_config.rows.*.created_at` | `ui/server/database/db.js:68` 独立 CURRENT_TIMESTAMP，单一创建时间，无该对象生命周期另一端或跨通道副本；跨初始化对象的秒级等值不是稳定别名 |
| `disk / home/auth.db` | `tables.vapid_keys.rows.*.created_at` | `ui/server/database/init.sql:67` 独立 CURRENT_TIMESTAMP，同上，无配对更新时间 / 时间副本 |
| `http / shadow.files` | `items.*.mtimeMs` | `src/gateway/dialog/projectFiles.ts:99` 来自 stat.mtimeMs；遍历顺序不代表修改顺序，逻辑快照不保存原 stat 时间见证 |
| `disk / @chat` | `*.metadata.updatedAt` | `src/session/metadata/SessionMetadataStore.ts:23,37,46,51,58,62,92` 在构造 metadata 时独立生成，早于事件 enqueue，不是 event.createdAt 别名。该原值作为 projection 的来源，但它自身没有另一份更原始时钟可见证；不凭字段名字虚构生命周期对 |

`ws / projects_updated / timestamp` 的来源为 `ui/server/index.js:337` 独立广播生成；第 7 轮已对整个 projects_updated 的已登记时钟保留排名，本轮**继续保留该既有规则**以及其格式 / wall 门，不归入上述仅格式路径。其他既有 CLOCK_PAIRS、CLOCK_RESPONSE_ORDER、expiresAt 见证保持不变。

### 验收方式

沿用原 20 ref + 5 TS + 1 proxy 的显式 historical-replay，不启动服务或重采。controls 增加消息倒序、消息全改读取时刻、日志 createdAt 倒序、anchor 改值的单侧与两侧同坏；projection conversation/status/metadata 另有按身份或最新值的反例；允许合法同毫秒并列，明确测试 incomplete-turn 路径；注入 clock 回拨必须 REFUSED。真实归档副本也执行四组单侧/双侧变异与时钟 REFUSED，重算 HTTP ETag/Content-Length/body bytes/offset 或磁盘快照，避免完整性门替代目标规则命中。最终结果记入 `_tmp/RESULT.md` 第 9 轮。
