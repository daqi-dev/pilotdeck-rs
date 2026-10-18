# Host context dispatcher 金样

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。`generate.mts` 直接运行真实 `createSidecarDefaultModuleDispatcher`，注入假 context/budget/model；身份注入使用真实 `createSidecarContextRequestIdentityServices`。真实 `SidecarTurnProtocol.moduleResponseFor` 记录 wire response（私有类仅在内存追加 export，依赖指回基线原文件），没有手写 dispatcher replica。

生成前后运行共享 `bindSourceTree`，绑定整棵基线 src 与固定 Git blobs。oracle 的 `sourceSha256` 列出本范围入口及确定性请求组装依赖，Rust 回放统一经 `tests/common/baseline.rs::oracle` 校验。期望值只从本次 TS 执行产生。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/host-context/generate.mts [output-parent]
cargo test --manifest-path src/Cargo.toml --locked --offline -p pilotdeck-gateway --test host_context
```

生成器每次以 `mkdtemp` 新建独占目录，stdout 最后一行 `{output,cases}` 指向结果。媒体只读该目录的夹具文件；不删除目录、不自动覆写仓库 oracle。Rust 回放同样使用 UUID 独占目录并保留证据。录用前跑两次、逐字节比较两份及仓库 oracle。

覆盖五个 operation 和全部 16 种可选能力组合；operation/input 缺失、null、错类型；宿主身份覆盖；capture 返回 null；runtime 错误的 code/canonical/retryable/retryAfterMs。普通 input 按 TS 只接纳对象，不额外施加 ContextRuntime 类型校验。

try_auto_compact 覆盖 stage 回填（含显式 budgetStage 优先于 budgetProjection）、配置预算缺省、budgetRequest 的浅 canonical 校验与删除、无 budget 端口、preparation 查找和路由一致性、materializer 引用要求、calibration 投影、可信 signal、候选消息与快照合同错误、runtime/materializer/budget 异步错误原样传播。

候选请求覆盖 assistant 合并、preview prepare、pre_route/routed/recovery 投影、plan reminder、systemPrompt nullish 回退、cache 重建与 locale 排序、UTF-16、深层 metadata、媒体成功和 fallback。缓存里的 JS String(value) 与排序错误以真实 TS 行为为准，不提前校验 runtime 返回值；preview 的 tools 为非可迭代对象/数字/undefined/null 或含 null 元素时，错误文本也由真实 V8 运行固定。

Rust 回放精确断言总条数，逐条比较 methods、完整 port 调用序列和响应 payload/错误字段；双方序列化后逐字节比较，保留 JSON 的 undefined 省略规则并锁定所有层级的键序，不使用数值容差。wire 相关 ID 属于既有 X0 测试覆盖范围。ContextRuntime 未来实现、真实模型网络调用和持久化压缩属于后续包；本夹具只替换依赖端口，dispatcher 与请求组装均为真实基线。
