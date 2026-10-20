# MCP 客户端协议层真实 TS 夹具（A6.5-pr10a）

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，SDK
`@modelcontextprotocol/sdk@1.29.0`（生成器断言版本）。出处：
`src/mcp/client/McpClient.ts`、`src/mcp/runtime/{sanitize,wireName,truncate}.ts`、
`src/mcp/config/{loadMcpServerConfig,expandPlaceholders}.ts`。`oracle.json.sourceSha256`
覆盖这三个入口的相对 import 传递闭包（8 个文件）。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
LANG=en_US.UTF-8 node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/mcp-client/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/mcp-client
```

生成器接共享溯源守卫 `bindSourceTree` + `sourceClosureSha256`，断言 en-US locale；
整个运行独占一个 `mkdtemp` 目录，删除前核归属。两次运行逐字节一致（无时间戳；临时
路径换成 `<dir>`，`mkdtemp` 后缀换成 `XXXXXX`；cwd 在运行目录外时记为 `<outside>`）。
`#[ignore]` 测试 `generator_reproduces_the_committed_oracle_twice` 会在设置
`PDRS_BASELINE` 时重跑两次并与入库文件逐字节比较。

## mock 服务器

`mock_server.mjs` 是无依赖的 Node ESM 脚本，TS 与 Rust 两侧跑的是同一份。它按
`MOCK_DIR/script.json` 对每个请求执行脚本步骤（`send` / `line` 原样行 / `sleep` /
`exit`，无脚本即挂起），每个进程把 spawn 时的 argv、cwd、非默认继承环境变量，以及收到的
每一帧原样字节写进 `log-<n>.jsonl`。macOS 自动注入的 `__CF_USER_TEXT_ENCODING` 不记。

## 语料：17 场景 / 77 步 + 7 组纯函数用例

| 场景 | 覆盖 |
|---|---|
| basic | 记忆化 start、listTools 缓存（只发一次 tools/list）、清洗/截断/重名 wireName、zod 输出键序、content 默认 `[]`、`args` 为 null/缺省、二次 close |
| call-errors | JSON-RPC 错误、zod 失败（union / 单一开放分支 / 布尔 / 数值下限 / base64）、required task 工具、outputSchema 缺 structuredContent 与 isError 豁免 |
| list-errors | tools 元素错、缺 tools + nextCursor 类型错、空列表 |
| invalid-frames | 非 JSON、非 JSON-RPC、未知 id 响应、服务器 ping（延后一跳回复）、未知请求（立即 Method not found）、通知、`\r` 行尾、字符串 id、同 id 重复响应、孤立代理项透传、内容与 `_meta` 键清洗 |
| handshake-error / -version / -schema / -crash / -timeout | 初始化 RPC 错误后重试成功、协议版本不支持、初始化结果 zod 失败、握手中崩溃、握手超时（进程不回收，同 TS） |
| spawn-enoent | `spawn <cmd> ENOENT` |
| call-timeout / timeout-message | -32001 超时与 `/timed out\|timeout/i` 消息都回收传输、下次调用新起进程、cancelled 通知帧 |
| server-crash | 调用中崩溃 → Connection closed，此后 Not connected，status 仍 ready（同 TS） |
| session-expired | 一次重连后重试成功；重连期间缓存失效；第二次过期错误原样抛出（McpError） |
| spawn-options / storage-root | args/env/cwd 透传、默认环境白名单、perSession 的 `--user-data-dir` 与 close 删除、STORAGE_ROOT 跳过注入 |
| close-pending | 在途请求遇 close → Connection closed |

纯函数：`sanitizeUnicodeString` 6、`recursivelySanitizeUnicode` 4（含清洗后键冲突与整数键序）、
`buildMcpToolWireName` 7、`parseMcpToolWireName` 7、`truncateMcpToolDescription` 5（含切开代理对）、
`expandMcpString` 11、`loadMcpServerConfig` 7（合并顺序、非法 JSON、根非对象、mcpServers 非对象、BOM、目录）。

Rust 回放 `tests/mcp_client.rs` 逐场景逐步骤按 `JSON.stringify` 逐字节比较结果、错误与状态，
再逐进程比较 spawn 记录与帧序列；条数精确断言。回放需要 PATH 上有 `node`（>= 18）。
