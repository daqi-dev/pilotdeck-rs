# Gateway 已接受差异（草稿，Issue #23 / A2）

当前 **1 条已接受差异**（A4-FILE-TOCTOU-001，见文末）。透明代理的八组场景必须 100% 一致。归一化规则只处理运行元数据，不把代理行为差异写成豁免。

基线协议重连后 elicitation_respond.delivered=false 是两侧共同的已知业务行为，驱动明确断言并披露；不计作被测侧已接受差异，也不宣称重连后的轮次完成或 UI 自动恢复成功。

A1 的 RSV/continuation/unmasked codec 差异由 A2 原始 WS 探针实际判 FAIL，已通过透明字节隧道修正；修复前原件仍留作回归依据。没有登记允许差异来变绿。

未来追加条目必须包含类别（bug 修复/有意改进/不可避免）、基线依据、具体字段/场景、机器判据和批准记录；禁止概括性忽略整个 method、所有 error、所有时间字段。A4 本地应答的计分还须上游观测点证明 request 未转发，输出相等不能代替本地实现证明。

## A4-FILE-TOCTOU-001 · project_file_read 检查后符号链接替换（原版缺陷 · 安全）

- **类别**：bug 修复（意向书「bug 默认修」第③项：有安全问题）。
- **基线依据**（cc7c4e6，`src/` 下）：`cli/createLocalGateway.ts:1105-1107` 先 realpath 得 canonical 字符串并检查在项目根内；`:1109` 按该字符串 `stat`、`:1111` `readFile`，均默认跟随符号链接，没有根描述符、NOFOLLOW 或与检查绑定的 fd；`tool/builtin/filesystem/pathSafety.ts:113-116` 只检查路径关系。检查与读取之间若路径分量被替换成指向根外的符号链接，TS 会读到根外文件。
- **Rust 行为**：以项目根目录 fd 为锚，逐组件 `openat` + `O_NOFOLLOW`；检查后被替换的链接不会被跟随：按替换发生的时机报错或返回 `ok:true, result:null`，**从不返回根外内容**，也不回退给 TS。
- **范围**：仅 `project_file_read`（以及后续沿用同一读取路径的本地 method，须各自另登记）在「检查后、读取前路径被替换」的竞态下的结果。不竞态的正常路径（含根内绝对路径与 `..`）与 TS 逐字节一致（60/60 原件）。
- **机器判据**：`src/pilotdeck-gateway/src/project_files/tests.rs` 的 `checked_path_replacements_cannot_read_outside_or_fall_back_to_ts`——同一组 Node 实测竞态探针中，TS 读到 `OUTSIDE_SECRET`，Rust 必须本地应答（报错或 result:null）、不含根外内容且不转发；A2 透明代理与本地应答场景无竞态，仍须 100% 一致。
- **批准记录**：人定 2026-10-15（issue #68 选 A），见 `state/decisions-20261014.md`「人的回复」。

## A4-ARCHIVE-SEC-001 · native_archive_manifest 根外读取安全拒绝（原版缺陷 · 安全）

- **类别**：bug 修复（意向书「bug 默认修」第③项：有安全问题）。
- **基线依据**（cc7c4e6 `src/`）：transcript 读取链（`cli/createLocalGateway.ts:1183-1196`）realpath 后默认跟随符号链接，静态根外链接与检查后替换均读出根外内容（真实 Node RPC 9 场景原件：TS 读出根外 sequence 4242）。
- **Rust 行为**：canonical 双侧 containment + 从 `/` 起逐层 `O_SEARCH|DIRECTORY|NOFOLLOW` checked-open + fd fstat。两档拒绝：**非 regular file（FIFO/device，含 stat 后 descriptor 竞态变非常规）或 >2GiB → `PATH_NOT_ALLOWED` 本地拒绝**；**50MiB < size ≤ 2GiB → `SESSION_TRANSCRIPT_INVALID`**（stat 阶段的 EISDIR 等仍走既有 errno 白名单的 TS 一致错误）。根外链接/检查后替换 → `PATH_NOT_ALLOWED`。**从不返回根外内容，也不回退 TS。**
- **范围与披露**：合法外部存储布局（transcript 根软链到别处）同被拒绝——行为回归属登记语义家族（人批 2026-10-17）。不防硬链接/bind mount；注册目录枚举与 stat 元数据仍跟随链接（不读内容）。
- **机器判据**：`native_archive/security_tests.rs` 9 场景（normal/静态外链/受控替换 × 三 method）本地应答 + `pairing/tests.rs` 8 个 wire 级配对测试（normal 与 TS 逐字节相等；拒绝场景经上游 probe 零转发证明——observer fence 精确读 + silent + hits 计数）。
- **批准记录**：人批 2026-10-17「批准 #134 的登记范围」，见 `state/decisions-20261017.md`。

