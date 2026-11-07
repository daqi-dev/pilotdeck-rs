# #1343 channel-bluebubbles 渠道金样

`generate.mts` 在 Node **v22.22.0 / ICU 78.2** 上读取固定基线 **cc7c4e6** 的
`src/adapters/channel/bluebubbles/` 两个源文件加上共享渲染与两个 helper
（BlueBubblesChannel.ts、BlueBubblesSessionMapper.ts、bluebubbles-render.ts、
`protocol/ImElicitationHelper.ts`、`protocol/ImPermissionHelper.ts`），
esbuild 转译后以受控 require 装载，只替换三类边界：`gateway.submitTurn` /
`respondElicitation` / `permissionDecide`（脚本化回合事件流与应答记录，只记录
TS 真实传入的字段）、BlueBubbles 服务器（`serverUrl` 指向 127.0.0.1:19461
环回假服务，请求入记录、按**每方法到达序号**消费脚本应答槽位）、
`node:crypto` / `node:url` 为基线真实模块。bluebubbles 不用 SDK——TS 说全局
`fetch`，本金样是 #1786 的无 stub 对照。不启动真实服务器、账号或网络。
每个用例一个新的 BlueBubblesChannel 实例；mapper 用真 uuid（记录时遮罩）。

端口段：环回假服务 **19461**（与 feishu 的 18960-18999、weixin 的
18800-18921、wecom 的 18900-18921 区间均不重叠）。重放前若端口被占，测试
报的是金样失败而不是被测代码回归，先 `lsof -nP -iTCP:19461` 核占用方。

16 个场景的输入步骤与 TS 结果都在 `oracle.json`：缺配置（无 serverUrl）只报
error 不起轮询、首轮回合-回复闭环、同 guid 二次投递不重执行、行过滤全集
（null/42/字符串行、isFromMe/is_from_me、空白正文、无 chatGuid、无 guid 仍
派发、chats[0] 兜底、id 兜底去重、500 轮询告警）、应答形态（数组 / `{data}` /
`{messages}`）与时间戳推进（数字 / 字符串推进、`time` 字段不推进、非数字为
null 不推进）、`/new` 命令与 `s_<uuid>` 会话映射、活动会话跳过、stop 不等待
在途回合（held turn 在 stop 后完成、回复被 running 闸门拦下）、restart 清空
seen 集合（同 guid 通道层再派发，差异 004）、hot-reload 新实例（mapper 归
`:general`）、elicitation 应答续流 / 回复 0 取消、permission 允许一次 / 拒绝、
submitTurn 报错（"Error: boom" 日志 + 兜底文案）、sendReply 500（message 字段
进日志）。

## oracle schema

顶层键：`sourceCommit` / `sourceSha256`（被装载基线源文件的 sha256） /
`command` / `node` / `icu` / `expectedCaseCount`（16）/ `cases`。每个 case：

```json
{
  "name": "…",
  "port": 19461,
  "password": "pw-N",
  "serverUrl": "http://127.0.0.1:19461",
  "turns": [
    { "gateBefore": "可选：该回合 submitTurn 后先等哪道闸门",
      "events": [{"type": "…"}],
      "holdForRespond": "可选：事件流先停在这里，等用户应答后续流",
      "resume": [{"type": "…"}],
      "error": "可选：回合最终错误" }
  ],
  "steps": [
    { "op": "start", "fresh": "可选：换新实例再 start（hot-reload 形态）" },
    { "op": "stop", "reason": "done" },
    { "op": "release", "gate": "闸门名" },
    { "op": "wait", "polls|sends|gateway|responds|decides|logs|ticks": 1 }
  ],
  "scripted": { "polls": [{"status": 200, "json": […]}], "sends": [] },
  "expected": { "pollQueries": […], "sends": […], "gateway": […],
                "responds": […], "decides": […], "logs": […] }
}
```

- `steps` 是可观察量序列，Rust 重放逐步执行并把六类记录拼成快照与
  `expected` 逐格比对。
- `scripted` 是该用例的**索引脚本**：第 i 个 GET 取 `polls[i]`、第 i 个 POST
  取 `sends[i]`，队列干了分别按 `200 {}` / `200 {"sent":true}` 兜底。Rust 重放
  在自己的环回假服务上按同一序号武装。第二起及以后的轮询只拿到兜底 `{}`，
  在去重的 `pollQueries` 投影里不可见。
- 记录口径：`pollQueries`（去重后的首个查询串）、`sends`（method/path/auth/
  contentType/body）、`gateway`（sessionKey/channelKey/message 三元组 +
  每回合一个 `{turnEnd: true}`；回合以错误收尾时不记 turnEnd）、`responds` /
  `decides`（`JSON.stringify` 的原文，键序即 TS 构造序）、`logs`
  （`[level, message]`）。

## maskDynamic / maskQuery

