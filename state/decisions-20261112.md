# 2026-11-12 决策

## #3062：channel-v2 已认证 WebSocket 随机值的有限比较例外

**状态：人已明确批准语义范围；实现、独立双审、正式复算与冻结登记仍须完成。** 原始批准由 Codex 记录在 [#3062](https://github.com/daqi-dev/pilotdeck-rs/issues/3062)，用户答复原文：

> 批准，仅实施上述有限 v2 例外（推荐）

- **卡在哪**：mbp2 两个普通 ws8.21.0 客户端向同一 loopback server 发送相同业务输入并获得相同输出，原始 parser 均通过，现投影因三个客户端帧的 mask/header 和握手 key/accept 共八处不同而不等。属于当前严格规则下的真实结果，不把旧 FAIL 改成通过。原件及16项候选反控在本轮 `_tmp/20261112-wave1-validation/mbp2-ws-arbitration/`；原测 SHA256 `fe7e7b2bc7fb3164e0f9ebb3a60dc8bac2755e16c77c7949f051dfd70688630e`。
- **谁受影响**：新 channel-v2 的完整 WS 比较；同时含 fixture-owned 控制连接与产品发起的 WS 连接。不修改既有 A2/G4、通用 Normalizer 或其他 profile。
- **能否撤回**：可以通过后续明确更正撤回该例外；本轮原始 TCP、handshake、所有帧头/payload、旧标准与旧结果均保留。不得直接改写既有追加式账本。
- **动不动尺子/意向**：改变 channel-v2 的有限比较语义，不改产品意向。批准范围仅为：严格认证16字节 key 及同 key 派生 Accept 后比较认证标识；合法客户端4字节 mask 保留原件及解码认证，比较非mask头原字节和mask长度，不比较四个随机数值。data payload 仍与实际 API 绑定；control payload 没有同类 API 事件，保留协议检查和完整解码内容。本检查不证明 RNG 熵或 nonce 新鲜性。
- **选项与后果**：维持严格标准会保留该阻塞；只固定 fixture 输入不能覆盖产品自己发起连接的随机输出；用户已选择有限v2例外。FIN/RSV/opcode/masked/长度编码、全部业务/control内容、close/ping/pong、其余header、UUID/次数及同连接全序均保持。
- **执行**：仅在现有完整原字节验证之后改投影，旧两项合法重mask控制仅在批准范围迁移；保留缺wire、非法nonce/accept/mask、非随机字段、次数和顺序反控。人批不等于阶段通过；独立issue、两家非作者审查、真实数据复算和冻结登记不得省略，也不扩大A7权限。

**可复用原则**：跨侧比较例外只处理已明确批准且已经原字节认证的具体字段；原件不重写，未列字段保留严格比较。机检断言落在 `scripts/gateway-parity/test_retirement_channel_v2.py` 的独立随机值正控及协议/业务/全序反控，登记边界见 `specs/gateway-parity/diffs/A8-CHANNEL-WS-RANDOM-001.md`。
