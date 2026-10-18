# Host tool context · TS 金样

基线为 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。生成器直接执行原始 checkpoint、seed projection、sidecar context builder / 三种 service wrapper，以及 dispatcher 的 drain/applySteerAuthorization。没有复制 TS 实现，也不读取 Rust 输出来生成期望。

```sh
PDRS_BASELINE=/absolute/path/to/PilotDeck \
  node --import /absolute/path/to/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/host-tool-context/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/host-tool-context

cd src
cargo test --locked --offline -p pilotdeck-gateway --test host_tool_context
```

每次运行在 `mkdtemp` 独占目录中工作，只清理自己创建的目录。生成前后断言基线 commit 和 tracked working tree 干净，并重新计算五个直接相关源文件的 `sourceSha256`；不信任旧哈希。缺省环境场景将进程环境替换为固定测试对象，不捕获真实环境变量。输出不含时间、临时路径或用户环境，重复运行应逐字节相同。

`oracle.json` 精确包含 **82** 条：seed 38、checkpoint 5、context 36、steer 3。生成器和 Rust 回放同时断言总数与分类数；服务调用轨迹逐项比较。JSON 对象按字段语义比较，Map 内部次序用独立键数组断言，非有限数用真实 TS 数值分类断言，避免 JSON 将 NaN/Infinity 转为 null 后造成假通过。

覆盖缺省/null/错型 seed、四种 read kind、可选字段过滤、非 plain object、null prototype、原生 Map、数值键排序、孤立 UTF-16、非有限数；构造与快照脱离原件、两张 Map 共享、allowedReadFiles 数组隔离与有序去重；宿主身份覆盖、远端两个字段的准入、继承属性读取、truthy/nullish 配置、restricted permission、auxiliary binding/fallback、plan/goal/fork/progress 服务调用及执行 ID/期限；steer 的延后授予、过滤、覆盖、空列表保留、未知与重复 apply。

## 宿主端口边界

Rust 的服务对象以 `Arc` 句柄传递，测试的可序列化标记只用于观察。另有 capability / permission trait 接线测试，检查取消句柄身份、共享文件状态、后续授权可见性，以及模型只绑定一次。

`TurnEnvironment` 是**必传宿主端口**：本单验证 cwd/sessionId/turnId/config.env 的完整传参、每次 runtime 构造调用一次，以及远端 env 不生效；测试端注入真实 TS `buildTurnEnvironment` 的输出。它**不证明 Rust 已实现该 helper**。标准环境适配器（process.env 缺省、path.resolve、NFKC 路径段和三个 PILOTDECK 变量）须在宿主装配前补齐；当前 gateway 没有可用的直接 NFKC API，而本单要求 Cargo.lock 不变，因此没有引入新依赖或简化 Unicode 行为。

原生 Map 的 Rust 输入由 `SeedState` 的两张有序表表示；`parse_seed_state` 也接受 `JsObject::set_native_tag("Map")` 的字符串键表。普通 wire record 则按 JS 数字索引键顺序解析。Rust 类型本身不允许 Map 的非字符串键。

steer mailbox/drain 状态机属 A6.7-pr1。本单只提供共享 `SteerAuthorizations`：drain 后用 host itemId 和过滤后的非空字符串列表 `remember`，canonical steer durable 后 `apply`；两者必须持有同一表和同轮 checkpoint。不能在 drain 时调用 allow_read_files。
