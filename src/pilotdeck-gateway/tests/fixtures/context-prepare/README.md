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
- `promptCacheCoordinator` — 真实 `PromptCacheCoordinator`，包含 generation、preview、reset、release 与 dispose；
- `memoryResolver` — 注入可控 provider，真实 `MemoryAttachmentBuilder` 执行包装、JS trim、诊断、超时与中止；
- `promptContributions` — 用例可构造真实 `PromptContributionRegistry`，并调用真实
  `registerExtensionPromptContributions`；无 contributions 的用例保持 `undefined`；
- `compaction.autoCompact` — 注入 compacted/skipped 结果，验证真实 runtime 的 cache reset 接线；
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

覆盖 72 个用例 / 138 步，Rust 精确断言条数并按序逐字节比较完整 `ModelContext`
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
- `maxMessages` 的 JS number 边界（Node 22.22.0 实测）：`-1` 走截断路径丢弃全部
  7 条并告警 `Truncated 8 message(s) to respect maxMessages=-1.`（`droppedCount`
  可以大于消息数）；`-0` 等价于 0；`2.5` 在 `messages[4.5]` 读到 undefined，整体
  抛 `TypeError: Cannot read properties of undefined (reading 'content')`；
  `NaN` / `Infinity` 不截断；`-Infinity` 丢弃全部消息，warning 含 Infinity，`droppedCount` 在 JSON 输出为 null。JSON 不能表示这些数，生成器在 oracle 输入里用
  `{"$number": "NaN" | "Infinity" | "-Infinity" | "-0"}` 标记，Rust 回放重建；
  `{"$undefined": true}` 标记 undefined 元素，`{"$deepArray": N}` 标记 N 层嵌套
  数组（深 `inputSchema` 只以标记进 oracle，避免在记录侧重新走一遍深结构）。
  消息指纹的 `stableSerialize` 与 TS 逐字节同序，但用显式栈：超过 2400 层容器
  按 V8 同文 `RangeError: Maximum call stack size exceeded` 拒绝（确定性成功区
  取保守上界；Node v22.22.0 经真实 `buildCachePlan` 入口实测 1998/2001 层
  `inputSchema` 连指纹都成功、2525 层是最深成功、2526 层首次 RangeError，
  预热 JIT 后可超 4000；上界按容器总数取 2400，2000 会误拒 TS 接受的输入）。

负控至少八处：`previewOnly` 后的真实调用必须仍产生同一 date-update（若预览提交了
状态就会消失）；带 checkpoint 的 `maxMessages` 不截断（若忽略 checkpoint 会丢消息）；
无效 UTF-8 指令文件必须保留并替换为 U+FFFD（若按错误丢弃则 layers 少一层）；
动态 provider 抛错若仍被包装，报错文本会多出 `must resolve to a string:` 前缀；
tool schema 捕获若被挪到最后，多错误用例会先报 multiple-complete / provider 错误；
`contributions-abort-before` 若入口检查缺失或晚于 `captureToolSchemas`，会先报重复
tool schema；`contributions-abort-during-provider` 若缺 `resolveText` provider 后的
检查，会返回 `LEAK` 而不是中止；`contributions-abort-during-variable` 若缺变量
provider 后的检查，会返回 `LATE` 而不是中止。

本轮新增：

- memory 有内容、空白/缺省、provider 报错、provider 已截断文本与透传诊断；
  builder 不承担 provider 的检索/截断策略。注入参数核对 query（最新 user 的首个非空 text，保留原空白）、
  sessionId、projectRoot 覆盖/回退、投影后的 recentMessages。system/user surface、重复准备时去掉旧 runtime context；
- memory 2ms 超时、预先中止、provider 内中止并 resolve、provider 内中止并保持 pending。
  中止返回部分 context，跳过 instructions/cache/time-state commit；Rust 同时验证 child signal 被中止；
- cache 协议/布尔能力开关、空/全 system/1/3/超过 3 条消息断点、system 排除、user surface 插入后的索引；
  相同指纹复用 generation、消息变化增加、session 隔离、preview 不提交、不消费 reset、
  disabled commit 消费 reset、release/dispose 清代次、仅 compacted 触发 reset；
