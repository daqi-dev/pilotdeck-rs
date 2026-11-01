# #1265 受限 RegExp 原型

本 crate 有独立 workspace 与 Cargo.lock，默认产品 workspace 不编译或链接它。只验证 rquickjs `=0.14.0`（仅 `std` feature，crates.io 源码构建）的适用性；没有 outputSchema/grep 接线，不修改现有预算或冻结判据。

公开调用为 `Engine::new(Limits)` → `request(max_polls)` → `exec(pattern: &[u16], flags, input: &[u16], last_index)`。模式和数据均由分块 `String.fromCharCode` 加一次 `Array.join` 传入，捕获用 `charCodeAt` 回传。调用的都是内建函数，没有 eval 源码、模块加载、宿主 I/O 或对外暴露的 JS 对象。上下文只装 BaseObjects 与 RegExp；基础对象里的 eval / Function 构造入口也因未安装 Eval 而拒绝执行源代码。

一次 `exec` 返回 match/no-match 加 lastIndex；匹配包含 UTF-16 范围、编号/命名捕获与 `d` indices。`last_index` 遵循 JS lastIndex：无 g/y 时忽略起点；u/v 可以从代理对中间退回对首；空匹配不会自动推进。这不是迭代器。

请求借用 Engine，不能在同一个 runtime 并发调用。一个请求内各次匹配共享 interrupt poll 总量；新请求创建独立预算和取消 token，旧 token 不影响后继。只有 native `RegExp.exec` 阶段扣 poll，编译与字符串桥不混入“匹配内部可中断”的证据。零预算、耗尽与取消是显式 Error，耗尽不能回退成无匹配。每个线程独立建立 runtime 可并发运行。

默认实验限额为内存 16 MiB、栈 512 KiB、模式 4096 u16、输入和累计捕获输出各 1,048,576 u16、flags 最多 8 bytes。拒绝 memory 小于 1 MiB、stack 不在 16 KiB–16 MiB，以及零长度配置，避免 native setter 的无限制值。长度限制属于原型局部策略，不代表已获批生产阈值。内存/栈限制由 QuickJS 执行；编译错误保留阶段、name 与 detail。QuickJS 将编译栈溢出、分配失败也报成 SyntaxError，调用方不能单凭 name 当作非法模式。

已发现的接入缺口：

- QuickJS 最多 254 个捕获组；固定 Node 接受 255 个。用例明确记录这个边界，没有修改金样使其通过。
- 旧 SDK 108 条金样中，两条 raw-lone-surrogate patternProperties 模式在 Ajv schema 路径处理时抛 URIError；裸 RegExp 可以执行。本封装只对齐 RegExp，未实现 SDK schema 编译行为。
- poll 是协作式终止，没有逐步精确计数或硬时间上界。编译阶段主要依赖模式长度和 native 内存/栈；没有独立字节码大小、编译截止时间或 cache/queue。
- 尚未做 test262 子集、真实 grep/完整 outputSchema 接线、Linux 构建/RSS、native FFI 安全审阅、漏洞审计、双谱系评审及人签字。因此不能据本原型解除生产接入暂停。

从仓库根执行（先设调用方独立 CARGO_TARGET_DIR，并运行 scripts/cargo-slots.sh）：

```sh
export CARGO_TARGET_DIR=/Users/da/ws/pdrs-worker-build/issue-1265
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
cargo test --locked --manifest-path src/pilotdeck-regexp-proto/Cargo.toml
cargo clippy --locked --manifest-path src/pilotdeck-regexp-proto/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path src/pilotdeck-regexp-proto/Cargo.toml --check
```

固定 Node v22.22.0 / ICU 78.2 与只读 `PDRS_BASELINE` 就绪后，加跑 ignored 重生成守卫。fixture 来源、负控与跨平台交接见 `../pilotdeck-gateway/tests/fixtures/rquickjs-regexp/README.md`。本轮完整验收证据交付于任务指定 `_tmp/20261026-issue-1265/RESULT.md`（不入提交）。
