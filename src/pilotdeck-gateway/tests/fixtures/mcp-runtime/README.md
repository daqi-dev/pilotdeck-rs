# MCP 运行时 / 工具桥 / 会话 registry 真实 TS 夹具（A6.5-pr10b）

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，SDK
`@modelcontextprotocol/sdk@1.29.0`。出处：`src/mcp/runtime/{McpRuntime,PluginToToolBridge,
SessionMcpRuntimeRegistry,ProjectMcpRuntimeProvider,parsePluginMcpServers,projectMcpSpec}.ts`、
`src/tool/builtin/{mcpTool,mcpResources}.ts`。`oracle.json.sourceSha256` 覆盖六个入口的相对
import 闭包（`tool/` 牵出大半个 `src/`，共 651 个文件）。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
LANG=en_US.UTF-8 node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/mcp-runtime/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/mcp-runtime
```

共享溯源守卫 + en-US 断言；运行独占一个 `mkdtemp` 目录、删除先核归属；两次运行逐字节
一致（`#[ignore]` 测试 `generator_reproduces_the_committed_oracle_twice` 复核）。每机不同的值
写成占位：运行目录 `<dir>`，`process.execPath` → `<node>`，Playwright / FunASR 入口与安装命令 →
`<playwright>` / `<funasr>` / `<install>`（Rust 侧由 `BuiltinMcpPaths` 注入同名占位）。

## 语料：17 场景 / 122 步 + 17 桥接用例 + 3 解析 + 6 补丁 + 10 builtin 运行

| 段 | 内容 |
|---|---|
| `live`（7 场景 / 47 步） | 真 `McpRuntime` 对 pr10a 的 `mock_server.mjs`：多 server 启停（ENOENT、握手 RPC 错误）、`connectConcurrency`、serverInfo、工具定义（annotation 旗标、宽松 schema）、内容投影（text/image/json 余项、cwd 下本地图片链接内联）、isError、未注册 server、崩溃后 Not connected、超时回收重起、AbortSignal（预先中止 / 在途中止）、**outputSchema 内容校验（#361）**、**同连接两请求乱序应答（#361）**；逐 server 比较每个进程收到的帧 |
| `handshakeClose` | **#361 握手中 close**：TS 记录为 `ready` 且进程未退出（泄漏）；Rust 有意不同：晚到的连接被关闭，`start` 报 `mcp_handshake_failed`，进程自行退出（回放断言） |
| `bridge`（17 用例） | 脚本化端口上的投影与错误映射：非数组 content、空数组、坏块、缺字段图片、多个本地图片、已有内联图片时不扫链接、非 stdio 不扫、isError 文本拼接与兜底、`errorCode` 透传（字符串 / 数字 / 缺省）、未注册 server |
| `parse` / `patch` | `parsePluginMcpServers`（占位展开、`url`→`httpUrl` 回落、二次展开、非字符串记录、数组 env、整数键序）；`patchProjectScopedMcpSpec`（browser-use / funasr / 其它 / 非 stdio、`resolve` 规范化、`replaceAll` 的 GetSubstitution） |
| `registry`（4 场景 / 30 步） | 同 key 多注册按身份释放（后注册者先释放）、重复释放幂等、dispose 记忆化与聚合错误、draining 中并行释放、disposed 后拒绝注册 |
| `provider`（6 场景 / 45 步） | 共享代际按签名复用（键序无关）、租约计数、release 幂等、dispose 后拒绝、per-session 规格（含 force）、项目/全局 mcp.json 合并与诊断、启动部分失败（funasr 文案）、start 拒绝回落空租约、stop 失败上抛、并发 acquire 只建一个代际、聚合 dispose 错误 |
| `builtins` | `createMcpTool` 名称归一化与抛错、默认/自定义描述与 schema、`list_mcp_resources` / `read_mcp_resource` 描述符与适配器调用、无适配器 `unsupported_tool` |

已知限制：`patch` 的对象键序是 TS 对象展开的副产物（`{...spec, command, cwd, args}`），回放按
排序后的键比较值；其余全部按 `JSON.stringify` 原序比较。provider 的并发 `stop` 事件无可观察
顺序，两侧都按 runtime 标签排序后比较。
