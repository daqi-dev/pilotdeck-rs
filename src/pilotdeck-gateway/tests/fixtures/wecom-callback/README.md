# #1336 wecom-callback 渠道金样

`generate.mts` 在 Node **v22.22.0 / ICU 78.2** 上读取固定基线 **cc7c4e6** 的
`src/adapters/channel/wecom-callback/` 三个源文件（频道本体、SessionMapper、render），
esbuild 转译后以受控 require 装载，只替换四类边界：`ImElicitationHelper` /
`ImPermissionHelper`（脚本化假件，调用全部入 helpers 记录）、`gateway.submitTurn`
（脚本化回合事件流）、qyapi（`https://qyapi.weixin.qq.com` 重写为 127.0.0.1:18998
环回假服务，请求与脚本应答队列）、`node:http`/`node:crypto`/`node:url` 为基线真实
模块。不启动真实服务器、账号或网络。渠道端口从 config 注入（18800-18820 与
坏端口 70000），每个用例一个频道实例与一个确定性 mapper uuid 计数器。

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
（连接建立后再写头）+`tcp-chunk` 固定。

重生成到**新的**目录（输出用 `recursive:true` 和 `flag:'wx'`）：

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/wecom-callback/generate.mts /new/evidence/output
```

忽略测试 `wecom_callback_regenerates_identically` 必须在具备固定基线的宿主显式运行。
生成器 `expectedCaseCount=22`，Rust 重放做三重校验：`cases` 非空、条数与
`expectedCaseCount` 一致、条数等于生成器的字面量 22（只钉声明的话，删一条用例
同时改小声明仍会 PASS）。本单元暂无
已登记差异：`deliverCronResult` 随 #1334 落地的
`ChannelAdapter::deliver_cron_result` + `im_cron::deliver_chat_cron_result` 一并实现，
由 `deliver_cron_result_follows_the_shared_helper` 钉住（金样不含 cron 场景）。
