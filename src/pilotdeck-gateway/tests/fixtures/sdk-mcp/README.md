# SDK MCP 配置真实 TS 夹具

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。出处：
`src/cli/ProjectRuntimeRegistry.ts:1184-1308`（set / toggle / reconnect /
permission override / status），以及它调用的
`src/mcp/runtime/parsePluginMcpServers.ts` 校验路径。`oracle.json.sourceSha256`
覆盖 `ProjectRuntimeRegistry.ts` 与
`GatewaySessionPermissionRuleSetRegistry.ts` 的相对 import 传递闭包。

生成器通过 `Object.create(ProjectRuntimeRegistry.prototype)` 调用真实类方法，
只注入以下协作者：`resolve`（返回固定 `/pdrs-fixture/project`）、
`pluginRuntime.refresh/snapshot`、`gateway.closeSession`、
`sdkSessionPlugins`，以及活的 `GatewaySessionPermissionRuleSetRegistry`。
`setSessionPlugins` 步骤是夹具专用准备步骤，因为写 `sdkSessionPlugins` 的
`setSdkSessionConfig` 属 A6.3-pr8；它只改注入的 Map，不冒充被测方法。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/sdk-mcp/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/sdk-mcp
```

生成器先接共享溯源守卫 `tests/baseline_source_guard.mjs` 的 `bindSourceTree`：
显式绝对 Git、HEAD==固定提交、已跟踪树干净、`src/` 每个 blob 逐字节核对；再断言
`Intl.Collator().resolvedOptions().locale === "en-US"`。两次运行 `oracle.json`
逐字节一致（无时间戳、无临时路径、无进程环境值）。

`oracle.json.scenarios[]`：

- `projectPlugins` / `sessionPlugins` / `configured`：注入的插件快照、会话插件和
  活权限规则数组；`configured` 在生成器里克隆后传给 registry，避免后续
  `updateAskRules` 反向改写录制的初始输入。
- `steps[]`：`step`（op+input）、`result`（`{ok}` 或 `{error:{name,code,message}}`）、
  `state`（servers/overrides/rules/projects + 活 ask/allow）、`calls`
  （累计的 resolve/refresh/closeSession 序列）。
- 输入里的显式 `undefined` 编码为 `{"$":"undefined"}`，回放前由生成器/Rust 测试解码；`enabled` 的 `undefined` 也按同一标记记录，其余值原样。

Rust `tests/sdk_mcp.rs` 逐场景逐步骤回放，结果、状态与调用序列都按
`stringify` 深比较；`baseline::oracle` 始终校验提交与哈希形状，设置
`PDRS_BASELINE` 时再用 Git 重算闭包字节。当前语料为 **16 场景 / 132 步**，
回放与生成器都断言这两个精确条数，空或截断的 oracle 不会通过。

| 场景 | 覆盖 |
|---|---|
| basic | set 的 added/removed、重复 toggle、reconnect 成功/禁用/未知、set 保留 disabled、空 set 删除 |
| invalid-configs | `servers` 缺键/null、空对象、stdio 缺/空 command、http 缺/空 url、`null` 配置的 TypeError、字符串/数字配置、数组/字符串 servers 的 `Object.entries` 键序、非可迭代 `args`、只给 `httpUrl`、占位符展开为空/空白 |
| reserved-names | 项目插件与会话插件占名；混合错误批不改状态、不 closeSession |
| permission | auto/default/clear、重复 auto、`normalizeMcpPermissionSegment` 的空白/下划线/非 ASCII/emoji、生成本地规则对象与 ask 数组同身份、无 live 权限条目时只存 override、session/server/mode 的缺键/null/空串/非法值以及非字符串 sessionKey/serverName 的 TypeError |
| permission-session-order | 两个 live 会话的生成规则 Map 在更新 s 后仍保持 `s,t` 插入序（TS `Map.set` 不移动已有键） |
| status-order | 会话插件先入、插件名整数键排在普通键前、SDK 服务器覆盖同名状态、禁用状态、无/空/null sessionKey 空投影、项目插件不进入列表 |
| prototype-env | `${env:toString|constructor|__proto__|hasOwnProperty|valueOf}` 经 `Object.prototype` 回落得到非空 command，配置有效并 added |
| home-expansion | `~/`、`~`、`${userHome}` 在 command 与 url 中展开；HOME 固定为 `` $` ``，`${userHome}` 单独出现按 GetSubstitution 展开为空（诊断），`${userHome}/x` 保持非空 |
| project-key | 字符串 `projectKey`（含空串）原样传给 resolver，并进入 `sdkSessionProjects` |
| errors | refresh 拒绝、resolve 拒绝、closeSession 拒绝（状态已改）、无 gateway 时不调用 close |
| config-key-order | 整数键 JSON 顺序、相同配置重复 set 不算 added、改 command 算 added |
| toggle-enabled | `enabled` 原值保留（null/0/空串/"false"/缺键）与 status 真值、nullish 时 re-set 回落 true |
| explicit-undefined | 输入显式 `undefined` 三处：sessionKey/serverName/mode 的错误、config 的 TypeError、toggle enabled 的 `undefined` 原值及 re-set `?? true` |

已知限制：`LoadedPlugin.mcp_servers` 是 `serde_json::Value`，默认对象映射按键排序，
无法保留插件 manifest 的 JS 插入顺序。整数键（数组索引）已由
`collect_plugin_mcp_names` 按数值升序排到普通键之前，与 `Object.keys` 的可计算部分
一致；普通键仍按 serde 的字典序给出，不覆盖任意 manifest 插入序。该字段由
A6.3-pr7b 的加载器填充，届时若需要任意键序，应在加载边界保留原始键序。
