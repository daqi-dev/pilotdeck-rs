---
id: A8-CHANNEL-WS-RANDOM-001
---

## 类别

有意改进：经人批准的新 channel-v2 比较口径例外。它不证明某项 TS/Rust 产品行为已对齐，也不豁免任何缺失的实际场景。

## 基线依据

固定 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85 检出 `/Users/da/ws/pdrs-runner/resources/PilotDeck` 可实际解析的 ws8.21.0 组件：`lib/websocket.js:747` 产生16字节随机 key，`:759` 写入握手头，`:960` 校验由同 key 派生的 Accept；`:1031` 将实例 generateMask 传入发送器。websocket.js SHA256 `48fdb0f54c1f8ce580d3cfa2908fe7d96fe18f06f9d82306950441cb09b5db26`；sender.js SHA256 `d0791d30c3defd44dcabbddb879a901c757993fea2c00a7ffea01d53b23b4b77`。

mbp2 两次真实组件连接的业务输入/输出、六帧次序和本轮 TCP chunk 长度相同，严格投影恰有八处随机值差异。原测 SHA256 `fe7e7b2bc7fb3164e0f9ebb3a60dc8bac2755e16c77c7949f051dfd70688630e`，本轮 `_tmp/20261112-wave1-validation/mbp2-ws-arbitration/mbp2-results/` 保留全部原件与前后源码/依赖绑定。此处只证明固定检出可解析组件的实际行为，未认证完整锁定依赖闭包，不冒称真实 TS Server、TS/native 八渠道或全域验收。

## Rust 行为

Rust 产品实现不因本条更改。`scripts/gateway-parity/retirement_channel_v2.py` 的 `wire_projection` 先调用现有 HTTP/WS 原字节验证，再仅对本条列出的随机值比较认证标识或非mask头前缀。完整原始 key、Accept、mask、wireHeaderHex、wireHex、payload 和 TCP 记录继续留在各自来源绑定的观察证据中。

## 范围

仅新 channel-v2 的已验证 WS upgrade/frame 投影：唯一 key 必须严格解码为16字节，唯一 Accept 必须由同一次请求 key 正确派生；二者比较认证标识。合法客户端帧的四字节 mask 必须存在且正确解码，data payload 继续绑定实际 API，control payload 保留全部协议检查和解码内容；跨侧比较非mask头原始字节 `wireHeaderPrefixHex` 与 `maskKeyBytes`，不比较随机 mask 数值。

FIN、RSV、opcode、masked位、原始长度编码/扩展长度、payloadLength、分片/消息数、全部payload、close/ping/pong、其它header、HTTP、UUID/业务ID、次数、同连接双向全序和完整性限制均不变。既有 A2/G4、`compare.py`/通用 Normalizer 和其 mask-key 控制保持原行为。本例外不证明密码学熵、新鲜性或未来实现的随机源质量；不扩大A7权限，不移除任何缺失profile，不改变 formalAcceptance=false。

## 机器判据

正式控制入口为 `scripts/gateway-parity/test_retirement_channel_v2.py`：独立握手/mask正控，合法重mask的精确预期迁移，以及缺原字节、非法nonce/accept/mask、非随机业务字段、控制帧内容/数量、未知UUID和同连接全序反控。正式源码回归、两家非作者审查及冻结登记尚待完成。

批准前的独占 scratch 已对两份真实原件实际复算：原投影不同、有限候选相同，16个派生反控全部符合预期；结果明确 `OBSERVED_NOT_AUTHORIZED`，不能倒填为获批后的正式成绩，也不能冒称原68套件或 TS/native 采集通过。候选 patch SHA256 `d64de10b476673c7560792dd8158916c4b3d899dbe65916734657b3a33473bfb`，原件位于 `_tmp/20261112-wave1-validation/mbp2-ws-arbitration/proposal/mbp2-results/evaluation/`。

## 批准记录

2026-11-12 用户对本单具体方案明确回答：“批准，仅实施上述有限 v2 例外（推荐）”。语义范围已获人批；批准记录见 `state/decisions-20261112.md` 和 https://github.com/daqi-dev/pilotdeck-rs/issues/3062 。独立双审、获批后真实复算与重新登记仍待完成，此条不是阶段验收或合并通过记录。
