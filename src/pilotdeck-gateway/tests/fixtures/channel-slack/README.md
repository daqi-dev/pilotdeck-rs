# #1344 channel-slack 渠道金样

`generate.mts` 在 Node **v22.22.0 / ICU 78.2** 上读取固定基线 **cc7c4e6** 的
五个源文件（`src/adapters/channel/slack/` 的 SlackChannel.ts、
SlackSessionMapper.ts、slack-render.ts，加 `protocol/` 的
ImElicitationHelper.ts、ImPermissionHelper.ts——都是真件），esbuild 转译后以
受控 require 装载，只替换两类边界：`gateway.submitTurn` /
`respondElicitation` / `permissionDecide`（脚本化回合事件流，`#1339` 的 sms
gateway 形状）与 `@slack/bolt`。**bolt 是桩合同级替身**（#1786 人拍 1：基线
没装真 bolt，`bolt-stub.mjs` 的七个成员就是提交的线上合同）：`new App({token,
appToken, socketMode})`、`event("message", h)`、`error(h)`、`start()`、
`stop()`、`client.auth.test(args)`、`client.chat.postMessage(args)`，其它成员
抛 `unexpected SDK call <name>`。`start()` POST `/api/apps.connections.open`
（body `{"token":appToken}`）取回 `url` 开 WebSocket；事件帧形状
`{"type":"message","payload":{"event":…}}`；`stop()` 即 `ws.close(1000)`；
`auth.test` / `postMessage` POST 参数对象为 JSON body（**键序保持**），`ok:false`
抛 `Error: <error>`。环回假服务把 REST 与 WebSocket upgrade 服务在**同一个**
端口上。不启动真实账号或非环回网络。每用例一个固定 uuid mapper 缝
（`new SlackSessionMapper(undefined, () => 'aaaa1111-…')`）。

端口段是**宿主全局**资源：18183-18198（16 个），36 个用例**严格串行**、按
`18183 + index % 16` 轮转；Node 与 Rust 重放两侧都在下一个用例绑定前关掉
监听（两侧都开 SO_REUSEADDR）。与既有 fixture 避开的区间：
weixin 18800-18921、feishu 18960-18993、wecom-callback 18900-18921、
api-server-channel 18100-18182 之外见各 README。重放前若端口被占，测试报的
是金样失败而不是被测代码回归，先按 `lsof -nP -iTCP:18183-18198` 核占用方。
同段并发由双层锁串行化（#2322）：进程内 tokio mutex（重放与 `#[ignore]`
再生成互斥），加宿主级锁 `/tmp/pdrs-channel-slack.lock`（`scripts/cli-parity/
README.md`「锁被保留时怎么解」正本形态：`.guard` flock + 目录 + `owner.json`
{pid,timestamp,token,cleanupPending}，tmp+rename 原子发布；死 pid 或超 2400 s
按正本规则回收；takeover 规则内的租约等待不失败，规则外的租约——未来时间戳、
truthy `cleanupPending` 等——等待上界 `HOST_LOCK_MAX_AGE` + 余量（3000 s）后
panic 指名 `scripts/cli-parity/README.md`「锁被保留时怎么解」）——两个
`cargo test` 进程同段并发时后者等待而非
误报 `ensure_replay_port_free`。端口段内的 18184/18185（#2321/#2312 组件测试，与夹具
用例同在 18183-18198 轮转段）靠同持两锁串行。

36 个场景覆盖 29 行状态机矩阵（含矩阵外补充格）：start 全配置、缺
botToken / 缺 appToken（mock 全程静默作证）、connections.open `ok:false`、
auth.test `ok:false`（socket 已开，失败句柄 stop 为 no-op——TS 侧连接存续到
teardown，Rust 侧 wire-invisible 回收：drop sink 发裸 TCP FIN、abort reader，
`expected.ws` 只记 `open` 两侧同形，差异登记于
`A8.3-CH-SLACK-001-R2.md`）、普通消息回合、thread
内消息、thread 根键、bot_id 过滤、bot_message subtype 过滤、
message_changed/deleted 过滤、自发消息过滤、纯 @提及（静默）、@提及剥离、
active 去重跳过、**连发两帧**（两帧之间无 wait——#2313 保序格：TS stub 对每个
message 帧同步调 handler、帧序即入场序，第一帧入回合、第二帧判 already active）、
turn 结束后的重投递、`/new`、`/new <正文>`（不发确认）、
超长分片（38500+\n+1500 → 40000+500... 两段）、turn 异常、elicitation 答复 /
取消（"0"）、permission 1/2/0、permission 提示发出失败→状态回复+重发、
turn 在途 stop、stop 后静默、同实例再 start（mapper 状态保留）、
start-fresh（mapper 重置）、postMessage `ok:false`、mrkdwn/链接转换、空文本
静默、无 channel 静默。

