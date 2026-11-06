# #1328 pr1 weixin 渠道金样（U12 prepare_weixin_login + WeixinChannel 挂载）

`generate.mts` 在 Node **v22.22.0 / ICU 78.2** 上读取固定基线 **cc7c4e6** 的
`src/adapters/channel/weixin/WeixinChannel.ts`、`WeixinSessionMapper.ts`、
`ChannelStatePersistence.ts`、`ImLiveReplyController.ts` 等 16 个源（闭包哈希守卫），
配套 `src/cli/pilotdeckServer.ts` 与 `src/cli/pilotdeck.ts` 的 prepare 回调逐字摘录，
原样转译执行。`weixin-ilink` 用真 npm 包；其 HTTP 端点全部指向本地 loopback
假服务端（`FakeIlink`），不接触任何真实账号或外网。

## 15 个场景（`oracle.json`，输入步骤 + TS 期望同录）

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
    再派发后续 turn（3 次 sendmessage：失败/重发/新回复）；
12. confirmed 但 `bot_token:""`（JS falsy）→ 登录失败 → 凭证监视器恢复；
13. confirmed 但 `ilink_bot_id:""` → 同上（auth.js:81 的另一半）；
14. 响应 `get_updates_buf:""` 不清游标：后续 poll 体仍带持久化 cursor；
15. `typing_ticket:""` 时仍取 getconfig 但不发 sendtyping。

12–15 是 JS 真值语义（`if (x)` 对空串为假）的金样钉子：去掉对应的空串
`filter`，回放即变红。负控 `empty-*` 四条变异一格一条，死法分两种：
14/15 死在金样比对（`#1328 weixin replay oracle`）；12/13 死在 10 s 等待闸门
（`#1328 weixin harness gate did not resolve`）—— 空凭证被接受后登录不再
`failed`，回放走不到比对，与 `credentials-fallback` 同形。

## #1821 live-reply 负控（控制器级 mock 测试 + weixin 超时集成）

`live_reply.rs`（`ImLiveReplyController.ts` 全量移植）的三条变异，
killer 是 `channel::weixin::live_reply::tests` 的控制器级 mock 测试与
`channel::weixin::tests` 的超时集成用例（假 `WeixinGatewayPort` 流在
`turn_started` 后挂起，暂停时钟上只有看门狗能让 turn 收尾）：

| 变异 | 死点 |
|---|---|
| `live-reply-edit-dropped`（`flush_current_now` 不走 edit） | EDIT 金样序列比对 |
| `live-reply-watchdog-not-armed`（`arm_turn_timer` 不写 deadline） | 单元看门狗格 + weixin 集成 abort 记录；`zero_turn_timeout_never_aborts_the_turn` 保持绿（=0 不起看门狗是正行为） |
| `live-reply-activity-not-pulsed`（`flush_activity_now` 不调 `pulse_activity`） | ACTIVITY 金样序列 + 十分钟静默边界两侧两格 |

weixin 的 transport 不提供 `edit` / `maxMessageLength`（TS 同形，
`WeixinChannel.ts:786-800`），这两条能力由控制器级 mock 测试钉住；
`turnTimeoutMs` weixin 默认 0（生产不起看门狗），超时格由选项打开后测。

## #1823 入站媒体（26 个解密/URL 场景 + 5 个 CDN 下载场景）

`generate.mts` 在同一个基线提交上追加 `expectedMediaCaseCount` 与 31 条
`kind` 标注的用例（`decrypt` / `download`）；渠道 15 条不动。Rust 侧重放在
`src/pilotdeck-gateway/src/channel/weixin/media/tests.rs`
（`decrypt_replays_the_oracle_media_cases` / `download_replays_the_oracle_cases`）。

媒体辅助函数在 `WeixinChannel.ts` 里是**模块级、未导出**的，所以生成器按标记
切出源码片断 `function extractWeixinMediaUrl(` … `function installIlinkFetchCompatibility(`
原样转译执行（与全文件闭包哈希守卫并存）；切片 sha256 记为
`mediaSliceSha256`，`WEIXIN_CDN_BASE_URL` 字面量记为 `cdnBaseUrl`，切片漂移
会以 oracle 差异暴露而不是悄悄换了被测函数。

