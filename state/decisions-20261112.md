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


## #3068：按既有正文规范修复比较类型碰撞

**状态：自决 · 待复核（AI 代批，待人复核）；修复既有判据实现，不新增相等例外。** 独立问题见 [#3068](https://github.com/daqi-dev/pilotdeck-rs/issues/3068)。

- **卡在哪**：macmini-01 真实 chunked HTTP 的 JSON `{"raw":"X"}`（11 bytes）和普通文本 `X`（1 byte）均通过完整原 TCP/HTTP 校验，两个比较投影却相等；Content-Length 对照能按11/1判差。证据在 `_tmp/20261112-wave1-validation/wire-value-arbitration-r2/`，真实 `compare_pair` 因缺双引擎业务和落盘原件仍 NOT_RUN，不能扩大结论。
- **谁受影响 / 能否撤回**：只影响新 channel-v2 的正文比较表示；产品实现、原始捕获、业务 oracle、models 和通用 Normalizer 不变。代码可由 Git 撤回，旧失败与捕获均保留。
- **动不动尺子 / 意向**：修比较装置，目标是恢复 `specs/gateway-parity/normalization.md` 已规定的 JSON 语义与非 JSON 全文区分；不增加忽略字段或归一化授权，不改变 #3062 仅 key/Accept/mask 的人批范围，不改意向及原冻结字节。独立两家评审、真实复算与合法登记仍是交付前提。
- **选项与后果 / 执行**：只修 wire 能挡文本误等，但 HTTP API 投影仍保留碰撞和非 UTF-8 异常。采用两层共同的 JSON/text/bytes 互斥包络，业务 JSON 必须嵌入其 JSON 包络，不能伪装内部文本标记。已锚 UUID/时钟仍在原内层形状归一。Node 实测拒绝的 NaN/±Infinity 按既有非 JSON 全文规则保留，合法数值溢出仍原 canonical 拒绝；重复 JSON key 保持固定 Node 与既有 Python 的后值覆盖，不擅改该语义。

**可复用原则**：比较器的内部类型标记不能与业务输入共享无标签的表示空间。断言由完整 `compare_pair` 类型碰撞控制、Content-Length 对照、JSON 语义正控及 NaN/数值溢出边界落实；旧实现须在精确文本碰撞控制上实际失败，不能拿无关异常代替检出。


### #3068 后续：内部比较表示统一，业务 oracle 不变

自决 · 待复核（AI 代批，待人复核）。r9两条局部形状P1已用旧423a完整合成compare_pair实测：最终均判差，不把局部字段相等当成真实假通过，原评语保留。全仓读方核实只有内部温度断言读取comparison.models裸形状，entries只转存；因此将models与channelStreams的比较输出也统一JSON/text/bytes包络，减少双重解析及非法常量的非预期canonical异常。这里的models比较字段不同于raw.models业务oracle：后者及transport_observation的decoded认证仍不动。合法JSON语义、同socket请求/响应绑定、长度/顺序/头策略、已锚实体归一保持；空体/null/空字符串控制与同19HTTP/16WS原件复算判定实效。此前5f17记录描述其当时的裸models比较输出，后续以本段为准；不新增#3062例外，不移除冻结/双审前提。

### #3072：Darwin GLib 补齐自身固定的 intl 源码

自决 · 待复核（AI 代批，待人复核）。5f17真实Darwin严格Clippy因GLib2.86.1缺intl而exit101；原nodownload正确拒绝未登记fallback。采用GLib固定wrap中的proxy-libintl commit33934de09af6a6627eb44e310a8079df009abdbb，13,064-byte官方源码归档SHA4ab5af85ec6124cb526a15c58bbc0877d241807e30855779808dbf9292d861aa，仅Darwin解包并force该fallback；Linux仍gvdb，既有nls=disabled/nodownload保持。上游STUB_ONLY构建匹配本项目现有不启用翻译目录的范围，避免引入全局gettext依赖。五路径改动可Git撤回，原FAIL及新执行均独占保留。

受影响的是产品图片原生库的源码闭包和精确source-profile登记：库存13→14，守卫只更新source-build/source-hashes两SHA，不放宽范围，不改变比较语义、意向或#3062授权。macmini候选五路径逐SHA相同，真实fmt/严格Clippy通过；52个staged库/别名、profile与otool闭包实核通过。真实库存正负控、GIF/PDF及生命周期定向用例和两家非作者终审仍须完成，Linux完整guard保持NOT_RUN。原件见本树_tmp/20261112-wave1-validation/darwin-libintl-source-candidate/，独立issue #3072。

可复用原则：平台缺依赖时先补其上游锁定的最小源码闭包，精确登记和实际动态库来源必须一致；不能以本机恰好装有的库或解除禁下载来替代。断言复用现有image_archive_pin_rejects_mutation_extra_file_and_symlink，并以真实构建/profile重算和逐库依赖核验约束。