## oracle schema

顶层键：`sourceCommit` / `sourceSha256`（五个被装载基线源文件的 sha256） /
`command` / `node` / `icu` / `expectedCaseCount`（36）/ `cases`。每个 case：

```json
{
  "name": "…",
  "port": 18183,
  "turns": [
    { "gate": "可选：submitTurn 先挂在这道闸门",
      "events": [{"type": "…"}],
      "error": "可选：回合最终错误",
      "holdAfter": "可选：'elicit'|'decide'——事件流停在答复 RPC 落地前" }
  ],
  "steps": [
    { "op": "start" },
    { "op": "start", "without": ["botToken"] },
    { "op": "restart" },
    { "op": "start-fresh" },
    { "op": "ws-send", "event": {…} },
    { "op": "stop", "reason": "done" },
    { "op": "release", "gate": "闸门名" },
    { "op": "wait", "requests": 3, "logs": 2 }
  ],
  "scripted": [ { "status": 200, "json": {…} } ],
  "expected": { "requests": […], "ws": […], "rpcs": […], "logs": […] }
}
```

- `steps` 是可观察量序列，Rust 重放逐步执行并把四类记录拼成快照与
  `expected` 逐格比对。
- `scripted` 是该用例的 HTTP 应答队列（**每个请求**按到达顺序消费一位；
  队列空了按路径兜底：`/api/apps.connections.open` → `{"ok":true,"url":…}`
  指回本用例端口，`/api/auth.test` → `{"ok":true,"user_id":"U123",
  "user":"slackbot"}`，其余 → `{"ok":true}`）。队列干了 = 用例吃了兜底 =
  那一格是垃圾——生成器断言脚本必须全消费，Rust 重放同断言。要失败某个
  postMessage 就必须连 open、auth.test 一起入队（顺序服务）。
- 记录口径：`requests`（method/path/contentType/body，body 是**原始 JSON 字符串**，
  postMessage 键序即 TS 对象字面量序）；`ws`（`{"event":"open"}` /
  `{"event":"close","code":1000}`——只记 open 与 close，文本帧方向不在合同面）；
  `rpcs`（`submitTurn` 记 request 对象、`respondElicitation`/`permissionDecide`
  记 response/decision 对象，与 sms #1339 相同的 `to_js` 键序口径）；
  `logs`（`[level, message]`）。
- 跨步骤等待用 requests/ws/rpcs/logs 计数或 ticks（`{ticks:50}` 是静默检验）。
- 拆卸口径：用例结束的 `stopMock` **先**对每个幸存 socket 写服务器侧
  close(1000)（`0x88 0x02 0x03 0xe8`）——start 失败句柄的 socket TS 侧
  永不关闭，裸 destroy 会变成异常关闭、桩的 error handler 会把 `bolt error`
  日志竞速进已快照的记录；`closing` 旗标让随后的帧读端忽略迟到数据。

## maskDynamic

**无**。会话键里的 uuid 来自固定 mapper 缝（`aaaa1111-…`），ts 时间戳是
事件字面量（`"300.1"` 等），没有任何真实时钟或随机量进入四类记录；两侧按原样
逐格比对即可。

## 已登记差异与本金样的关系

差异编号见 `specs/gateway-parity/diffs/A8.3-CH-SLACK-001.md` 及其 R1（生产装配
接线更正）/ R2（#2312 auth.test 失败路径 wire-invisible 回收）（正反向行为差异
只在差异文档登记，本金样两侧同库同规则，不承担差异检测）。
