# #1329 channel-feishu 渠道金样

`generate.mts` 在 Node **v22.22.0 / ICU 78.2** 上读取固定基线 **cc7c4e6** 的
`src/adapters/channel/feishu/` 三个源文件（FeishuChannel.ts、FeishuSessionMapper.ts、
feishu-render.ts），esbuild 转译后以受控 require 装载，只替换四类边界：
`gateway.submitTurn`（脚本化回合事件流，只记录 TS 真实传入的三个字段）、
attachment store 与 system command registry（`used in tests` 的空假件）、
Lark Open API（`https://open.feishu.cn` 重写为 127.0.0.1:18990 环回假服务，
请求入记录、按到达顺序消费脚本应答队列）、`node:http`/`node:crypto` 为基线
真实模块。不启动真实服务器、账号或网络。每个用例一个新的 FeishuChannel 实例
与一个固定 uuid mapper 缝（`new FeishuSessionMapper(undefined, () => '…')`）。

端口段是**宿主全局**资源：webhook 用例端口 18960-18972、重放 Lark 假服务
18990、组件钉子专用 18973（null-body 路由）/18991/18992/18993（钉子 Lark）。
与既有 fixture 避开的区间：wecom-callback 18900-18921 与坏端口 18997-18998、
weixin 18800-18921。重放前若端口被占，测试报的是金样失败而不是被测代码
回归，先按 `lsof -nP -iTCP:18960-18993` 核占用方。

13 个场景的输入步骤与 TS 结果都在 `oracle.json`：url-verification、verify token
不匹配（静默忽略）、顶层数字 token 遮蔽 header.token、非 JSON body、event_id
去重（第二次 `{"ok":true,"deduped":true}`）、文本 turn 出站序列（tenant token
POST → reaction POST → messages POST → reaction DELETE）、`/new` 命令、同 chat
第二条排队串行、5000 字符分两段（3998+1002）、post 无 `data.message_id` 回落
`msg_type:"text"`、stop 后路由落 404、未 start 503 `{"error":"feishu_not_started"}`、
hot-reload 保留路由。

## oracle schema

顶层键：`sourceCommit` / `sourceSha256`（被装载基线源文件的 sha256） / `command`
/ `node` / `icu` / `expectedCaseCount`（13）/ `cases`。每个 case：

```json
{
  "name": "…",
  "port": 18960,
  "turns": [
    { "gateBefore": "可选：该回合 submitTurn 后先等哪道闸门",
      "events": [{"type": "…"}],
      "error": "可选：回合最终错误" }
  ],
  "steps": [
    { "op": "start" },
    { "op": "http", "method": "POST", "path": "/feishu/webhook", "body": "…" },
    { "op": "stop", "reason": "done" },
    { "op": "release", "gate": "闸门名" },
    { "op": "wait", "lark": 4 },
    { "op": "wait", "gateway": 1 },
    { "op": "wait", "ticks": 20 }
  ],
  "scripted": [ { "status": 200, "json": {…} } ],
  "expected": { "http": […], "lark": […], "gateway": […], "logs": […] }
}
```

- `steps` 是可观察量序列，Rust 重放逐步执行并把四类记录拼成快照与
  `expected` 逐格比对。
- `scripted` 是该用例的 Lark 应答队列（生成器先拷贝再让 mock 消费，oracle 里
  留的是未消费副本）；Rust 重放按同顺序武装自己的 Lark 假服务，队列干了按
  `{"code":0}` 兜底。第二回合的 reaction DELETE 不占脚本位（token 缓存跨回合，
  兜底即够）。
- 记录口径：`http`（status/contentType/body，连接失败记 `{"error":"ECONNREFUSED"}`）、
  `lark`（method/path/query/auth/contentType/body；DELETE 的 body 是 `""`、
  contentType 是 null）、`gateway`（sessionKey/channelKey/message）、
  `logs`（`[level, message]`）。TS 生成器经 `fetch` 记录**解码后**的 body，
  Rust 裸 TCP 客户端因此对 `Transfer-Encoding: chunked` 应答先去分帧
  （`decode_chunked`）再入格——404 兜底应答走 chunked 分帧，`{"ok":true}`
  走 content-length，两边记录的都是逻辑 body。