## A4-ARCHIVE-SEC-002 · native_archive_entries 根外读取安全拒绝（原版缺陷 · 安全）

同 A4-ARCHIVE-SEC-001（共享读取路径与原语；分页语义普通路径与 TS 逐字节一致，138 RPC fixture 原件）。按规约逐 method 登记。批准记录同上。

## A4-ARCHIVE-SEC-003 · native_archive_artifact 根外读取安全拒绝（原版缺陷 · 安全）

同 A4-ARCHIVE-SEC-001，差异两点：拒绝错误码为 `ARCHIVE_ARTIFACT_NOT_FOUND`（与该 method 既有 NOT_FOUND 语义一致，按 method 分别登记口径）；artifact 根为项目目录，读 tool-results。10MiB limit 契约与 TS 一致。批准记录同上。

## A6.5-MCP-OUTSCHEMA-REGEX-001 · MCP outputSchema 正则长串预算（不可避免 · 向严）

- **类别**：不可避免（SDK 用 Ajv 在 V8 上做正则匹配，回溯栈断点随正则结构与字符组成变化，Rust 的 `regex` 不回溯，无法逐点复刻）。
- **基线依据**（cc7c4e6，`@modelcontextprotocol/sdk` 的 `AjvJsonSchemaValidator`，Node 22.22.0）：含无界重复的 pattern、patternProperties 键名正则或带正则的 format，在超长字符串上让 V8 栈溢出，SDK `Client.callTool` 返回 `-32602 "MCP error -32602: Failed to validate structured content: Maximum call stack size exceeded"`。真实 Client 实测首溢约 210 万至 2500 万 UTF-16 单位（嵌套捕获约 210 万；`^(a|b)*$` 约 419 万；`.*` / `[^<>]*` 配 BMP 与星外字符混排约 1260 万；byte 约 447 万），原值 `_tmp/20261021-a65-pr10b-r9/RESULT-full.md:1283` 起与修复轮 9 原件。
- **Rust 行为**：`src/pilotdeck-gateway/src/runtime/mcp/output_schema.rs:2001` `MAX_REGEXP_STACK_UNITS = 1_048_576`；凡含无界重复（`*`、`+`、`{n,}`、大上界计数量词）的正则权重至少 1（权重取 pattern 分组数），`权重 × UTF-16 长度` 超过该值即返回与 SDK 栈溢出逐字相同的错误。无界重复之外的正则（纯字符类无量词、有界重复）不计入。
- **范围**：仅 MCP 工具 outputSchema 校验中的长字符串。① 向严区间：长度在端口上限与 SDK 首溢之间时 SDK 判 valid、端口报栈溢出；② message 差异：数据本应因普通校验失败（例如 hostname 格式错）而被拒时，超出预算后端口报栈溢出文案、SDK 报普通格式错误；③ 对 time、date-time、iso-*、duration 及不带分组的纯 BMP 重复，实测范围内 SDK 从未栈溢出，因此「超过 SDK 断点后两侧一致」对它们不成立，超预算后两侧结果按 ①② 区分。超过 1 MB 的输出校验不是 A6.5-pr10b 的验收范围（人定）。
- **机器判据**：`output_schema.rs` 的 `regexp_stack_model_covers_all_unbounded_atoms` 与 `replays_the_ajv_oracle`（845 条，其中正则长串按类在上限两侧、SDK 首溢之上各有样本，长串只存生成规则）。
- **批准记录**：人定 2026-10-21 13:10「PR #391 (A6.5-pr10b) 选 A；批准对应正则预算差异及 #4、#5……超过 1 MB 的输出校验暂不作为本 PR 的硬性验收要求，限制须明确记录」，见 `state/decisions-20261021.md`「人定 · 2026-10-21 13:10」；PR #391。

