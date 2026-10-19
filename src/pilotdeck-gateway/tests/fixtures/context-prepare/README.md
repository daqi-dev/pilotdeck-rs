# context prepare 金样

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，入口
`src/context/DefaultContextRuntime.ts`（`prepareForModel`），并直接加载
`src/context/instructions/InstructionDiscovery.ts`。`sourceSha256` 是入口的**完整
相对 import 闭包**（含 `PromptAssembler`、`MessageProjector`、
`PromptCacheCoordinator`、`toolPairIntegrity` 等），生成前后用共享
`bindSourceTree` 绑定基线 HEAD 的整棵 `src` Git blob，并断言基线工作树干净。
`localeCompare` 取 ICU 默认 locale，生成器断言为 `en-US`；Rust 回放固定同一
locale。

真实运行时注入：

- `now: () => new Date(clockMs)` — 每步时钟由用例给定；
- `promptCacheCoordinator` — 返回 `undefined` 的空实现，隔离缓存代次（本 PR 不含
  cache plan）；
- `promptContributions` — 用例可构造真实 `PromptContributionRegistry`，并调用真实
  `registerExtensionPromptContributions`；无 contributions 的用例保持 `undefined`；
- `compaction: {}` — 只避免构造 native compaction provider，`prepareForModel` 不读；
- 扩展 resolver — 由用例的 commands/skills/mcp 数组驱动；
- `InstructionDiscovery` — 真实 Node 文件系统 + 真实层级发现；
- `abortSignal` — 带 `abort` 的步骤构造真实 `AbortController`：`before` 在调用前
  中止，`section` / `variable` 由对应 provider 在返回前中止；其余步骤不传。
  provider 断言它收到的 `context.abortSignal` 就是该 controller 的 signal。

夹具项目树写在 `generate.mts` 并同时录进 oracle 的 `files`/`directories`（base64）
与 `symlinks`（路径 + 相对目标），Rust 回放据此在自己的独占临时目录重建，不共享
生成器目录。路径统一规范化为 `<fixture>`。生成器记录真实的
`process.platform` / `process.version`；Rust 运行时把二者作为 `Options` 注入，
因此金样不依赖 CI 的 OS / Node 版本。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8 \
  node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/context-prepare/generate.mts [output-parent]
cargo test --manifest-path src/Cargo.toml --locked --offline -p pilotdeck-gateway --test context_prepare
```

录用前生成两次并逐字节比较两份 `oracle.json`，再覆盖仓库金样；生成器每次用
`mkdtemp` 独占目录，不删除。

覆盖 40 个用例 / 58 步，Rust 精确断言条数并按序逐字节比较完整 `ModelContext`
（不排序键、无容差）：

- 默认 prompt 三种文档策略（web_search / 仅 web_fetch / 无网络工具）、权限/运行
  模式、附加工作目录、MCP 指令的 ICU 排序与 XML 转义、commands/skills 渲染；
- custom / append system prompt，`includeExtensionsWithCustomSystemPrompt`；
- `runtime_context` 消息过滤、`system_prompt` 与 `user_message` 两种 surface、
  运行时上下文消息插入到最后一个真实用户请求之前；
- 消息投影：`maxMessages` 工具对安全切窗（跳过 tool_result-only 消息）、
  compact checkpoint 下不截断、孤立 tool_result/media_reference 删除、缺失
  tool_call 的占位注入；
- instructions 多层发现：managed / user / user rules / project /
  `.pilotdeck` / project rules / local，root→cwd 链、目录内 UTF-16 默认排序、
  `SKILL.md` 与目录排除、去重、缺失、空文件、BOM/JS trim、无效 UTF-8 的
  U+FFFD 替换；
- instructions discovery 边界：rules 目录里的符号链接 `.md`（`Dirent.kind` 不是
  file，两侧都不读）、名字是 `x.md` 的子目录、`rules` 本身是普通文件（ENOTDIR
  按缺失吞掉）、cwd 在 projectRoot 之外时目录链仍按 root→`/`→cwd 走到 cwd；
- prompt-time 状态：跨日 date-update、同前缀下保留旧 update、prefix 改变后丢弃
  越界 update、`previewOnly` 不提交状态、full-compaction checkpoint 刷新日期；
- prompt contributions：extension adapter 的 `localeCompare` 排序、section/runtime
  context 的 order+name 排序、变量插值、complete section 覆盖、tool schema 克隆
  （只留 name/description/inputSchema）与按名排序、`promptGeneration`；
- contributions 错误：未知变量、缺失变量值（`cwd` undefined）、多个 complete
  section、重复 tool schema 的真实 TS 报错文本；动态 text provider 返回字符串时
  照常渲染并参与变量插值，自身抛错原样传播；只有 resolve 出非字符串才报
  `{label} must resolve to a string`；`captureToolSchemas` 在排序 / 多 complete
  检查 / 动态 provider 之前执行（重复 tool schema 与后两类错误并存时仍报 tool
  schema 错误）；
- contributions abort：`snapshot` 入口、`resolveText` provider 前后、变量 provider
  前后共五处 `throwIfAborted()`。`contributions-abort-before` 在信号已中止时仍带
  重复 tool schema 和会抛错的 provider；`contributions-abort-during-provider` 由
  section provider 中止并返回字符串（无注册变量，排除后续 pre-check 兜底）；
  `contributions-abort-during-variable` 先让静态 section 通过，再由变量 provider
  中止。生成器对这三个用例捕获的异常断言 `name=AbortError`、`code=20`、
  `message=This operation was aborted`（Node 22.22.0 的默认 DOMException 形状），
  oracle 记录真实 `error.message`；
- 边界输入：`customSystemPrompt: null`、`maxMessages: null`、`stepId` 0/7/null、
  `runtimeContextSurface: null` / 未知值、空 messages/tools。

负控至少八处：`previewOnly` 后的真实调用必须仍产生同一 date-update（若预览提交了
状态就会消失）；带 checkpoint 的 `maxMessages` 不截断（若忽略 checkpoint 会丢消息）；
无效 UTF-8 指令文件必须保留并替换为 U+FFFD（若按错误丢弃则 layers 少一层）；
动态 provider 抛错若仍被包装，报错文本会多出 `must resolve to a string:` 前缀；
tool schema 捕获若被挪到最后，多错误用例会先报 multiple-complete / provider 错误；
`contributions-abort-before` 若入口检查缺失或晚于 `captureToolSchemas`，会先报重复
tool schema；`contributions-abort-during-provider` 若缺 `resolveText` provider 后的
检查，会返回 `LEAK` 而不是中止；`contributions-abort-during-variable` 若缺变量
provider 后的检查，会返回 `LATE` 而不是中止。

切分点：内存附件（`MemoryAttachmentBuilder` / `MemoryResolver`，含
`memory_provider_error` 诊断与 abort 早退）留 **A6.6-pr5b**；prompt contributions
的 abort 检查已在本夹具覆盖。prompt cache plan（`cachePlan` / `cacheBreakpoints`）
不在本夹具范围，由缓存 owner 接入。运行时上下文只从 user-context、prompt
contributions 与 instructions 生成，`applyToolResults` 等 operation 仍走 A6.6-pr1
的默认 unsupported。
