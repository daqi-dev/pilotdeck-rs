# #1336 wecom-callback 渠道金样

`generate.mts` 在 Node **v22.22.0 / ICU 78.2** 上读取固定基线 **cc7c4e6** 的
`src/adapters/channel/wecom-callback/` 三个源文件（频道本体、SessionMapper、render），
esbuild 转译后以受控 require 装载，只替换四类边界：`ImElicitationHelper` /
`ImPermissionHelper`（脚本化假件，调用全部入 helpers 记录）、`gateway.submitTurn`
（脚本化回合事件流）、qyapi（`https://qyapi.weixin.qq.com` 重写为 127.0.0.1:18998
环回假服务，请求与脚本应答队列）、`node:http`/`node:crypto`/`node:url` 为基线真实
模块。不启动真实服务器、账号或网络。渠道端口从 config 注入（18900-18921 与
坏端口 70000），每个用例一个频道实例与一个确定性 mapper uuid 计数器。

端口段是**宿主全局**资源：本机并行跑 `pilotdeck-gateway` 的 lib 套件时，同段端口被
别的渠道金样占用会以 `EADDRINUSE` 假红（#1336 r2 实测 channel-sms 占 18800-18854，
因此本套改用 18900-18920；#1336 r3 又发现 `channel/sms/tests.rs` 的单测固定绑
18901，故 `missing-outbound-config` 的「无人监听」端口改到 18921，避开整个
18800-18901 区间；该 sms 单测后于 #2257 fix-r5 移至段外 18926，本套 18921 的
选择保留）。重放前若端口被占，测试报的是金样失败而不是被测代码
回归，先按 `lsof -nP -iTCP:18900-18921 -sTCP:LISTEN` 核占用方。`#1941` / `#1336` 的两条单测用例
（`wecom_callback/tests.rs` 的超限与到顶格）绑的是 **18990 / 18991**、不在该段内，
被占时按 `lsof -nP -iTCP:18990 -iTCP:18991 -sTCP:LISTEN` 核（只看监听套接字）。

22 个场景的输入步骤与 TS 结果都在 `oracle.json`。覆盖：缺必填配置两条（corpId 与
secret/agentId，监听不启动）、EADDRINUSE 与 ERR_SOCKET_BAD_PORT、GET 面
（缺 echostr / 坏签名 / echostr 解密回显 / 405）、POST 拒绝面（无 Encrypt / 坏签名）、
正常派发回合、非 text 消息与空 Content 忽略、同 chat 在飞重复拒绝、`/new` 命令
（空消息建会话提示与带消息续接）、八种事件渲染（含 elicitation/permission 请求即答）、
elicitation 待答回填、permission 应答成功/投递失败回滚/无文本不发、回合错误兜底文案、
errcode 40014 清 token 重取、stop 关监听（后续连接 ECONNREFUSED）、重启不重放
（mapper 会话延续）、stop 排空在飞慢请求、stop 后已脱离回合仍送达、stop 幂等。

记录口径：`http`（status/contentType/body，连接失败记 `{"error":"ECONNREFUSED"}`）、
`qyapi`（method/path/query/contentType/body）、`gateway`（sessionKey/channelKey/message）、
`helpers`（调用名+参数）、`logs`、`events`（stop 完成序）。跨步骤等待用 qyapi/helpers/
gateway 计数或微任务 tick；stop 与在飞请求的相对位置由 `stop-pending`+`tcp-open`
（连接建立后写头、等服务端 `request` 事件再进下一步）+`tcp-chunk` 固定。`tcp-open`
的屏障是**请求头已解析、body 未到**：生成器包了 `node:http.createServer` 记录实例，
在客户端 `connect` 并 `write(step.request)` 之后等该实例的 `request` 事件；这与
Rust 重放取的屏障是同一个点（`service_fn` 入口，即 `http.rs` 的 accept 计数器自增处）。
只等客户端 `connect` 或只等服务端 `connection`（accept）都不够：`server.close()`
可能把「已 accept、头还没解析」的连接当空闲回收，把本该进排空的请求判成拒绝；
#1336 r3 起改用 `request` 事件，现存金样即在此屏障下重生成，两次运行逐字节一致。

重生成到**新的**目录（输出用 `recursive:true` 和 `flag:'wx'`）：

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/wecom-callback/generate.mts /new/evidence/output
```

忽略测试 `wecom_callback_regenerates_identically` 必须在具备固定基线的宿主显式运行。
生成器 `expectedCaseCount=22`，Rust 重放做三重校验：`cases` 非空、条数与
`expectedCaseCount` 一致、条数等于生成器的字面量 22（只钉声明的话，删一条用例
同时改小声明仍会 PASS）。`deliverCronResult` 随 #1334 落地的
`ChannelAdapter::deliver_cron_result` + `im_cron::deliver_chat_cron_result` 一并实现，
由 `deliver_cron_result_follows_the_shared_helper` 钉住（金样不含 cron 场景）。

`specs/gateway-parity/diffs/` 下登记的差异有三条：`A8.3-WECOM-CALLBACK-408-001`
（hyper 帧化的 408）与 `A8.3-WECOM-CALLBACK-KEEPALIVE-001`（空闲 keep-alive 时限
与缺 `Keep-Alive: timeout=5` 头）仍为未批准（非已接受差异）；
`A8.3-WECOM-CALLBACK-BODY-LIMIT-001`（#1941：`POST *` 请求体 1 MiB 上限，超限
零字节关闭；TS 的 `readBody` 无上限）已人批为已接受差异（批准记录
`state/decisions-20261108-2033-wecom-body-limit-approval.md`）。22 个
场景都不含 408 格、空闲 keep-alive 格与超限格（全部带 `Connection: close`，
body 远小于 1 MiB），故这三条不影响任何金样判分。