## A6.5-MCP-OUTSCHEMA-DEPTH-001 · MCP outputSchema 深度 / 宽度预算 2048（不可避免 · 向严）

- **类别**：不可避免（SDK Ajv 递归校验在 V8 上的栈断点随 schema 形状、调用栈深度与 JIT 状态变化）。
- **基线依据**（同上）：极深嵌套或很宽的 anyOf / allOf / dependencies 组合 schema、深比较（const / enum / uniqueItems）让 V8 栈溢出，SDK 返回上述 -32602 错误。30 组形状的真实 Client 首溢与端口支持点见 `_tmp/20261021-a65-pr10b-r9/RESULT-full.md:1474` 起（修复轮 10 §1），对正常工具的影响范围见 `_tmp/20261021-a65-pr10b-r9/depth-impact.md`。
- **Rust 行为**：`output_schema.rs:69` `MAX_VALIDATION_BUDGET = 2_048`（按 schema 静态节点与递归边加权计费，深比较共用余额）；超出即返回与 SDK 栈溢出逐字相同的错误。`MAX_SCHEMA_DEPTH = 256` 不变。
- **范围**：仅 outputSchema 校验。向严区间为「端口首拒 ≤ 活跃成本 < SDK 首溢」：此区间 SDK 判 valid、端口报栈溢出；代表性残余为同形查询 169 列以上、同形目录树 107 边以上提前拒绝；uniqueItems 在区间内 SDK 是普通重复项错误、端口为栈溢出文案（message 差异）。成本 ≤ 2048 两侧一致。
- **机器判据**：`output_schema.rs` 的 `recursive_keyword_shapes_share_one_weighted_budget`、`unused_wide_branches_spend_recursion_budget`、`non_schema_dependency_arrays_spend_static_budget`、`deep_comparisons_spend_the_shared_validation_budget` 与 `replays_the_ajv_oracle`（各形状在 2048 两侧有样本）。
- **批准记录**：人定 2026-10-21 14:25「pr10b depth budget 按照你的推荐选用2048。」，见 `state/decisions-20261021.md`「人定 · 2026-10-21 14:25」；PR #391。

## A6.5-WEBFETCH-DEPTH-001 · web_fetch HTML→Markdown 深度上限 256（不可避免 · 向严）

- **类别**：不可避免（TS 的断点取决于 V8 栈深，Rust 迭代实现无法逐点复刻）。
- **基线依据**（cc7c4e6）：web_fetch 默认 HTML 走 `src/tool/builtin/web/urlFetcher.ts` 的 `getURLMarkdownContent` → domino + Turndown 递归转换；嵌套过深时 V8 抛 `RangeError: Maximum call stack size exceeded`，web_fetch 以 `tool_execution_failed`「web_fetch failed: Maximum call stack size exceeded」失败。Node 22.22.0、真实 `createWebFetchTool({}).execute`、默认栈实测最低 DOM 深度断点约 1051–1059 层（div / span / blockquote / em / pre / 纯 ul、ol 约 1052 层；ul>li、ol>li 约 526 组；table 约 263 组），预调用后略高（约 1104 层）。原值与方法：`_tmp/20261021-a65-pr9b/RESULT.md:198` 起。
- **Rust 行为**：`src/pilotdeck-gateway/src/runtime/tools/builtin/web/html_dom.rs:434` `MAX_DOM_DEPTH = 256`（元素祖先深度，含隐式 html / body / Turndown 包装）；超过即返回与 TS 栈溢出时逐字相同的 web_fetch 错误（`:435` `DOM_LIMIT_ERROR`）。无节点数上限。
- **范围**：仅 HTML 内容的 web_fetch 转换。向严区间为 DOM 深度 257 至 TS 断点（约 1050 层）：此区间内 TS 成功、Rust 报上述错误；除 pr9c 待对齐的两条解析器版本差异外（`src/pilotdeck-gateway/tests/html_markdown.rs` 的 `parser_version_gaps_match_real_ts`），深度 ≤ 256 两侧逐字节一致；超过 TS 断点两侧都报同一错误。正常页面不受影响（评审抽样维基百科大页、MDN、Rust 文档、WHATWG、RFC 9110、ECMA-262 均成功）。
- **机器判据**：`src/pilotdeck-gateway/tests/html_markdown.rs` 的 `depth_budget_boundaries_and_review_shapes_match_real_ts`（256 层与真实 TS 逐字节一致、257 层按上述错误拒绝）与 `depth_budget_rejects_deep_non_list_shapes`；超过 TS 断点的样本两侧错误一致。
- **批准记录**：人定 2026-10-21 15:25「pr9b 选 A，批准 256 层限制及对应向严差异」，见 `state/decisions-20261021.md`「人定 · 2026-10-21 15:25」；PR #406。

