# PilotDeck Anthropic 纯协议库

从原 gateway 提取的请求构造与流解析算法。依赖 `pilotdeck-protocol` 的边界值和 `regex`；不依赖 gateway、Kernel、网络、文件系统或凭据。

公开接口包含原 Anthropic `build_request`、`StreamState`、`normalize_stream_event`、usage 归一化，以及请求所需的 thinking、内容块和 UTF-16 工具参数修复。gateway 通过薄适配保留原错误类型，并注入原测试使用的字符串长度限制；原 fixtures/oracles 不变。

`pilotdeck-protocol::coercion` 承载共用的原 ECMAScript 转换语义，gateway 和 Budget 使用同一实现。缺失值、undefined/null、UTF-16、数值和对象标签语义保留。

本包继承仓库 workspace 元数据，仍需本 checkout 的 Protocol 等依赖；“可独立消费”表示不必构建 gateway 或 Kernel，不表示本轮已提供可搬移发行包。独立消费者示例位于 `../pilotdeck-context/examples/independent-consumer`。