- 跨步骤等待用 lark/gateway/logs 计数或 ticks；与 wecom-callback 相同，
  `tcp-open` 类屏障不需要（feishu 的 webhook 请求-应答是同一步内完成的）。

## maskDynamic

双侧重放同一规则，顺序固定：先 `\b\d{13}\b` → `<now>`，再
`[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}`（**不带词边界**，
要命中会话 key 的 `s_<uuid>` 内部）→ `<uuid>`。作用域：lark body、logs
message、gateway 的 sessionKey（**不**含 message 本文）。

## 已登记差异与本金样的关系

差异编号见 `specs/gateway-parity/`（001 stream/WS · 002 附件 · 003 系统命令
注册表 · 004 /new 不 abort · 005 无流式编辑 · 006 TurnRequest 字段/看门狗/
explicitSend · 007 顶层 null body，即 A8.3-CH-FEISHU-007）：

- **002 / 003**：生成器把 attachment store 与 system command registry 换成
  空假件（都是基线公开的测试缝），不触碰 `~/.pilotdeck`，13 个场景均不含
  附件与系统命令路径，不影响判分。
- **004**：TS 的 `/new` 会调 `gateway.abortTurn`；假 gateway 提供空实现且
  **不记录**（Rust A7 无 abort 概念，记录会假 FAIL）。
- **005**：live controller 的中途编辑不移植。生成器用基线公开测试缝
  `FeishuChannelOptions.liveReplyOptions` 抬高 buffer/throttle 阈值并关掉活动
  指示，使 TS 金样与 Rust 收尾同样「每段一次终稿 POST」，oracle 无需遮罩即可
  逐格比对；3998 的分段本身仍是生产语义（`MAX_TEXT_MESSAGE_LENGTH 4000 - 光标 2`）。
- **006**：gateway 记录只含 TS `submitTurn` 的三个入参；看板狗与
  explicitSend 不入可观察。
- **007**：顶层 `null` body（4 个字面字符）在 TS 是读 `raw.token` 的未捕获
  TypeError，金样无法携带该格，**由 Rust 侧组件钉子覆盖**
  （`top_level_null_body_is_answered_ok_and_never_dispatches`：POST `null` →
  200 `{"ok":true}`、不入站、无出站）。

## 重生成

重生成到**新的**目录（输出用 `recursive:true` 和 `flag:'wx'`，旧目录先手工归档）：

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/channel-feishu/generate.mts /new/evidence/output
```

生成器 `expectedCaseCount=13`，Rust 重放做三重校验：`cases` 非空、条数与
`expectedCaseCount` 一致、条数等于生成器的字面量 13（只钉声明的话，删一条
用例同时改小声明仍会 PASS）。

## 组件钉子（Rust tests.rs）

金样之外，`channel/feishu/tests.rs` 另钉三个单元行为，全部用
`crate::negative_assert*` 走 receipt：顶层 null body（差异 007）、2000 条
去重环的逐出（最老的 id 被逐出后重投不再报 deduped、未逐出的仍报）、
`99991663` 清 token 缓存（失败 POST 后文本回落用新 token，6 次出站序列）。

## 负控

`negative_controls.py` 形态照 wecom-callback：`--check` 校验锚点；
`OUT [mutation …]` 在落盘后的 HEAD 树上执行（`mutating_sources` 拒绝未提交的
产品文件，`channel/feishu/` 整树与 fixture 目录都是 #1329 的新文件，脚本随
补丁入库、由协调者在合入后跑）。#1329 初始 9 个变异锚点覆盖：去重环上限、
verify gate 比较方向、分段上限、token 失效码、排队串行、hot-reload 保留语义，
以及 oracle 的三重校验各一条；#2055 并发面（每事件代际闸两腿、elicit 占用、
权限 panic 释放、drainer panic 释批、`/new` 回读代际）与 #2775 fix-r4
（elicit 陈旧释放无条件化）后续按行追加，当前 17 行，各行语义见脚本内注释。