## 已知未对齐（未接受，待 pr9c）

以下两条为解析器版本差异的透明披露，不是豁免，不属于已接受差异；均为浅层输入，不能由 A6.5-WEBFETCH-DEPTH-001 的深度批准覆盖。

| HTML 输入 | 真实 TS 输出（domino 2.2.0） | Rust 输出（html5ever 0.40.1） | 测试名 | 状态 |
|---|---|---|---|---|
| `<select><hr></select>` | 空串 `""` | `"* * *"` | `src/pilotdeck-gateway/tests/html_markdown.rs::parser_version_gaps_match_real_ts`（当前 ignore） | **未批准，非已接受差异**；待 pr9c 对齐 |
| `<p>a<search>b</search>c</p>` | `"abc"` | `"a\n\nbc"`（两个换行） | `src/pilotdeck-gateway/tests/html_markdown.rs::parser_version_gaps_match_real_ts`（当前 ignore） | **未批准，非已接受差异**；待 pr9c 对齐 |
| 240 个属性各异的 `<b a=i>` + `<p>` + 重复 `<span>x</span>` + 补齐 `x` + 240 个 `</b>`，精确补至 4 / 8 MiB；元素深度 < 256 | 4 MiB 本轮真实 TS 复测：`RangeError: Invalid array length`；8 MiB 同错（修复轮 7 原值）。2 MiB 本轮成功：`"**".repeat(240) + "x".repeat(149586) + "**".repeat(240)` | 成功返回 Markdown（向宽）；4 MiB：`"**".repeat(240) + "x".repeat(299390) + "**".repeat(240)`；8 MiB：`"**".repeat(240) + "x".repeat(598972) + "**".repeat(240)` | `src/pilotdeck-gateway/tests/html_markdown.rs::reparented_wide_subtrees_stay_fast` | **未批准，非已接受差异**；待 pr9c 对齐 |
| select 内块级元素（domino 旧 InSelect 忽略整类标签，html5ever 0.40 无 InSelect）；实测也含 img / br | `<p>a<select><div>b</div></select>c</p>` → `"abc"`<br>`<select><p>x</p></select>tail` → `"xtail"`<br>`<div>a<select><li>b</li></select>c</div>` → `"abc"`<br>`<div>a<select><table><tr><td>b</td></tr></table></select>c</div>` → `"abc"`<br>`<div>a<select><h1>b</h1></select>c</div>` → `"abc"`<br>`<div>a<select><ul><li>b</li></ul></select>c</div>` → `"abc"`<br>`<div>a<select><img src="x" alt="b"></select>c</div>` → `"ac"`<br>`<div>a<select>b<br>d</select>c</div>` → `"abdc"`<br>`<div>a<select><option><div>b</div></option></select>c</div>` → `"abc"` | `select-div-in-p` → `"a\n\nb\n\nc"`<br>`select-p` → `"x\n\ntail"`<br>`select-li` → `"a*   bc"`<br>`select-table` → `"a\n\nb\n\nc"`<br>`select-h1` → `"a\n\nb\n=\n\nc"`<br>`select-ul` → `"a\n\n*   b\n\nc"`<br>`select-img` → `"a![b](x)c"`<br>`select-br` → `"ab  \ndc"`<br>`select-option-div` → `"a\n\nb\n\nc"` | `src/pilotdeck-gateway/tests/html_markdown.rs::parser_version_gaps_match_real_ts`（当前 ignore，真实 TS 原值录入 `parserGaps`） | **未批准，非已接受差异**；待 pr9c 对齐 |

