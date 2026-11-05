# #1328 pr1 weixin 渠道金样（U12 prepare_weixin_login + WeixinChannel 挂载）

`generate.mts` 在 Node **v22.22.0 / ICU 78.2** 上读取固定基线 **cc7c4e6** 的
`src/adapters/channel/weixin/WeixinChannel.ts`、`WeixinSessionMapper.ts`、
`ChannelStatePersistence.ts`、`ImLiveReplyController.ts` 等 16 个源（闭包哈希守卫），
配套 `src/cli/pilotdeckServer.ts` 与 `src/cli/pilotdeck.ts` 的 prepare 回调逐字摘录，
原样转译执行。`weixin-ilink` 用真 npm 包；其 HTTP 端点全部指向本地 loopback
假服务端（`FakeIlink`），不接触任何真实账号或外网。

## 11 个场景（`oracle.json`，输入步骤 + TS 期望同录）

1. prepare 三形状（无 server → unsupported；有 server → QR 登录 → requested:true）
   + 消息回复；
2. 已存凭证启动，首个 getupdates 体携带持久化 cursor；
3. `/new` 中止在飞 turn（aborts 携带 runId）、mapper 轮换、后续消息跑在新键；
4. 4500 码点回复按 weixin maxLength 2000 分片（2000/2000/500）；
5. errcode -14 → expired 报告 + 活跃 chat 通知 + 轮询 break，在飞 turn 仍完成回复；
6. 真实 HTTP 500（异常分支，钉 `ilink/bot/getupdates 500: …` 错误形状）与
   `ret!=0` 重试，各睡真实 3s；
7. 可恢复传输错误（clientFactory 包装注入，双端同构）→ 重建 client 日志；
8. 重复 prepare → hot-reload 替换 + 经 flush 恢复的 mapper 状态；
9. QR 登录失败（confirmed 缺 bot_token）→ failed 报告 → 2s 凭证监视器恢复；
10. config 停用（config-disabled）+ close 幂等（memoized null）；
11. 回复发送失败（sendmessage 500）入 pendingReplies，下一 poll 先 flush 队列
    再派发后续 turn（3 次 sendmessage：失败/重发/新回复）。

## 确定性

- getupdates 长轮询保持到脚本下一条，且**在响应时刻**才记录（消除 poll 到达与
  派发请求之间的次序竞态）；其余端点到达即记录。
- 停止类操作（prepare/reload/close）用 settleStop：先应答 held poll 让
  pollLoop 在顶部检查处退出（TS stop 同步发 abort 后等 pollPromise）。
- held turn 的 park 标记（`turn-N-held` 合成请求）只在消费方消化完
  `turn_started`（activeRun 登记）后记录；`wait-held` 闸以此固定 `/new`
  abort 的 runId 存在性。
- 真实 timer（QR 轮询 1s、重试 3s、恢复监视 2s）照跑，闸为时间上限 10s。
- 掩码：client_id → `<client-id>`、x-wechat-uin → `<uin>`、UUID → `<uuid>`、
  ISO 时间戳 → `<now>`、端口 → `<port>`、场景目录 → `<tmp>`、`; at=…` 栈尾
  （formatWeixinError 携带宿主路径）；日志按 [level, text] 排序后入 oracle
  （console 与 channel logger 跨流交错不作为协议）。
- 假服务端默认：getconfig → `{ret:0, typing_ticket:'tt-1'}`（保证 sendtyping
  总是跟随）、qrstatus 粘滞（脚本耗尽后重复最后答案）。

## 重生成（输出用 `recursive:true` + `flag:'wx'`，写到**新**目录）

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/weixin-channel/generate.mts /new/evidence/output
```

忽略测试 `weixin_channel_regenerates_identically` 必须在具备固定基线的宿主
显式运行。pr1 未覆盖面（elicitation/permission、附件、live-reply 节流、cron、
命令注册表、真实 WS 关停顺序、其他 bundle 渠道、config 热重载事件源）见
RESULT「后续」。