- cache 工具按 ICU 名称与 stableSerialize 二级排序（重复名、大小写、变音字符，逆序输入同指纹）；
  整个 cachePlan（包括 SHA-256）逐字节比对。cache 场景使用固定 `/pdrs-cache-fixture` cwd：
  临时目录进入哈希后无法再做路径规范化，这些场景不启用 instruction 文件 IO。

已有 41 个 case 保留真实 TS 输出；本次更换真实 coordinator 后，其旧输入没有启用 cache，旧输出不变。
新增/扩展行为的临时实现变异、原始失败日志与恢复后的回放记录见交付 `_tmp/20261022-a66-pr5b/RESULT.md`。

本轮（#495 评审遗留 P2）新增：

- `extractRecentUserText` 的畸形 text 块失败模式：`type:"text"` 缺 `text` /
  `text` 为数字 / null 时整个 prepare 以 TS 同文 TypeError 拒绝（provider 不会被
  调用），坏块在前也不跳过到后面的好块；非 text 块与纯空白 text 仍照常跳过；
  无 memory resolver 时该函数根本不会被调用，同样输入照常出 context；
- cache 工具排序的 `left.name.localeCompare` 失败模式：单工具不进比较器；
  只有比较器左操作数的 `name` 需要是字符串（index 0 的坏名只作右操作数被
  `String()` 强转，不抛错）；`null` 工具元素进比较器后在 `name` 读取处抛错，
  `undefined` 元素则被 JS `Array.prototype.sort` 挪到末尾、根本不进比较器。
  运行路径上 nullish 元素更早在 PromptAssembler 的
  `tools.some((tool) => tool.name === "web_search")` 处抛同文 TypeError
  （`cache-tool-undefined-element` 的 7 步全为该错误，见修复轮 1 的更正），
  排序语义本身由 host-context 夹具的 `cache-r1-*` 观测。金样含
  良构非 ASCII 名（emoji 在 ICU 中排字母前）与孤代理项 / 星面键（两侧都按
  `\ud800` 转义 / 原星面字符进入指纹的 stableSerialize 文本）。孤代理项
  **名**不进用例：Node ICU 给未配对代理项独立权重（实测 `\ud800` 排在字母后、
  `` 前），而 icu4x 两个比较通道都按 U+FFFD 处理，指纹必然分叉，登记
  为已知未对齐而不是钉进金样；
- 指纹算法收敛为单一 `cache_plan_identity`（coordinator 与 host 侧 finalize 共用），
  旧 58 case / 108 步逐字节不变即证两处行为未变。

本轮（#495 修复轮 1，评审 P1）新增：

- `cache-tool-undefined-element`（7 步）与 `cache-tool-eval-order`（5 步）：nullish
  工具元素在运行路径的失败点是 PromptAssembler 的 `name` 读取（`some` 按元素序
  扫描、命中即短路，故 `[{name:'web_search'},{name:'web_fetch'},null]` 类输入两轮
  扫描都短路成功后才轮到比较器——比较器对 `null` 仍报 `name` 读取错误）；
  比较器求值序按 JS：`left.name` 读取 → `localeCompare` 属性读取（nullish 名在此
  抛错）→ `right.name` 参数（nullish 元素在此抛错）→ 非字符串左名的调用检查；
- `cache-tool-schema-depth-1998` / `cache-tool-schema-depth-2001`：`inputSchema`
  嵌套 1998 / 2001 层数组经真实 `buildCachePlan` 出全指纹（fingerprint 逐字节
  比对），`$deepArray` 标记进 oracle，证明 2400 容器上界不误拒 TS 接受的输入；
  host 侧同深度的指纹由 `cache.rs` 单测 `deep_input_schema_fingerprints_match_real_ts`
  以两次 TS 探针指纹钉住（host-context 的 oracle 记录 indent-2 JSON 会膨胀到
  ~16MB，深 schema 不进那套夹具）。