TS 原值保存在 `src/pilotdeck-gateway/tests/fixtures/html-markdown/oracle.json` 的 `parserGaps`；修复轮 5 显式运行上述 ignored 测试确认 Rust 原值，失败按未对齐保留，不计为通过。这两条若要改为接受差异，须另走人批程序。

修复轮 8 追加说明：上表第三条是 domino 搬移宽子树时触发 V8 数组上限的运行时行为，与前两条解析器版本差异不同，也与 A6.5-WEBFETCH-DEPTH-001 的深度批准无关。上文「以下两条」仅指原有两条解析器差异；上文「除两条解析器版本差异外，深度 ≤ 256 两侧逐字节一致」须同时排除本条已知未对齐，不能据此声称该深度内全部一致。真实 Node 22.22.0 / V8 12.4.254.21-node.33 本轮重新实测 2 MiB 成功、4 MiB 抛上述错误，二分所得首错尺寸区间为 (3,407,872, 3,670,016] 字节（3.25–3.5 MiB，区间宽 256 KiB；限本输入形状与运行时，不作为通用阈值）。Rust 本轮 2 / 4 / 8 MiB 均成功，2 MiB 与 TS 逐字节一致。TS / Rust 原值、无损输出表达与 SHA256 见 `_tmp/20261021-a65-pr9b/RESULT.md`「修复轮 8」。第三条同样未批准、非已接受差异，不新增豁免、不改判分语义；要改为接受差异须另走人批程序。

修复轮 9 追加说明：当前上表已披露 **4 条已知未对齐**：3 类解析器差异（原 select/hr、search，以及本轮 select 内标签家族）和 1 类宽子树运行时差异。原文「两条解析器差异」及「以下两条」仅描述修复轮 5 的原始样例，不代表当前总数或排除清单。`parserGaps` 现有 11 个真实 TS 样例（原 2 个 + 本轮 9 个），仍由 ignored 测试保留未对齐状态；**select 家族未穷举**，`span` 与纯文本两例实测一致，不据此推断其他标签一致。上述 4 条均不由深度批准覆盖，也不能据深度 ≤ 256 声称全部逐字节一致。第三条宽子树的 2 MiB 完整 TS 输出已无损录入 `large-oracle.json`，由 `large_shallow_pages_match_real_ts_bytes_and_stay_fast` 展开后逐字节断言，4 MiB 由 `reparented_wide_four_mib_succeeds` 仅断言 Rust 成功；4 / 8 MiB 与 TS 的运行时差异仍未批准，不新增豁免、不改判分语义。两侧原值及复现命令见 `_tmp/20261021-a65-pr9b/RESULT.md`「修复轮 9」。

## 已知未对齐（未接受，待人批）· A6.4 stats

以下两条为 A6.4-pr9b（PR #476）行为差异的透明披露，**未批准，非已接受差异**；不新增豁免、不改判分语义，待人批。补登依据：gate3 sibling P1（retention 差异此前只在代码注释与 fixtures README 自我披露，账本无条目）与 sibling P2（未知 `stats.baselineModel` ref 的 fatal 诊断未实现且未登记）。

### A6.4-STATS-RETENTION-001 · stats 账目保留只解析 `toISOString` 规范形（有意 · 向保留）

