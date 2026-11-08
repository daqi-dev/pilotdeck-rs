# 独立源码消费者

本示例有独立的 `[workspace]`，通过相对路径使用三个能力包及 `pilotdeck-protocol`。不依赖 gateway、Kernel、凭据、模型端点或全局应用配置；断言验证 API 链接，不证明完整行为对拍或实际预算强制执行。

从仓库根目录运行：

```sh
CARGO_TARGET_DIR="$PWD/_build/cargo-target" cargo run --locked --manifest-path src/pilotdeck-context/examples/independent-consumer/Cargo.toml
```

请使用该树自己的构建目录；上述命令不会在示例目录生成深层 target。已登记的独立 Cargo.lock 固定依赖闭包。

本示例仍依赖完整 checkout 的 workspace 元数据、根 LICENSE、Protocol/Tokenizer 及既有 vendor assets。这是仓外 workspace 消费源码的验证，不是可搬移或可发布的软件包，也未接入 MintHub 发布。