- `maskDynamic`：`[0-9a-f]{8}-…-[0-9a-f]{12}`（**不带词边界**，要命中
  `s_<uuid>` 内部）→ `<uuid>`。作用域：sends body、logs message、gateway 的
  sessionKey（**不**含 message 本文）。responds / decides 不遮罩——两个
  interaction 用例的会话都是 `:general`，不含 uuid。
- `maskQuery`：`after=<digits>` 且值距录制时钟 ±300 s 内 → `after=<now-s>`
  （start 播种的时钟种子，逐次运行变化）；脚本时间戳停在 1600000000 /
  1900000000 段，永不命中该规则，第二轮询的 `after=1900000300` 原样入格。

## 已登记差异与本金样的关系

差异编号见 `specs/gateway-parity/`（002 无生产 InteractionPort · 003 stop 不
drain · 004 restart 同 guid 通道层再派发 · 005 非法 serverUrl 不炸进程 ·
006 无信号轮询 GET 的 300 s 总界；001-R2 摘除拒绝表条目）：

- **002**：生产装载挂载装 `no_interaction()`，elicitation/permission 应答链在
  生产不可达；金样回放装真实 helper 支撑的 `HelperInteraction` + 脚本
  responder，状态机路径全部被金样覆盖。
- **003**：`stop` 不等待在途回合（#1353 收口 drain 契约）。stop-drains 用例
  钉的就是 TS 形状：held turn 在 stop 之后完成，`sendReply` 的
  `running === false` 闸门拦下出站。
- **004**：restart 后同 guid 行在通道层再次派发（TS 同形）；「不重执行」由
  上游 A7 接纳器保证，duplicate-guid 用例只钉单 epoch 内的去重。
- **005**：`serverUrl` 非空但非法时 TS 是未处理拒绝（Node 默认崩进程），
  Rust 记 `error` 日志继续轮询——金样不覆盖该路径（假服务地址恒合法），
  组件钉子只钉 URL 构造的 `Invalid URL` 错误串。
- **006**：无 `AbortSignal.timeout` 的轮询 GET 仍以 undici 默认
  `headersTimeout`（实测 301,257 ms）为总界收口，错误串保持 TS 同形的
  `TypeError: fetch failed`；组件钉子
  `http_client::tests::request_unbounded_settles_a_silent_server_at_the_undici_default`
  用暂停时钟（keeper + receipt 防空转）钉 300 s 定界与错误串，负控
  `unbounded-timeout-dropped` 删界必被杀。

## 重生成

重生成到**新的**目录（输出用 `recursive:true` 和 `flag:'wx'`，旧目录先手工归档）：

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/channel-bluebubbles/generate.mts /new/evidence/output
```

生成器 `expectedCaseCount=16`，Rust 重放做三重校验：`cases` 非空、条数与
`expectedCaseCount` 一致、条数等于生成器的字面量 16（只钉声明的话，删一条
用例同时改小声明仍会 PASS）。

## 判别力正控（#1786「怎么验证」第 1 条）

装置判别力由四段原始输出证明（`_tmp` 轮次目录 `positive-control/` 下）：
基线副本（含 `.git`，钉在 cc7c4e6，LFS 以指针形态跳过 smudge）上重跑入库
生成器 → oracle 与入库件逐字节相同；直接改副本工作树（删 isFromMe 过滤行）
→ 入库生成器被 provenance 守卫拒绝（`baseline has uncommitted changes to
tracked files`），所以行为变异经生成器副本的**编译缝**注入（来源树仍由
assertBaseline 绑定未变异副本）：删 isFromMe 过滤 → oracle 的
row-filters 用例多出 `我自己`/`我方` 两条派发（gateway 6→10 格）；改
`已创建新会话。` 文案 → new-command 用例的回复格变字。再把 m1 金样喂给不
同步的 Rust 回放 → row-filters 用例判红 FAIL，换回入库金样 → PASS。即：
金样跟着 TS 行为走，Rust 跟着金样走，两层都不吃写死值。

## 组件钉子（Rust tests.rs）

金样之外，`channel/bluebubbles/tests.rs` 另钉三个单元行为：`query_encode`
对 URLSearchParams 序列集（空格→`+`、`+`→`%2B`、`!~'()`→百分号编码）、
`url_with_path` 的 origin 解析（绝对引用替换基路径、默认端口摘除、scheme/
host 小写、`Invalid URL`）、`normalize_base_url` 的尾斜杠裁剪——都是 Node
v22.22.0 实测字面量。

## 负控

`negative_controls.py` 形态照 channel-feishu：`--check` 校验锚点；
`OUT [mutation …]` 在落盘后的 HEAD 树上执行（`mutating_sources` 拒绝未提交的
产品文件）。变异锚点覆盖 seen 去重删除、fromMe 闸反转、时间戳推进删除、
`/new` 回复删除、sendReply running 闸删除、MESSAGE_LIMIT 改值、拒绝表回插
bluebubbles、无信号轮询 GET 的 300 s 总界删除，以及 oracle 三重校验各一
条；先 `--compile-only` 全过再交协调者跑 OUT。