解密场景（每格的 `expected` 录 `keys` / `keyShape` / `candidates` / `imageLog`
/ `resultHex` / `changed` / `image` / `fileResultHex` / `fileChanged` /
`fileKnown` / `downloadUrl`）：

| 场景 | 钉住的行为 |
|---|---|
| `media-image-plaintext-passthrough` | 已是明文 → 原样返回，不试 key |
| `media-image-owner-hex-aes128` | `aeskey` 32 hex 字符 → aes-128 |
| `media-image-owner-hex-48/64-not-a-key` | owner hex 仅认 32 字符；48/64 字符既非 hex key，base64 解出也不是 16/24/32 字节 → 零候选 |
| `media-image-media-base64-aes128/192/256` | `media.aes_key` base64 三种长度 → 三种算法 |
| `media-image-second-candidate-hits` | 第一个 key 错、第二个对 → 候选循环不能只看第一个 |
| `media-image-media-hex-text-key` | 微信真实形状：base64 的 32 字符 hex 文本，解出 32 字节 ASCII（首候选，aes-256）+ 其 hex 解码（次候选，aes-128） |
| `media-image-dedupes-identical-keys` | 两处来源同一把 key → 去重后只剩一条 |
| `media-image-owner-base64-fallback` | owner 非 32 hex 时按 base64 再试 |
| `media-image-media-key-bmp-low-byte` / `-low-byte-equals` | Node 的 base64 把 JS 串按 **UTF-16 单元的低字节**取字节：`\u0141` 当 `A`、`\u013D` 当 `=` 截断；「跳过非 ASCII 字符」的读法会还原 key16 并解密，两格都要求零候选 |
| `media-image-all-miss-falls-back` | 全不中 → 原样返回不抛 |
| `media-image-not-block-multiple` | 非 16 字节整数倍 → 无候选（Node `final()` 抛） |
| `media-summary-bad-keys` / `-no-owner` / `-owner-not-object` | 诊断串的 `keyShape=none` / 长度 / hex 判定 |
| `media-summary-trimmed-owner-key` | `readString` 返回**未 trim** 原串（len=36），hex 判定与解码用 trim 后 |
| `media-file-pdf-plaintext` / `-encrypted-pdf` / `-text-passthrough` | 文件路径用 `isLikelyKnownFileBuffer`（含 mostly-text 启发式） |
| `media-file-plaintext-wins-over-decrypt` | 明文直通的可观测格：该 buffer 本身是 known file，**用它自己的 key 解出来也仍是 known file**；去掉 `isExpectedBuffer(buffer)` 早退就会采用诱饵 |
| `media-url-encrypt-query-param` / `-fallbacks` / `-missing` | `encrypt_query_param` 优先并 `encodeURIComponent`；否则 `full_url`→`download_url`→`url`→`cdn_url` |

下载场景跑的是**真的 `ImAttachmentStore.saveFromUrl`**（`input.responses`
是脚本化应答，`expected` 录字节或错误串 + CDN 收到的请求行）：

`media-cdn-download-ok`（200）· `-http-404`（`Attachment download failed HTTP 404.`）·
`-connection-failed`（`TypeError: fetch failed`）· `-follows-redirect`（302 →
`/final`，两条请求都记）· `-declared-too-large`（`content-length: 52428801` →
`Attachment is 52428801 bytes (limit 52428800).`，且必须在读 body 前就拒绝）。

Rust 侧的重定向走 crate 共享的 `fetch_redirect::follow`（undici 的
`redirect: "follow"` 语义，含 bad-port 表、多 `Location` join、fragment 剥离），
不是本模块内联的第二份循环。**收到字节的上限**另有一条 Rust-only 用例
`the_received_body_is_capped_at_the_limit`：基线 `saveFromUrl` 先
`arrayBuffer()` 全量缓冲再比大小，无 `content-length` 的流式 CDN 会把内存撑爆，
本移植改为读时即封顶（`fetch_redirect::read_body`），超限报
`Attachment is 52428801 bytes (limit 52428800).`——这是唯一一处有意偏离
基线实现细节的地方，端到端可观察结果（拒绝超限附件）不变；50 MiB 的 body
不进 fixture（会撑大 oracle），故用脚本化 loopback 服务器直接推流。

负控（`negative_controls.py`，全部 KILLED）：

