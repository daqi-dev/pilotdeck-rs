---
id: D6-WEIXIN-CDN-REDIRECT-001
---

## 类别

bug 修复：Node 固定环境二进制 POST 的 307 重发自身失败，Native 保留既有拥有 Bytes 的正确重发。非无差异等价。

## 基线依据

固定 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85 src/adapters/channel/weixin/WeixinChannel.ts:1115-1143，CDN fetch POST Uint8Array AES 密文。真实 _tmp/20261110-channel-difference-evidence/weixin-307/outbound-capture.mjs 的 redirect307 在 Node22.22.0/ICU78.2 返回 false 并发文本 fallback，未到第二 CDN 跳。独立 _tmp/20261110-channel-difference-evidence/weixin-307/redirect-probe.mjs 对 Uint8Array/Buffer 两例均观察 TypeError fetch failed，cause 为 Cannot perform ArrayBuffer.prototype.slice on a detached ArrayBuffer。证据 _tmp/20261110-channel-difference-evidence/weixin-307/redirect-observation.json SHA 3edb90b7fcf600891a3b30fca314b1fc2f67d81b1f4138bd3c02f3df45790119。仅 307 已实测；不把 308 推断写成实测。

## Rust 行为

src/pilotdeck-gateway/src/channel/weixin/outbound.rs upload_encrypted 复用已有 fetch_redirect，拥有 Bytes 后正确重发同一密文。真实 r43 libtest（SHA540e00c5875f80b4cc2efe1d79cf63f80aa2f595231c90cd2dabd469e608e4d5）直接运行 native_outbound_real_ilink_http_aes_cdn_and_fallbacks_match_fixed_ts：redirect307 result=true，两跳均 POST、octet-stream、bytesBase64 完全相同，然后真实 ILink sendmessage 媒体成功。原始日志 _tmp/20261110-channel-difference-evidence/weixin-307/native-r43.log SHA7e1e8bcb0713ea41c73a7edea4389a94bd4fec515dec2130aacc5080c5a0b4be。

## 范围

仅 Weixin AES CDN binary POST 307 的基线运行时失败修复。不声称 308 已实测，不扩展到 301/302/303、不泛化字符串 body，也不据此认定全部媒体/停机合同通过。共享 redirect 实现及冻结装置均未改。

## 机器判据

TS：固定源码真实 _tmp/20261110-channel-difference-evidence/weixin-307/outbound-capture.mjs redirect307 + 独立 binary POST 最小 probe；Native：channel::weixin::outbound_tests::native_outbound_real_ilink_http_aes_cdn_and_fallbacks_match_fixed_ts 显式区分 TS false/Native true，校验两跳方法/相同密文/最终媒体，不把差异伪装全等。libtest --exact 上述测试 --nocapture 可复算；诊断环境清除六个代理变量，所有 HTTP peer 为 loopback。真实 Server 媒体与停机另有 media_host_tests，不与本条成绩混合。

## 批准记录

未批准，非已接受差异。主协调已按既有人批默认修 bug 原则明确保留正确 Native 重发，不为复刻 TS detached-buffer 崩溃而降级。该产品授权不等于正式评分豁免、双谱系独立评审或人签验收；需主协调复核后另行登记。冻结 ledger/golden 未改。

Evidence manifest: _tmp/20261110-channel-difference-evidence/manifest.json. Raw Native first failures and immutable r43/r46 source bindings are retained under first-failures/; r48 rerun and independent reviews remain pending.