- **类别**：有意的向保留偏离（决策卡「A6.4-pr9b」选项 B：会删数据的路径宁可多留、不可误删）。
- **基线依据**（cc7c4e6 `src/` + Node 22.22.0 实测）：`src/router/stats/TokenStatsCollector.ts:297-302` 的 `isRetained` 用 `Date.parse(record.endedAt)`，只有 `NaN` 才保留；V8 还接受 date-only、无毫秒、数字偏移、RFC 2822、日历翻转（`2026-02-30`）、`24:00`、四位小数秒与非规范扩展年（`+000000`）。固定 `Date.now = 2026-03-03T00:00:00.000Z`、`retentionMs = 1000` 时，真实 collector 把这 8 类行**全部删除**，只保留 `Date.parse` 为 `NaN` 的 `-000000-…` 与 `010000-…`。探针与原始输出：`/tmp/probe-stats-retention.mts`，结果见 `_tmp/20261022-a64-pr9b/RESULT.md`「修复轮 3」。
- **Rust 行为**：`src/pilotdeck-gateway/src/runtime/model/stats.rs` 的 `parse_date_ms` 只接受 24 字节 `YYYY-MM-DDTHH:mm:ss.sssZ` 与 27 字节 `±YYYYYY-MM-DDTHH:mm:ss.sssZ`（后者限 `Date` 有效范围 ±8.64e15 ms）；其余返回 `None`，`is_retained_record` 按「无法解析 → 保留」处理。因此上述 8 类行 Rust 保留、TS 删除。
- **范围**：仅 stats JSONL 的 retention 判定与 compaction 重写结果（含重开重建）及其派生聚合/快照。collector 自己写入的 `endedAt` 恒为规范形，正常路径无差异；差异只在手写/历史/损坏 journal 行上出现，方向是多保留账目行，不会少留。
- **机器判据**：`stats_tests.rs::retention_keeps_non_canonical_rows_the_baseline_deletes`（同一组 8 行 + 1 行规范过期控制行，断言 Rust 保留 8 行、删除控制行）；`parses_the_canonical_to_iso_string_shapes` 钉住两种规范形与拒绝集；`token_stats` 的 `retention-expanded-years` 金样由真实 TS 生成，钉住规范扩展年与越界行两侧一致的部分。
- **批准记录**：无。决策卡 `state/decisions-20261022.md`「A6.4-pr9b」为「自决 · 待复核（AI 代批）」并选定 B；本条目按 gate3 P1 补登，`未批准，非已接受差异`。

### A6.4-STATS-BASELINEMODEL-001 · `stats.baselineModel` 未知 ref 不报 fatal（未实现，待后续单）

- **类别**：未实现（Rust 侧缺 `modelConfig` 输入），非有意语义豁免。
- **基线依据**（cc7c4e6 + Node 22.22.0 实测）：`parseRouterConfig.ts:791-811` 的 `optionalBaselineModel` 把 ref 交给 `consumeRef(..., modelConfig, diagnostics)`；未知 provider 时输出 `ROUTER_REF_PROVIDER_NOT_FOUND`（`severity: "fatal"`、`path: "router.stats.baselineModel"`）并从 `config.stats` 丢弃 `baselineModel`。探针原始输出见 `_tmp/20261022-a64-pr9b/RESULT.md`「修复轮 3」。
- **Rust 行为**：`runtime/model/router/config.rs:461` 的 `parse_baseline_model` 只校验形状（对象 trim 后非空 / `provider/model` 字符串），没有 `modelConfig` 输入，未知 ref 被原样接受并用于 baseline 成本。
- **范围**：仅 `router.stats.baselineModel` 指向未配置 provider/model 时——TS 以 fatal 诊断丢弃该字段，Rust 接受并可能算出不同的 `baselineCost` / `totalSavedCost`。已配置 ref 的正常路径两侧一致（`token_stats` 的 `configCases` 钉住）。
- **机器判据**：`runtime::model::router::tests::unknown_baseline_model_is_accepted_like_the_recorded_gap`（钉住现状，实现 fatal 诊断后会转红，提醒同步更新本条）。
- **批准记录**：无。`未批准，非已接受差异`；前置于 stats 配置接入 `modelConfig` 的后续单，届时补真实 TS 金样与 fatal 路径回放。
