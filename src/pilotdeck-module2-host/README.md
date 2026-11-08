# Module2 宿主侧接缝

本包直接依赖原 `pilotdeck-protocol` 和 `serde_json`，不依赖 gateway、Kernel SDK 或三库提取 PR #2549。`publish.workspace` 继承主仓的禁止发布策略；许可沿用主 workspace。它提供原协议验证、24 个 HostCall 的清单、通用路由、调用方交换端口和既有时间格式工具，不执行模型、工具、审批、网络或资源释放。

`protocol::decode_boundary_line` 使用原 Protocol 解析器和 `EnvelopeRef` 验证，返回无损 `JsValue`，保留 UTF-16、null 与扩展字段。`decode_line` / `decode_frame` 是兼容现有 driver 的显式 serde DTO 投影；合法但 serde 无法表示的帧返回 `UNREPRESENTABLE_FRAME`，不是 `INVALID_JSON`，不静默替换 surrogate。需要完整 Module2 值域的调用方应走无损入口，不能把 serde 投影范围声称为全协议等价。

`dispatch` 只把已投影的 `module_call` 交给调用方 `HostExchange`；未知方法和未接的 lifecycle 明确拒绝。保留历史命名 `KernelClient` / `LcError` 是为了共享 sidecar driver 兼容，库本身没有连接、授权或 SDK 实现。时钟格式化不是原操作已接纳/完成的证据。帧字节上限、I/O、取消、进程生命周期、权限和副作用结算仍由调用方负责。

从仓库根目录运行独立 workspace 消费者：

```sh
CARGO_TARGET_DIR="$PWD/_build/cargo-target" cargo run --locked --manifest-path src/pilotdeck-module2-host/examples/external-consumer/Cargo.toml
```

示例仍依赖本 checkout 的 workspace 元数据、LICENSE 和 Protocol，验证链接和边界，不表示可搬移发行、Kernel 宿主映射完成或产品端到端验收。不要在示例内生成深层 target。

路由以原 Module2 的 `payload.operation` 为权威；顶层同名扩展缺失、冲突或错型均不改变它。已接收的 module_call 成功和拒绝均返回携带原 `inReplyTo`、`requestId` 的 response；拒绝保留 code/message/retryability，不转成流级 error。

无损入口只完成原协议验证并返回值，不直接执行 HostExchange；serde interop 路由仍受前述 UTF16 表示范围限制。UTC 秒级辅助函数不等同于 transport 毫秒事件时钟，也不承担生命周期时间权威。独立消费者的持续 CI 覆盖归跟进 #2560，当前仅声明已实际执行的 locked 验证。