| 变异 | 死点 |
|---|---|
| `media-plaintext-passthrough-dropped`（早退加 `&& buffer.is_empty()`） | `plaintext_passthrough_wins_over_its_own_decryption`（`media-file-plaintext-wins-over-decrypt` 的诱饵被采用） |
| `media-candidates-first-key-only`（`.take(1)`） | `the_candidate_loop_reaches_the_second_key`（`media-image-second-candidate-hits`） |
| `media-key-length-always-128`（24 字节也走 `Aes128`） | `the_key_length_selects_the_aes_variant`（`media-image-media-base64-aes192`） |
| `media-all-miss-returns-empty`（全不中返回空 Vec 而非 `None`） | `an_all_miss_falls_back_to_the_input`（`media-image-all-miss-falls-back`） |
| `media-pkcs7-strip-dropped` | `decryption_strips_the_pkcs7_padding`（`media-image-owner-hex-aes128`，尾部 padding 未剥） |
| `media-base64-single-char-guard`（去掉 1 字符保护） | `node_base64_semantics` 的 `"A"` / `"!"`（index 越界 panic） |
| `media-download-received-cap-removed`（读 body 上限改 `usize::MAX`） | `the_received_body_is_capped_at_the_limit` |
| `media-download-terminated-mapped-to-fetch-failed`（断流映射回 `fetch failed`） | `a_terminated_body_reports_the_fetch_type_error` |
| `replay-missing-case`（`remove(0)` 丢掉一条渠道场景） | `weixin_replays_the_ts_channel_oracle` 的 `#1328 weixin oracle case count` |
| `media-oracle-missing-case`（`pop()` 丢掉一条媒体用例） | `weixin_replays_the_ts_channel_oracle` 的 `#1823 weixin generator media declaration` |

解密行的死点是**聚焦单元用例**而不是 31 格回放：回放在第一个不符的格上中止，
不同变异停在不同的格，而 receipt 按「测试名 + 断言原文」精确匹配，逐格回放
给不出稳定的声明死点。这些聚焦用例读的是同一份 oracle 格输入（单一真相源），
只把每格行为单独钉死。

**端口**：任务书建议挑固定段（如 18950-18969），本 fixture 改为**临时端口**
（`listen(0, '127.0.0.1')`，沿用本文件既有 `FakeIlink` 的写法）：oracle 里只存
掩码后的 `http://127.0.0.1:<port>/…`，Rust 重放时绑一个 `127.0.0.1:0` 的
listener 再把 `<port>` 换掉。这样端口不是宿主全局资源，不会 EADDRINUSE，
也就不需要登记固定段（原任务书的端口冲突风险随之消失）。

## 确定性

- getupdates 长轮询保持到脚本下一条，且**在响应时刻**才记录（消除 poll 到达与
  派发请求之间的次序竞态）；其余端点到达即记录。
- 停止类操作（prepare/reload/close）用 settleStop：只应答**停止开始时已
  parked** 的 held poll（stop 的 pollPromise park 在它上面；TS stop 同步发
  abort 后等它）；替换渠道在 stop 期间新发的 poll 是普通长轮询，保持 parked
  等下一条脚本——否则记录数取决于事件循环粒度（Rust 重放无法复现）。
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
- #1823 媒体格没有随机源：明文/密钥/ciphertext 都是常量，AES-ECB 确定性，
  CDN 端口与临时目录都进掩码；同一基线两次重生成逐字节相同（实测 r6==r7）。
  解密输入清一色 16 字节整数倍，诱饵格在生成时就断言「明文与它的解密结果都
  是 known file」，不成立直接生成失败，不会录进一格空转用例。

## 重生成（输出用 `recursive:true` + `flag:'wx'`，写到**新**目录）

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/weixin-channel/generate.mts /new/evidence/output
```

忽略测试 `weixin_channel_regenerates_identically` 必须在具备固定基线的宿主
显式运行。pr1 未覆盖面（elicitation/permission、live-reply 节流、cron、
命令注册表、真实 WS 关停顺序、其他 bundle 渠道、config 热重载事件源）见
RESULT「后续」；入站附件（#1823）已由本文件上一节覆盖，出站上传（#1824）与
入站消息项接线（#1822）仍不在本 fixture 内。
