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

补实现（issue #494；PR 号待协调者创建后补记）：ISO 与 legacy 解析已接入共享的 V8 12.4 / Node v22.22.0 移植（`pilotdeck-transport/src/system_clock.rs`），保留完整 UTF-16，原先仅认两种规范形的限制已消除。真实 TS 的 `tests/fixtures/stats-dateparse/` 金样为 15,887 个输入 × UTC / America/New_York / Asia/Shanghai，覆盖历史 TZif 转换边界、春秋跳时、未来 400 年周期样本、扩展年份与 malformed 输入；三个时区逐值保留、边界、压缩字节和重开快照一致。原机器判据由 `stats_dateparse_tests.rs` 的真实 TS 回放取代，保留本条历史原文。

仍保留为本条已接受差异的子集：**本地时间需要的 TZif 字节不在 `tests/fixtures/stats-dateparse/local-profiles.json` 的已对拍 SHA256 中**（含系统升级、更换未验证时区、POSIX TZ、自定义路径、数据不可读），以及共享 TZif 解析器无法确定转换的本地输入，一律保留。进程设置 TZ 时沿用该 TZ，未设置时读取 `/etc/localtime`；仅接纳三个已验证 TZ 标识与同名 TZif 的 SHA 配对；未设置 TZ 时，`/etc/localtime` 的规范链接须指向其中同名 profile 且字节匹配；未验证别名（如 Node 按 UTC 解释的 `TZ=localtime`）继续保留。显式时区、ISO date-only 不受该限制。Node ICU tzdata 为 2026a，生成机器系统为 2026d，因此不宣称任意系统规则与 ICU 一致；未知数据不猜 UTC/当前偏移，不误删。该子集沿用本条人批的向保留差异，尚未全量消除，故不追加虚构 PR 号或「全部差异消除」结论。

### A6.4-STATS-BASELINEMODEL-001 · `stats.baselineModel` 未知 ref 不报 fatal（未实现，待后续单）

- **类别**：未实现（Rust 侧缺 `modelConfig` 输入），非有意语义豁免。
- **基线依据**（cc7c4e6 + Node 22.22.0 实测）：`parseRouterConfig.ts:791-811` 的 `optionalBaselineModel` 把 ref 交给 `consumeRef(..., modelConfig, diagnostics)`；未知 provider 时输出 `ROUTER_REF_PROVIDER_NOT_FOUND`（`severity: "fatal"`、`path: "router.stats.baselineModel"`）并从 `config.stats` 丢弃 `baselineModel`。探针原始输出见 `_tmp/20261022-a64-pr9b/RESULT.md`「修复轮 3」。
- **Rust 行为**：`runtime/model/router/config.rs:461` 的 `parse_baseline_model` 只校验形状（对象 trim 后非空 / `provider/model` 字符串），没有 `modelConfig` 输入，未知 ref 被原样接受并用于 baseline 成本。
- **范围**：仅 `router.stats.baselineModel` 指向未配置 provider/model 时——TS 以 fatal 诊断丢弃该字段，Rust 接受并可能算出不同的 `baselineCost` / `totalSavedCost`。已配置 ref 的正常路径两侧一致（`token_stats` 的 `configCases` 钉住）。
- **机器判据**：`runtime::model::router::tests::unknown_baseline_model_is_accepted_like_the_recorded_gap`（钉住现状，实现 fatal 诊断后会转红，提醒同步更新本条）。
- **批准记录**：无。`未批准，非已接受差异`；前置于 stats 配置接入 `modelConfig` 的后续单，届时补真实 TS 金样与 fatal 路径回放。

人批（2026-10-22，用户本人在协调者会话）：A6.4-STATS-RETENTION-001、A6.4-STATS-BASELINEMODEL-001 两条**接受为已知差异**，但要求 Rust 补实现：BASELINEMODEL 见 https://github.com/daqi-dev/pilotdeck-rs/issues/493，RETENTION（按 V8 dateparser 逐格式移植并用 TS 金样对拍）见 https://github.com/daqi-dev/pilotdeck-rs/issues/494。补齐后在本节追加「已实现，差异消除」。

已实现，差异消除（本 PR 修复轮 4）。`runtime/model/router/config.rs` 的 stats 投影已从同一 snapshot 读取 `modelConfig`，对 `baselineModel` 及 `modelPricing` 每个对象条目的键调用引用校验：未知 provider/model 与非法引用均按 TS 输出 fatal 并丢弃字段/条目；引用诊断入口为 `RouterConfig::from_snapshot_with_ref_diagnostics`。真实 TS 金样及逐字节诊断回放见 `tests/fixtures/token-stats/` 与 `stats_config_projection_matches_real_ts`，同步消除 gate6 sibling P1 的 modelPricing 同源遗漏；原记录保留作为历史。

## A6.4-STATS-SIDECAR-001 · 降级追加写旁路文件（向安全偏离 TS，人批 2026-10-22）

- **类别**：bug 修复（意向书「bug 默认修」第①项：丢数据），向安全偏离。
- **TS 依据**（cc7c4e6）：`src/router/stats/TokenStatsCollector.ts:258-259` 在 retention 锁的 25×1ms 尝试失败后直接追加主 JSONL；`:316-327` 的压缩在读完主文件后 rename 替换主文件，两者交错会覆盖新追加行。`tests/fixtures/token-stats-sidecar/generate.mts` 在真实 TS 压缩 rename 前插入另一真实 collector 的 observe，`oracle.json.tsRace` 保留替换前含新行、替换后丢行的原始字节及快照。
- **Rust 行为**：仅 retention 写入锁失败（或遗留合并事务恢复失败）时，将完整 JSONL 行写到 `stats.jsonl.pending/<纳秒时间>.<pid>.<uuid>.jsonl`；临时文件写完并 sync 后原子发布，已发布文件不再修改。无锁加载同时读取主文件与旁路；持锁压缩沿用现有保留判断与序列化，将本轮可见旁路并入主文件。`stats.jsonl.merge` 中的名单与替换文件硬链接先原子发布，主文件 inode 与该硬链接相同即已提交；已提交名单不重复计账，旁路删除并 sync 后才原子退休事务凭据。下次持锁操作先续完遗留事务，避免重放或覆盖后续追加。
- **范围**：只修 retention 压缩与降级追加交错的丢账窗口，及其必要的无锁重建、崩溃恢复、clear 清除旁路；账目内容、聚合输出、保留判断与压缩过滤不变。不按账目内容去重，相同内容的两次观察仍计两笔。既有实例继续沿用 TS 的内存缓存快照语义，新建/重开的无锁实例可看见已发布旁路；不增加跨实例实时刷新。沿用 best-effort IO 及既有锁重试/遗留锁处理，不扩展为显式 clear 并发语义或混用不同 retention 配置的协调重构。
- **机器判据**：`runtime/model/stats_journal_tests.rs` 的 `sidecar_interleaved_compaction_and_independent_writers_never_lose_or_duplicate`（三个压缩阶段，各三轮，四个独立写实例，总计 32 行逐笔无丢无重）；`sidecar_crash_boundaries_recover_exactly_once`（发布前、准备后、提交后、删一条、全删、凭据退休六处恢复，同内容独立观察及晚到旁路）；`sidecar_recovery_precedes_a_locked_append`；`sidecar_snapshots_merge_and_clear_match_real_ts`（新建无锁读者的聚合、分时、模型、最近记录、session 与主文件字节对拍真实 TS 金样，含 clear）。每类行为负控及完整命令输出见 `_tmp/20261022-issue-496/RESULT.md`。
- **批准记录**：用户本人 2026-10-22 选 B，明确「Rust 偏离 TS 修掉：抢锁失败时，降级追加先写到旁路文件；持锁压缩时把旁路文件合并回主文件。只修这个丢数据窗口，其他行为不变」。决策卡：主仓 `_tmp/20261022-loop/card-stats-race.md`；实现 issue #496。本条按该授权登记；独立评审由协调者续办。

## #502 集中补登 · 已知未对齐（未批准）

以下条目只披露现状，**均非已接受差异，不增加豁免，不改变判分语义**。旧决策卡的「AI 代批，待人复核」不等于人批。TS 根均为固定 `cc7c4e6` 的 `src/`；本单真实 Node 22 探针与源码绑定见 `src/pilotdeck-gateway/tests/fixtures/diff-backfill/`。在途 `runtime/turn/`、`runtime/memory/edgeclaw/` 只列总表，不改文件。完整扫描、误报分类、低成本对齐及证据见本单 RESULT。

### A6-JSVALUE-DEPTH-001 · 固定 wire 值深度预算

- **类别**：实现限制（用固定预算近似 V8 调用栈）。
- **TS 依据**：`extension/skills/SkillManager.ts:199,207` 使用 JS String/JSON；`cli/ProjectRuntimeRegistry.ts:963` 使用 structuredClone；V8 断点随操作和栈变化。本单 `oracle.json.arrays` 原生 String 实测：2000、2001、2100 层成功为 `7`，3000 层为 RangeError；不沿用旧注释的 2516 为精确断点。克隆/序列化另见 `state/decisions-20261019.md:3-11` 的 structuredClone 探针，不把 String 探针当作其阈值证据。
- **Rust 行为**：`js_value.rs` 的 `MAX_ARRAY_DEPTH=2000` 分别用于数组 String/Number 转换与 JSON/克隆深度检查，超限拒绝；`runtime/mcp/naming.rs` 的递归值变换也使用该常量。各操作计层方式以实现和边界测试为准，不声明共享同一 V8 阈值。
- **范围**：仅触发这些检查的深 wire 值；String 存在 TS 成功而 Rust 报 RangeError 的区间，structuredClone 也可能 TS 先失败。正常浅值不豁免，不把所有深 JSON 一概忽略。
- **机器判据**：`js_value::tests` 的深度边界测试、`tests/tool_runtime.rs::string_conversion_depth_bound_is_shared_js_value_limit`；TS `diff-backfill/oracle.json.arrays` 锁定本次 String 对照。将来对齐时须分别测真实入口与调用深度，不能仅抬高常量。
- **批准记录**：无。2026-10-19 决策仅 AI 代批；状态为**已知未对齐（未批准）**。

### A6.4-JSONREPAIR-DEPTH-001 · 工具参数修复深度 200

- **类别**：实现限制（递归栈预算）。
- **TS 依据**：`model/providers/anthropic/stream.ts` 导入基线安装的 jsonrepair 3.14.0；本单从该 import 解析真实依赖并校验既有 jsonrepair 金样登记的包哈希。`diff-backfill/oracle.json.repairs` 的 199/200/201/205/250 个未闭合 `[` 均成功修复，不把 V8 的栈断点写成固定数字。
- **Rust 行为**：`runtime/model/jsonrepair.rs` 的 `MAX_DEPTH=200` 超限返回 RepairError；`providers/mod.rs::parse_tool_arguments` 在原始 JSON 也无法解析时返回 None，流调用方走各自失败分支。
- **范围**：仅需要修复且达到 Rust 修复深度预算的参数；原本合法、可由迭代 JSON parser 直接解析的深 JSON 不经过 jsonrepair，不能声称也会被此预算拒绝。共享修复器被 Anthropic/OpenAI/Responses 使用，不限 Anthropic。
- **机器判据**：`jsonrepair::tests::depth_limit_fails_instead_of_overflowing_the_stack` 及 `jsonrepair_tests.rs` 真实 TS 回放；追加 TS 深度探针在上述 oracle。精确边界按 parser 的 parse_value 计费，不能把 200 简称为所有形状都恰好能收 200 个容器。
- **批准记录**：无。`state/decisions-20261020.md` 的移植选型不是本预算的人批；**已知未对齐（未批准）**。

### A6.6-SNAPSHOT-IO-001 · 未覆盖 errno 的错误文案回退

- **类别**：实现限制（Node 错误文案覆盖不全）。
- **TS 依据**：`storage/workspaceSnapshot.ts:260-279` 将实际 Error.message 写入失败标记；Node 的 fs 错误由 libuv/运行平台生成。
- **Rust 行为**：`storage/workspace_snapshot.rs::io_message` 对 `node_io` 已测量表内 errno 使用 Node 文案；表外回退 `io::Error::to_string()`，可能写入与 TS 不同的 `_FAILED.json.error`。
- **范围**：仅 snapshot 捕获实际遇到表外 OS 错误时的失败文案；已测量 errno、成功 manifest、JSON 语法错误不在此差异中。不宣称当前平台必能制造每个表外错误。
- **机器判据**：`tests/workspace_snapshot.rs::scenarios_match_ts_outcomes_and_trees` 覆盖现有错误表；表外差异的验收应对同一平台、同一可注入 errno 比较失败标记原始字节，**目前没有表外真实 OS 错误的完整对拍，不能当已通过**。
- **批准记录**：无；**已知未对齐（未批准）**。


### A6.6-SNAPSHOT-COLLATION-001 · 孤代理 createdAt 排序拒绝

- **类别**：实现限制（ICU4X 与 Node ICU 对 ill-formed UTF-16 权重不同）。
- **TS 依据**：`storage/workspaceSnapshot.ts:106` 用 createdAt.localeCompare 降序；真实 TS 金样 `workspace-snapshot/oracle.json.hostBoundaryCases.unpairedSurrogateCollation` 给出含孤代理、U+FFFD、PUA 的 list 顺序。
- **Rust 行为**：`storage/workspace_snapshot.rs` 仅在实际需要比较且 key 含孤代理时返回 requires_host 错误，文案 `Snapshot createdAt collation requires the Node host for unpaired surrogates.`，不返回错误顺序。
- **范围**：多个 descriptor 的 createdAt 排序实际触发该比较；单项 list 不调用比较器，仍原样返回孤代理。当前 storage API 的错误不是已接线的自动 TS 回退，不能声称用户请求必能成功。
- **机器判据**：`tests/workspace_snapshot.rs::unpaired_surrogate_collation_requires_host` 同时断言 TS 顺序、Rust 错误及单项例外。
- **批准记录**：无；旧 R19 评审边界编号不是人批；**已知未对齐（未批准）**。

### A6-COLLATION-PUA-001 · 孤代理与真实私用字符的权重碰撞

- **类别**：实现限制（Unicode 排序近似）。
- **TS 依据**：Node 22 的 localeCompare 为孤代理保留独立权重；本单 `diff-backfill/oracle.json.collation` 实测 U+D800 < U+E000、U+D801 < U+E001。
- **Rust 行为**：`project_files.rs::node_collation_units` 将孤代理 U+D800..DFFF 映到 U+E000..E7FF，再交给 ICU4X。相应真实 PUA 与映射值碰撞，比较可能为 Equal，而 TS 非 Equal。
- **范围**：所有使用 `node_compare_utf16` 的排序，在决定顺序的位置同时出现真实 PUA 和相应孤代理时；两个孤代理相互比较仍保持递增，合法 Unicode 对合法 Unicode不受该映射影响。snapshot 的显式拒绝另见上一条，不能混为一谈。
- **机器判据**：`project_files::tests::node_compare_utf16_keeps_unpaired_surrogate_weights` 保障已有孤代理内部顺序；碰撞对照为上述 Node 探针与 `node_compare_utf16(collator, [D800], [E000]) == Equal`。完整跨 locale 碰撞矩阵尚未验证，不声明已对齐。
- **批准记录**：无；A6.6-pr10a RESULT 的 trade-off 不是人批；**已知未对齐（未批准）**。

### A7-ADMISSION-FENCE-001 · 相同 runId 的过期清理身份栅栏

- **类别**：有意改进（代际身份防护，尚无人批）。
- **TS 依据**：`gateway/SessionRouter.ts:241-245` 的 endTurn 只检查 runId 真值和字符串相等；旧消费者持相同 runId 可清理后来复用该字符串的 reservation。
- **Rust 行为**：`runtime/session/admission.rs::end_turn` 对非空 runId 还要求 begin_turn 返回的 Arc 身份相同；None/空 ID 仍不受栅栏限制。end_identity 也按代际清理。
- **范围**：close 后复用同一 runId 且旧消费者迟到清理；不改变正常当前身份的释放或无 ID/空 ID 的 TS 真值语义。
- **机器判据**：`runtime::session::tests::exact_end_requires_original_admission_identity_but_empty_end_is_unfenced`，旧身份不得释放新 reservation，当前身份可释放。
- **批准记录**：无；**已知未对齐（未批准）**。本单只补注释引用，不移除安全防护。

### A6.2-TRANSCRIPT-LOCK-001 · Rust 单写者锁与硬链接别名

- **类别**：有意改进及实现限制（单写者持久化契约）。
- **TS 依据**：`session/persistence/JsonlSessionPersistence.ts:20-38` 的 append 不获取 Rust 的 `<canonical path>.lock`；它不会与 Rust advisory lock 协调。
- **Rust 行为**：`storage/transcript.rs::JsonlLedgerStore` 持有 OS advisory lock 到 Drop，并以 sibling 文件原子替换。不同硬链接路径各有锁；替换一个路径后另一个仍指旧 inode，历史分叉，不支持别名协调。
- **范围**：TS/Rust 混合并发写入，以及同 inode 的不同硬链接名；切换前须停止旧 writer。不能把 Rust 对 Rust 同路径锁保证外推为跨 TS 或硬链接安全。不涉及普通单写者的 JSONL 内容差异。
- **机器判据**：现有 transcript 持久化和互续测试覆盖正常路径；混写/硬链接验收须两个独立 writer 持同 inode 的不同名字交错 append/replace，比较两名原始字节与锁结果，**该矩阵尚未完整对拍**。
- **批准记录**：无；**已知未对齐（未批准）**。

### A6.3-CONFIG-POLL-001 · fs.watch 被轮询采样替代

- **类别**：实现限制（宿主文件监视机制）。
- **TS 依据**：`pilot/config/PilotConfigStore.ts:107-139` 对现存路径/缺失路径的 parent 使用 fs.watch，通知后 debounce（默认 250ms）。
- **Rust 行为**：`config/store.rs` 默认每 20ms 采样 metadata 和至多 CONTENT_SAMPLE_LIMIT 的文件内容，再 debounce。采样额外延迟、跨真实 debounce 边界的合并和采样间净零变化遗漏，均见模块文档。
- **范围**：仅 watcher 触发的 reload 次数、时机、version/changedPaths；手动 reload 不受影响。净零遗漏需内容和采样 metadata（含 ctime）均相同；大文件超出内容样本、目录其他项只按 metadata，不保证所有文件系统均能发现瞬态变化。
- **机器判据**：`tests/config_store.rs::config_store_replays_real_typescript_oracle` 保障已覆盖 reload 行为；真实短突发验收须在同一文件系统记录 fs.watch 通知和 Rust 采样时间线，比较 version 和 changedPaths，**尚未穷举 tick 相位/ctime 粒度**。
- **批准记录**：无；**已知未对齐（未批准）**。

### A7-PROJECT-DISPOSE-001 · 项目关闭等待所有租约代际

- **类别**：有意改进（显式关闭契约）。
- **TS 依据**：`cli/ProjectRuntimeRegistry.ts:391-411,1569-1588` dispose 遍历发布的 runtime，跳过 sessionLeases>0 的 runtime。
- **Rust 行为**：`runtime/project.rs::dispose` 关闭 admission，排空包括 invalidate/reload 移出 cache 的所有仍存活 generation，等待租约释放和 disposal；取消调用方不取消清理。
- **范围**：dispose 时有未释放租约或 retired generation；Rust 返回可能更晚，租约一直不释放可一直等待。正常无租约关闭的资源顺序仍按 TS，不据此豁免其错误或顺序。
- **机器判据**：`runtime::project_tests::concurrent_acquire_and_dispose_drain_invalidated_generation_once`、`cancelled_release_and_dispose_are_idempotent_and_keep_cleanup_alive`。
- **批准记录**：无；任务的实现契约不等于行为差异人批；**已知未对齐（未批准）**。

### A6.5-MCP-DISPOSE-001 · MCP provider 关闭等待租约

- **类别**：有意改进（资源所有权）。
- **TS 依据**：`mcp/runtime/ProjectMcpRuntimeProvider.ts:125-129,150-174` stopAll 直接 stopEntry，stopEntry 等 ready 后即 stop，不等待 leases 变零。
- **Rust 行为**：`runtime/mcp/registry.rs::ProjectMcpRuntimeProvider::dispose` 先 draining、拒绝新租约，已有租约清零后才 stop 各 generation。
- **范围**：dispose 与已有 MCP 租约交错时，工具可使用时间与关闭完成时刻不同；不释放租约会延迟完成。无租约路径不在差异范围。
- **机器判据**：`tests/mcp_runtime.rs::dispose_drains_all_existing_leases_before_stopping`、`lease_dropped_on_plain_thread_unblocks_dispose_once`。
- **批准记录**：无；**已知未对齐（未批准）**。

### A6.5-TOOLS-SCHEDULE-001 · 并发组额外要求只读

- **类别**：有意改进（避免丢弃并发 future 截断写入）。
- **TS 依据**：`tool/scheduler/ConcurrentToolScheduler.ts:34-53` 仅检查 isConcurrencySafe；真实 `tool-runtime/oracle.json` 的 `non-readonly-safe-*` 记录所有 safe 工具并发启动。
- **Rust 行为**：`runtime/tools/scheduler.rs` 额外要求 isReadOnly 真值；只对 safe 工具查询 isReadOnly，查询抛错按 false。safe 但非只读的工具进串行组。
- **范围**：多调用 Concurrent batch；单调用和 Sequential 不查询该分组谓词。不出错时结果去时间字段后相同，但执行交错和时间戳不同；并发组先失败时串行工具可能根本不启动。
- **机器判据**：`tests/tool_runtime.rs::non_readonly_safe_tools_run_sequentially`；原 TS 输出不被覆盖，特殊场景走显式 Rust 期望而非通用相等回放。
- **批准记录**：无；fixture README 的理由不是人批；**已知未对齐（未批准）**。

### A6.5-TOOLS-CANCEL-001 · 首错后丢弃已启动兄弟

- **类别**：有意改进（禁止脱离所有权的工具任务）。
- **TS 依据**：`tool/scheduler/ConcurrentToolScheduler.ts:46-53` Promise.all 首错拒绝，兄弟仍后台完成；`tool-runtime/oracle.json` 的 `concurrent-error-lets-started-siblings-finish` 录下 settledAt 后的 end 事件。
- **Rust 行为**：首错轮将各兄弟 poll 一次后丢弃未完成 future，立即返回错误；外层取消/deadline/Drop 同样回收。并发组只读约束见上一条。
- **范围**：并发 batch 的失败/取消后的后续事件、副作用和资源存活；不改变成功 batch 的结果 slot 顺序。取消语义仅保证合作式 future 丢弃，不扩展到不受所有权约束的外部工作。
- **机器判据**：`tests/tool_runtime.rs::concurrent_error_rejects_immediately_and_drops_started_siblings`、`dropping_batch_reclaims_all_noncooperative_tool_futures_immediately`，以及首错 TS 金样逐事件对照。
- **批准记录**：无；**已知未对齐（未批准）**。

### A6.4-MODEL-PROTOTYPE-001 · 模型请求校验忽略默认原型成员

- **类别**：实现缺口（JS 属性查找）。
- **TS 依据**：`model/request/validateModelRequest.ts:15-27` 用普通下标访问 providers/models；本单 `diff-backfill/oracle.json.validation`：provider=toString/__proto__/constructor 时读 models 后 TypeError；model 取同名值且无附加能力检查、messages=[] 时返回成功。
- **Rust 行为**：`runtime/model/providers/mod.rs::own` 仅 own-property 查找，这些名字缺省时报 provider_not_found/model_not_found。
- **范围**：绕过正常模型引用过滤、直接交给该校验器的默认 Object.prototype 成员名；同名自有属性仍按自身内容处理。与已修复的 token count 属性查找不同，不把 count 的对拍结论外推到生成请求校验。
- **机器判据**：同一 oracle.validation 六例应经 validate_model_request 回放 TS 的异常/成功形状；**当前实现仍已知不等，未新增相等断言冒充通过**。
- **批准记录**：无；**已知未对齐（未批准）**。

### A6.4-OPENAI-URL-001 · 直接 builder 非字符串 URL 的宽容处理

- **类别**：实现缺口（JS 方法调用错误）。
- **TS 依据**：`model/providers/openai/request.ts:370` 的 provider.url.trim()：非字符串抛 TypeError；null/undefined 为读取 trim 失败，数字/对象为 trim 非函数。正常配置由 `model/config/parseModelConfig.ts:91` 选字符串或 catalog 默认值。
- **Rust 行为**：`runtime/model/providers/openai.rs::google_openai_compatible` 对非字符串先 String(url)，一般返回 false 而非 TS 异常；id=google 的提前返回与 TS 同路径，不列为差异。
- **范围**：仅 protocol=openai、id!=google 且绕过配置规范化的直接 builder 调用；不影响本单已对齐的 TokenAccountingRuntime count URL 路径。修改该 bool API 需传播 builder 错误，未借本单扩写重构。
- **机器判据**：现状由 `providers::openai::tests::google_compatible_non_string_url_is_false_not_panic` 钉住；对齐时用真实 TS builder 的同一 request/config 回放 TypeError（包括 null/undefined 文案），不得只改注释。
- **批准记录**：无；**已知未对齐（未批准）**。

### A6.7-HOOK-HTTPS-001 · HTTP hook 无 TLS 连接器

- **类别**：未实现（HTTPS transport）。
- **TS 依据**：`extension/hooks/execution/HttpHookExecutor.ts:9,18-26` 默认使用全局 fetch，可注入 fetchImpl；Node fetch 支持 HTTPS。
- **Rust 行为**：`runtime/hooks/exec.rs` 使用 plain HTTP hyper client，HTTPS 初始 URL 或重定向到 HTTPS 报 `Unsupported HTTP hook URL scheme: https`，不能执行正常 TLS 请求。
- **范围**：仅 HTTP hook 的 HTTPS 链路；普通 HTTP 的 redirect/headers/JSON 规则不豁免。model/web_fetch 已有 TLS 不代表 hooks 已接入。
- **机器判据**：在受控本地 TLS 服务上回放同一 hook，TS 成功/Rust 当前失败；**本单未跑 TLS listener，不把源码核实当作该网络验收已通过**。后续 #345 接入时补真实 TS 金样与负控。
- **批准记录**：无；#345 为实现跟进而非批准；**已知未对齐（未批准）**。

### A6.7-HOOK-RESOURCE-001 · condition 序列化与空重复正则预算

- **类别**：实现限制（固定预算近似 V8 栈）。
- **TS 依据**：`extension/hooks/config/matchHookCondition.ts:23` JSON.stringify 后 includes；真实 `hooks-config/probe-resources.mts` 在 HookRuntime.run 的 +0/+32 帧上实测 condition 6500 层成功、7000 失败；动态空重复 2M 工作样本成功、40M 样本 RangeError 后 matcher false，原件见 fixture README。
- **Rust 行为**：`hooks/config.rs::MAX_STRINGIFY_DEPTH=4000` 超限抛 RangeError；`hooks/regexp.rs::MAX_EMPTY_REPEAT_WORK=2_000_000` 超限整次 matcher false，负向断言不能翻转。两类计费与耗尽结果不同。
- **范围**：实际走到的条件容器层数/空重复展开成本；Rust 首拒到 V8 首拒之间会提前失败，不声称所有表达式存在统一 V8 阈值；消费型回溯未加预算，不属于本条。
- **机器判据**：`tests/hooks_config.rs` 的真实 TS oracle 回放与资源边界测试；`hooks-config/probe-resources.mts` 原生重测；两种预算必须分别检查上界/上界+1，不能混用单位。
- **批准记录**：无。`state/decisions-20261017.md`「V8 资源耗尽边界怎么对齐」仅 AI 代批，夹具「已接受不可判定带」文字不是人批；**已知未对齐（未批准）**。

### A6.5-MCP-OUTSCHEMA-SUPPORT-001 · 部分合法 JS pattern 不能求值

- **类别**：未实现（JS 正则覆盖）。
- **TS 依据**：SDK Ajv 对合法 `/u` lookaround/backreference 可编译并求值；既有 `output-schema/ajv-oracle.json` 与 output_schema 的真实 Ajv 回放提供普通行为依据，不能把预算批准扩为功能豁免。
- **Rust 行为**：`runtime/mcp/output_schema.rs` 编译时先验合法 JS 语法，实际用到不能翻译的 pattern 才返回 ValidationError::Unsupported。无 base 的相对/远端 `$ref` 同 Ajv 报 unresolvable，不能另算差异。不静默判 valid。
- **范围**：实际命中不支持语法、歧义字符类或 Unicode 属性的 schema；未命中的 pattern 不提前失败。预算上限另见已批 REGEX-001/DEPTH-001，本支持缺口未获其授权。
- **机器判据**：`output_schema::tests::unsupported_patterns_fail_at_validation_instead_of_passing` 用 `^(?=a)` + `a` 钉住 TS 可判断而 Rust Unsupported；其余类别须按具体 schema 补齐真实 SDK 样本，当前不宣称全覆盖。
- **批准记录**：无；**已知未对齐（未批准）**。

### A6.5-BACKGROUND-PERSIST-001 · 后台输出 spill 与快照恢复缺失

- **类别**：未实现（可选持久化能力）。
- **TS 依据**：`task/storage/TaskOutputStore.ts:29-35,176-245` 接受可选 spillDir 并可恢复；`task/runtime/BackgroundTaskRuntime.ts:71-89,487` 接受 diskSpillDir/snapshotStore 并恢复历史任务。默认无这些选项时也是内存 ring。
- **Rust 行为**：`runtime/background.rs` 只有默认 1,000,000 字节 ring，无 disk spill/snapshot storage 接口，不能产生恢复的 unknown 任务。
- **范围**：仅启用可选 spill/snapshot 的宿主配置、超出 ring 的历史输出读取与重启恢复；**不能称默认 TS 有无限输出而 Rust 截断**。异步 spawn stop 修复已在 `specs/parity/accepted-diffs.md` #5 人批，不与本缺口合并。
- **机器判据**：既有 `tests/background_tasks.rs` 回放只证明无持久化配置的正常路径；持久化验收须 TS 配置 spill/snapshot、写超 ring、重建 runtime 后比较读取/状态，**该能力未实现，尚无 Rust 对等通过证据**。
- **批准记录**：无；**已知未对齐（未批准）**。

### A7-HOST-RESOURCE-001 · 宿主队列上限与本地 watchdog

- **类别**：有意改进（有界宿主资源）。
- **TS 依据**：`agent/modules/transport/agentLoopSidecarClient.ts:659-684` 的串行 async generator 依次应用事件；真实入口文件与行号见本单 RESULT，不把 TS ACK/resume 游标与 Rust 并发排队时的游标时刻混为一谈。
- **Rust 行为**：`host/mod.rs` 声明事件应用队列/保留 NDJSON 字节预算，满时报 Limit；独立 execution_timeout（默认 300s）以及 terminal_drain_timeout 控制本地等待，合作式 callback 可与读取并发，结束时丢弃未完成 callback。
- **范围**：停滞/极慢 consumer、长于本地 watchdog 的合法 turn、终态后未完成 callback；这些本地边界可能先于 TS 返回错误。不是 operationDeadline 的等价实现，也不是可忽略所有 timeout 的豁免。ACK/cancel 独立额度及不丢 event 的约束仍适用。
- **机器判据**：现有 host budget/取消测试保障实现策略；同一 TS/Rust 慢消费者与超时配置矩阵仍须分别比较终态、事件和副作用，**本单未声称全边界相等**。
- **批准记录**：无；#191 资源策略任务不作为人批差异证明；**已知未对齐（未批准）**。

### A6.4-CLOCK-TIMECLIP-001 · 非法注入时钟未抛 Invalid time value

- **类别**：实现缺口（Date TimeClip 错误传播）。
- **TS 依据**：`router/RouterRuntime.ts:631,936,972` 调用 deps.now().toISOString()；本单 `diff-backfill/oracle.json.dates`：NaN、±Infinity、8.64e15+1 抛 RangeError: Invalid time value。范围内的扩展年份合法，本单已对齐，不算本条差异。
- **Rust 行为**：`runtime/context/prompt.rs::iso_timestamp(f64) -> String` 将浮点 saturating cast 到 i64 后格式化；NaN 变 epoch，Infinity 饱和，超 TimeClip 范围仍输出字符串，router 可能继续写记录。
- **范围**：仅注入的非法/超范围时钟；正常墙钟、范围内小数毫秒截断和合法扩展年份均不豁免。该函数不可失败的 API 需与 router 错误传播一起对齐，本单只复用共享 formatter 修正合法日期格式。
- **机器判据**：`context::prompt::tests::extended_year_timestamps_match_real_ts` 对范围内真实 TS 日期逐值比较；非法时钟未来应在 router 三个采样点分别断言 TS 同形错误及记录副作用，**当前不声称非法输入已相等**。
- **批准记录**：无；**已知未对齐（未批准）**。

### A6.4-COUNT-CACHE-DEPTH-001 · token count 缓存键深度 1800

- **类别**：实现限制（固定预算近似 V8 sortValue 栈）。
- **TS 依据**：`context/budget/TokenAccountingRuntime.ts:392-405,465` 的缓存键先 sortValue 再 JSON.stringify；既有 `tests/token_accounting.rs::cache_key_depth_matches_measured_v8_sort_boundary` 所引 Node 探针测得投影 transform 深度 1892 成功、1897 失败（schema 深度少 2）。
- **Rust 行为**：`runtime/model/json_walk.rs::CACHE_KEY_MAX_DEPTH=1800` 超限报 Maximum call stack size exceeded，`token_accounting.rs::cache_key` 使用该独立预算，不是通用 js_value 的 2000。
- **范围**：启用 provider count 时缓存键投影中实际保留的深值；禁用 provider count 不应访问仅用于缓存的深字段。1801 到 V8 首拒之间可提前拒绝，不声称此区间已获豁免。
- **机器判据**：`tests/token_accounting.rs::cache_key_depth_matches_measured_v8_sort_boundary` 与 `disabled_provider_ignores_deep_cache_only_fields`；真实栈阈值须以对应投影入口重测，不用裸 JSON.stringify 代替。
- **批准记录**：无；代码的 accepted undecidable band 是旧 AI 策略措辞，不是人批；**已知未对齐（未批准）**。

### A6.2-TRANSCRIPT-COLLATION-001 · 同 sequence 的孤代理时间戳拒绝

- **类别**：实现限制（ICU 排序权重）。
- **TS 依据**：`session/transcript/TranscriptReader.ts:87-90` 对相同 sequence 用 createdAt.localeCompare 升序，Node 保留孤代理权重；本单 collation 探针与 snapshot 原件证明该运行时差别。
- **Rust 行为**：`storage/transcript.rs::load` 如重复 sequence 组的任一 createdAt 含孤代理，返回 `Transcript timestamp collation requires the Node host for unpaired surrogates.`，不返回 TS 的已排序 entries。
- **范围**：仅重复 sequence 的排序组；没有同 sequence 时不要求 collator，孤代理原样保存。此检查是整个重复组的预检，可能在具体排序实现尚未比较该元素之前拒绝，比 snapshot 的按实际 comparator 调用边界更宽。
- **机器判据**：写两个合法同 sequence 的记录，其中 createdAt 分别为 U+D800/U+FFFD，再分别调用真实 readTranscript 与 Rust read_transcript；应记录 TS 顺序和 Rust 错误。**本单未新增该完整文件级金样，保留未对齐状态**。
- **批准记录**：无；**已知未对齐（未批准）**。

### A6.6-SNAPSHOT-CLOCK-001 · pre-epoch 系统时钟捕获失败

- **类别**：实现限制（系统时钟格式化）。
- **TS 依据**：`storage/workspaceSnapshot.ts:251,277` 使用 new Date().toISOString()，Date 范围内的 1970 年前时间可正常表示。
- **Rust 行为**：snapshot 的 created_at 调用 `storage/transcript.rs::timestamp`，SystemTime::duration_since(UNIX_EPOCH) 为负即失败；manifest 捕获转失败标记，标记自己的 createdAt 再失败时为空字符串。
- **范围**：机器墙钟被设在 Unix epoch 前；另一个 router 浮点时钟 formatter 本单已对齐合法日期，不影响此独立函数。极端未来扩展年份仍用四位最小宽度，未保证 TS 的带符号六位年份格式。
- **机器判据**：须在隔离可控时钟环境对同一 snapshot 捕获比较 manifest/failed marker 字节；**本单未更改系统时间，也未以当前正常墙钟测试冒充该边界通过**。
- **批准记录**：无；**已知未对齐（未批准）**。

### A6.6-FILEHISTORY-IO-001 · 未测 errno 文案回退

- **类别**：实现限制（Node 错误文案表外覆盖）。
- **TS 依据**：`session/filesystem/FileHistoryStore.ts` rewind 及捕获返回 fs Error.message；Node 按实际 syscall/路径/errno 格式化。
- **Rust 行为**：`storage/file_history.rs::node_fs_error` / node_read_error 对已测 errno 用 node_io，表外去掉 Rust `(os error N)` 后缀后保留 Rust reason，仍不保证 Node 的 code/syscall/path 文本。
- **范围**：file history 实际遇到已测表以外的 OS 错误；已测路径和普通内容字节不在差异内。与 snapshot 的未经删后缀回退是两套入口，不共享文案承诺。
- **机器判据**：现有 file_history 真实 TS 夹具只保障测量集；表外必须按 syscall 注入同 errno、比较公开 result.error，**当前无完整表外 OS 对拍**。
- **批准记录**：无；**已知未对齐（未批准）**。

### A7-LIFECYCLE-NFKC-001 · work path segment 的不完整 NFKC

- **类别**：未实现（Unicode normalization）。
- **TS 依据**：`agent/turn/TurnEnvironment.ts:24-27` 对 session/turn 字符串做完整 normalize("NFKC") 后替换、trim、截 96 单元；如 U+2460 `①` 规范化为 ASCII `1`。
- **Rust 行为**：`host/modules/lifecycle.rs::path_segment` 只折全角 ASCII 与 U+3000；其他字符走非 ASCII run 的 `-` 替换，① 这样的单字符最终成 unknown。
- **范围**：work 目录中的 sessionId/turnId 路径段含未实现兼容折叠；全角 ASCII 等已覆盖。结果仍符合 ASCII 安全字符集，但可能对应不同目录名；不因此声称完整 Unicode 等价。
- **机器判据**：真实 buildTurnEnvironment(含①) 与 Rust build_turn_environment 比较工作路径段；本单经 Git blob/import closure 守卫实际运行 TS，①/Ａ/Ⅳ 分别给出 `1/A/IV` 路径段（原始 `nfkc-probe.log` 见 RESULT）。Rust 未实现折叠的 ①/Ⅳ 按现有替换算法为 unknown；**未新增 Rust 此入口的完整相等金样**，未来接入完整 normalizer 时补。
- **批准记录**：无；依赖预算限制不是差异人批；**已知未对齐（未批准）**。

### A6.5-BASH-OS-001 · shell 会话和 stdio 类型

- **类别**：实现限制（POSIX 进程创建原语）。
- **TS 依据**：`tool/builtin/bash/commandRunner.ts:40-46` detached + pipe stdio；Node POSIX detached 使用 setsid，stdio pipe 在 Node/libuv 上为 socket pair，既有 `bash-tool/README.md` 保留该测量边界。
- **Rust 行为**：`runtime/tools/builtin/bash/runner.rs` 使用 process_group(0)，仍在父 session，stdio 为 pipe(2) FIFO。信号重置的 pre_exec 例外没有执行 setsid。
- **范围**：命令探查 session/tty/fd 类型时可观察；网关有 controlling tty 时打开 /dev/tty，TS detached 通常 ENXIO，Rust 可能成功并产生交互。通常 stdout/stderr 字节与整组信号清理不据此豁免。
- **机器判据**：受控 PTY 下运行同一报告器，记录 getsid/getpgrp、/dev/tty open 和 fstat fd 类型；**现有 bash 字节回放不覆盖这组 tty 判据，本单未宣称已验**。
- **批准记录**：无；批准 child signal 重置不等于批准会话差异；**已知未对齐（未批准）**。

### A6.5-BASH-OUTPUT-001 · 输出超 V8 字符串上限不退出网关

- **类别**：有意改进（避免未捕获异常结束服务）。
- **TS 依据**：`tool/builtin/bash/commandRunner.ts` stdout/stderr data listener 的字符串追加超 V8 MAX_STRING_LENGTH 会抛未捕获 RangeError；`cli/pilotdeck.ts:267` 的 uncaughtException 处理关停网关。真实 TS 原件 `bash-tool/README.md`「输出上限」所引 oom-rule.out/oom-nohandler.out。
- **Rust 行为**：`runner.rs::StreamText` 丢弃放不下的整块及其 progress callback，后续放得下的块继续追加，命令和网关继续运行；默认单流上限 0x1fffffe8 UTF-16 单元。
- **范围**：单 stdout/stderr 累计长度越过 V8 上限的 chunk；测试缩小上限只是负控装置，不把 10 单元当产品限制。不改变普通大小输出，也不宣称流分块本身跨运行稳定。
- **机器判据**：`tests/bash_tool.rs::stream_cap_drops_only_chunks_that_do_not_fit`、`stream_cap_defaults_to_v8_max_string_length`；网关存活差异仍需隔离真实 TS 子进程原件，不能只以缩小 cap 的 Rust 单测作两侧实效证明。
- **批准记录**：无；**已知未对齐（未批准）**。

### A6.5-BASH-WNOWAIT-001 · 不支持 WNOWAIT 时提前清理进程组

- **类别**：实现限制（平台后备路径）。
- **TS 依据**：`tool/builtin/bash/commandRunner.ts` 安排 termination 后 3 秒 SIGKILL，即使结果已经 settled；bash-tool 的 deferred-kill 原件覆盖此时序。
- **Rust 行为**：正常 WNOWAIT 平台保留未回收 leader，按 3 秒再杀组；waitid(WNOWAIT) 不可用时退回收尾立即 SIGKILL。runtime 在延时任务期间整体停机时 GroupGuard 也立即杀组。
- **范围**：仅 WNOWAIT 不支持或 runtime 提前整体退出的路径；普通 macOS/Linux 正常 deferred-kill 是对齐实现，不登记为差异。停机时 leader 僵尸回收需进程退出/后续 Tokio 回收，不承诺即时消失。
- **机器判据**：注入 waitid 不支持并用继续写入/延迟退出的后代报告实际 SIGKILL 时刻；**该平台 fallback 尚无两侧完整断言**，不能用正常 deferred-kill 测试充当通过。
- **批准记录**：无；**已知未对齐（未批准）**。

### A6.5-MCP-HTTPS-001 · MCP HTTP transport 缺少 TLS

- **类别**：未实现（HTTPS connector 接线）。
- **TS 依据**：`mcp/client/McpClient.ts` 的 buildTransport 使用 SDK streamable HTTP / SSE（源码真实路径见本单 RESULT），Node fetch/EventSource 支持 HTTPS。
- **Rust 行为**：`runtime/mcp/http.rs` 的 parse_endpoint 只接纳 http，https 直接报 mcp_unsupported_transport；`runtime/mcp/client.rs` 模块文档已披露。
- **范围**：MCP streamable_http / sse 的 HTTPS 端点；与已存在的 model TLS connector 无自动共用保证。HTTP 普通消息和 SDK 协议版本不豁免。
- **机器判据**：同一受控 TLS MCP server 对真实 SDK 与 Rust 执行 initialize/listTools/callTool，比较公开状态/错误；**本单未跑该 TLS listener，不把 HTTP 测试计为 HTTPS 通过**。
- **批准记录**：无；**已知未对齐（未批准）**。

### A6.5-MCP-HTTP-LIFETIME-001 · 连接后台生命周期与代理 dispatcher

- **类别**：有意改进及未实现（连接所有权/宿主集成）。
- **TS 依据**：SDK/EventSource transport 在关闭或首连失败后可能保留 body/3 秒重连任务；`network/fetch.ts` 与 `cli/proxy.ts` 负责 Node dispatcher。既有 `runtime/mcp/http.rs` 文档披露相关 SDK/undici 运行时事实，不作为「永远无可观察影响」保证。
- **Rust 行为**：关闭时丢弃打开的事件流；legacy SSE 首连失败后停止而不后台每 3 秒重试；不读取 cli/proxy dispatcher。
- **范围**：关闭后资源存活、失败后的后续请求/重连次数，以及需要代理才能连通的部署；未宣称每条协议消息均不同。与正常断线后的已实现 reconnect 机制分开，不能把所有重连概括为缺失。
- **机器判据**：受控服务分别记录 close 后连接存活、首次拒绝后请求次数及代理访问日志，比较两侧；**本单仅保留现状披露，未完成这些网络时间线实测**。
- **批准记录**：无；原文「no observable protocol effect」不是人批，也不排除网络副作用；**已知未对齐（未批准）**。

### A4-MIXED-BATCH-ORDER-001 · 已就绪转发与本地请求的响应交错

- **类别**：实现缺口（代理/本地应答调度时序）。
- **TS 依据**：真实固定基线金样 `tests/fixtures/metadata/order.json` 两例：immediate-forward 先 forwarded 后 local；delayed-forward 先 local 后 forwarded。原始帧字节未归一排序。
- **Rust 行为**：`pairing/tests.rs::mixed_batch_wire_goldens_document_immediate_gap_and_delayed_parity` 显式断言 immediate case 本地帧先出，与 TS 不等；delayed case 相等。
- **范围**：同批 mixed local/forward 请求、上游 forwarded Promise 在本地调度时已就绪的交错；不是所有 mixed batch，也不影响每条应答 id/result 对应关系。此差异尚不构成透明代理分数豁免。
- **机器判据**：上述 wire-golden 测试同时断言 immediate != TS 和 delayed == TS；后续修复须将 immediate 改成真实字节相等断言，不能按 id 排序掩盖时序。
- **批准记录**：无；**已知未对齐（未批准）**。

### A6.2-TRANSCRIPT-DOMAIN-001 · 尚未支持的域事件与恢复投影明确拒绝

- **类别**：未实现（transcript 域校验/投影覆盖）。
- **TS 依据**：`gateway/storage/AsyncTranscriptStorageAdapter.ts:120-414` 与 `session/events/SessionRuntime.ts:46-152` 接受 TS SessionDomainValidation 支持的完整域事件，真实 `transcript-adapter` 金样保留这些事件和存储调用。
- **Rust 行为**：`storage/transcript_adapter.rs` 通过 domain_supported / projection_supported 对当前切片之外的域事件/恢复投影返回 HostRequired 或显式错误，在 store.append 前拒绝，不猜测接受；recorder 私有验证的 model/tool 路径可写。
- **范围**：仅当前支持集合外的 turn/step/inbox 等域事件和恢复投影；模块当前未接线到 gateway 默认路由，不能表述为所有线上用户写入均失败，也不能将存储切片的正常测试外推为完整 session 支持。整项 fork-copy/事务等未来 API 建设不由本条豁免。
- **机器判据**：`storage::transcript_adapter::tests::unported_domain_entries_fail_closed_before_the_store` 与 `real_ts_scenarios_match_outcomes_calls_and_bytes` 的 Declared gap 分支，要求首次不等明确拒绝而非写入错误数据；未来实现时须改成真实 TS 的完整结果/存储调用相等断言。
- **批准记录**：无；既有测试的 Declared gap 注释不是批准；**已知未对齐（未批准）**。

## A7.3-INTERACTION-BOUNDARY-001 · pending 输入类型域与线程映射（边界登记，非豁免）

- **类别**：producer 输入适配尚未实现；不是已批准的行为差异。
- **TS 依据**（cc7c4e6）：`src/interaction/InteractionReplay.ts:9-30` 接纳 unknown，Date 转 ISO、BigInt 转字符串，函数等值按 replay 规则处理；`GatewayPermissionBus.ts:118-150`、`GatewayElicitationBus.ts:127-137` 的同步回调可以重入 live Map。
- **Rust 范围**：本 PR 的 pending 总线仅接纳 `JsValue`，可表示 undefined、非有限数字、UTF-16 字符串、对象和数组，不能直接表达 Date、BigInt、函数或循环宿主对象。producer 接入前须按 TS 规则转换，不可把不支持值静默丢弃。该适配前置于 A7.3 host/turn 接线验收，本 PR 不宣称已完成。
- **线程映射**：TS 没有跨线程重叠的 bus 调用。Rust 在共享 mutex 内记录 bulk 结算线程，其他线程不得 consume 或重复 bulk 结算已领取条目；同线程同步回调重入保留 TS 已拒条目可再次取出的结果。这是同步语义的实现约束，不是放宽行为验收。
- **机器判据**：`runtime::interaction::tests::interaction_real_ts_stepwise_oracle`（46 场景、268 步，含两种 kind 的同步嵌套 reject 与 consume）；`interaction_reject_cross_thread_response_and_bulk_have_one_winner`（channel 固定交错，含 delivered observer）；`tests/fixtures/interaction-bus/README.md` 为复现与类型边界。
- **批准状态**：无行为豁免授权；类型域外的 producer 适配待后续接线补齐。本记录不允许忽略 Date/BigInt 等适用输入，也不允许把跨线程双结算登记为接受。

## 已知未对齐（未接受，待人批）· A7.2 turn replay

以下一条为 A7.2-pr1（ReplayStore / EventCoordinator）行为差异的透明披露，**未批准，非已接受差异**；不新增豁免、不改判分语义，待人批。补登依据：codex r4 P1 #2 与 gate4 sibling P1——该差异此前只在代码注释、fixture README 与 `_tmp/` 的 RESULT.md 中自我披露，账本无条目，仓内读者会得出「该区域无已知差异」的假象；登记流程沿用 PR #476 对 gate3 sibling P1 的同类补登（只追加待批条目，check_frozen 通过，人批前不算已接受）。

### A7.2-ZERO-RETENTION-TIMER-001 · 零保留终态的生产回收时机（不可避免）

- **类别**：不可避免（JS 宏任务边界依赖宿主事件循环，Rust 库级无「调用栈已退回事件循环」的钩子）。
- **基线依据**（cc7c4e6 `src/` + Node 22.22.0 实测）：`gateway/client/GatewayTurnReplayStore.ts:81-90` 的 `setTimeout(0)` 只在调用栈退回事件循环后执行；经基线 tsx 加载未改源的真实 store 实测：`retainTerminal(0) → 同步忙转 20.06 ms 不让出事件循环 → snapshot` 仍 `{terminal:true, events:[{type:"kept"}]}`，`timerDelays:[0]`（探针 `_tmp/20261022-a72-pr1-replay/probe-zero-retention.mts`）。
- **Rust 行为**：生产 `ThreadTimer` 对 0 延迟取 Node 自身 1 ms 下限（`pilotdeck_protocol::timer_delay_ms`），到期即在 timer 线程删除；不承诺「调用方同步段内不删」。宿主需要精确 JS 边界时经 `TurnReplayOptions::with_timer` 注入自己的 `ReplayTimer`（如把 0 延迟推迟到宿主自己的调度回合）。
- **范围**：仅生产 `ThreadTimer` 且 `terminalRetentionMs=0`；默认 30000 与注入 timer（`ManualTimer` 等）路径不受影响。
- **机器判据**：`src/pilotdeck-gateway/src/runtime/turn/replay.rs` 的 `zero_retention_terminal_stays_visible_until_the_timer_runs`（注入 timer 钉精确边界）与 `production_zero_retention_after_a_busy_gap_is_already_reclaimed`（生产路径：忙转 20 ms、无 store 调用，轮询内部状态确认已回收，断言消息含本编号）。
- **批准记录**：无。`未批准，非已接受差异`；人批另行。
- **更正（2026-10-22 修复轮 5，本节自此含两条）**：本条范围与类别均有误——差异不限于 `terminalRetentionMs=0`，生产 `ThreadTimer` 下**所有**保留时长（含默认 30000）的到期回收都不等调用方当前同步段与微任务结束；「不可避免」归类依据不足（`with_timer` 已有宿主调度注入口）。见下方 A7.2-REPLAY-RETENTION-TIMER-001；原文字保留不改。

### A7.2-REPLAY-RETENTION-TIMER-001 · 生产 ThreadTimer 终态回收不等调用方同步段（实现选择，待人定）

- **类别**：实现选择（待人定：接受为已知差异，或在 A7.2-pr5 接宿主串行计时器后对齐）。codex r5 P1 #1/#2 与 Grok r5 指出轮 2-4 的「不可避免」归类依据不足：「库无法探知调用栈边界」只证明当前默认调度方式不能保证一致，不能证明产品无法实现；`TurnReplayOptions::with_timer` 已提供宿主调度注入口，宿主串行计时器（到期只排队、由宿主在当前这一步处理完之后执行，类似 JS 的下一个宏任务）待 A7.2-pr5（生产 ConnectionGateway 的宿主调度循环）接线。决策卡：`_tmp/20261022-loop/card-replay-timer.md`（选项 A 接受 / 选项 B 分两步对齐）。
- **基线依据**（cc7c4e6 `src/` + Node 22.22.0，**真实 timer，不替换 `setTimeout`**）：`gateway/client/GatewayTurnReplayStore.ts:81-90` 的 `setTimeout(terminalRetentionMs)` 回调只在后续 timer 回合执行。实测（`_tmp/20261022-a72-pr1-replay/probe-retention-real-timer.mts`，两次运行一致）：retention 0（同步忙转 20 ms）、5（忙转 25 ms）、30000（忙转 30020 ms）三种保留时长的到期时间均落在调用方同步段内，同步段 snapshot 与随后 Promise 微任务内 snapshot 均仍见 `terminal:true`，下一个 timer 回合（`await new Promise(r => setTimeout(r, 0))` 之后）才回收。
- **Rust 行为**：生产 `ThreadTimer` 对**所有**保留时长：到期回调在 timer 线程上按 `Instant` 到期即执行，不等调用方当前同步段或微任务结束（0 延迟仍取 Node 自身 1 ms 下限 `pilotdeck_protocol::timer_delay_ms`）。修复轮 5 未改该回收行为，仅更正登记范围与类别、并把测试宣称收窄为可证部分。
- **范围**：生产 `ThreadTimer` 的所有 `terminalRetentionMs` 值（含默认 30000 与 0）；注入 timer（`ManualTimer` 等）路径不受影响。可观测场景：调用方在同一逻辑步骤内先 `retain_terminal` 再读快照、且两者之间跨过到期时刻——TS 仍可见，Rust 可能已回收（如客户端重连重放时偶发缺失）。另注：TS 侧默认 retention 为 30000（`GatewayTurnReplayStore.ts:44`），唯一生产构造点 `cli/createLocalGateway.ts:929` 不传参，0 仅显式配置可达；差异对 0 与 30000 同构（见基线依据实测）。
- **机器判据**：`src/pilotdeck-gateway/src/runtime/turn/replay.rs` 的 `terminal_stays_visible_until_the_injected_timer_runs`（注入受控计时器 + 回调执行标记，覆盖 retention 0/5/30000，钉「timer 回合执行前可见」一侧）与 `production_retention_reclaims_eventually_after_a_busy_gap`（生产路径 retention 0/5：忙转跨过到期后**只断言最终回收**、无任何 store 调用，断言消息含本编号；不对「忙转期间已回收」做任何宣称）。
- **批准记录**：无。`未批准，非已接受差异`；待人定（接受，或在 A7.2-pr5 对齐并标「差异消除」），人批另行。

人批（2026-10-22，用户本人在协调者会话，采纳决策卡建议 B）：A7.2-REPLAY-RETENTION-TIMER-001 本 PR 以**已知未对齐（人批）**状态合入，**前置于 A7.2-pr5**：pr5 接入生产 ConnectionGateway 时实现宿主串行计时器（到期只排队，宿主处理完当前步骤后再回收），用真实 TS 边界金样对拍，届时在本节追加「已实现，差异消除」。决策卡 `state/decisions-20261022.md`「A7.2-pr1：重放缓存终态的回收时机」。

- **当前状态（以本行为准）**：已知未对齐（人批 2026-10-22，决策卡 state/decisions-20261022.md「A7.2-pr1：重放缓存终态的回收时机」），前置于 A7.2-pr5；非已接受差异。本行按 Grok r6 P1（`_tmp/20261022-a72-pr1-replay/rv-grok-r6.md` #1）追加，统一本条目内「未批准/待人定」与「人批」的表述冲突；上文原有文字（含「未批准」字样）保留不改，状态以本行为准。

### A7.2-COORDINATOR-DELIVERY-001 · 多线程宿主下的事件交付边缘与 TS 单线程同步交付不一致（实现选择，未批准）

补登（2026-10-22 修复轮 6，本节自此含三条）：gate6 sibling P1（`_tmp/20261022-a72-pr1-replay-gate6/sibling.md` #1）指出 coordinator 模块文档自我披露的其余 port-only 交付差异同源漏登——与轮 4 补登 A7.2-ZERO-RETENTION-TIMER-001、轮 5 补登 A7.2-REPLAY-RETENTION-TIMER-001 是同一反模式（差异只在代码注释与 fixture README 自我披露、账本无条目，仓内读者会得出「该区域无已知差异」的假象）。本条合并登记 a-d 四点；登记流程沿用 PR #476（只追加待批条目，check_frozen 通过，人批前不算已接受）。逐条评估结论：四点均需宿主调度（A7.2-pr5 的宿主串行接线）才能对齐，本 PR 内无可低成本对齐项——(a) 让 `retain_terminal`/`start` 等待在途 drainer 会在 sink 内重入同一会话时死锁；(d) 让并发 emitter 阻塞到 sink 完成等于推翻 codex-r3 P1 #1 已钉的「入队即返回」回归测试，均属需重新谈判既有评审结论的改动，未自行实施。

- **类别**：实现选择（未批准；对齐需宿主调度）。四点同源于宿主并发模型：TS 的 `emit` 在单线程事件循环上同步调用 sink 后才返回，`start`/`retainTerminal`/`dispose` 与 `emit` 在同一事件循环上串行，不存在「已接受未交付」的中间态，也不存在并发 emitter；Rust `TurnEventCoordinator` 面向多线程宿主，交付走每会话 FIFO 队列、由成为 drainer 的 emitter 异步完成。
- **基线依据**（cc7c4e6 `src/`）：`gateway/client/GatewayTurnEventCoordinator.ts:38-45`——`emit` 查 sink、`withRunId`+`record` 之后**同步调用 `sink(eventWithRunId)` 再 `return true`**；`:29-32`（`start` 先 `replayStore.start` 再替换 sink）、`:47-50`（`retainTerminal` 先 `sinks.delete`）、`:56-59`（`dispose` 清空 sinks）都在同一事件循环上与 `emit` 串行执行，故 TS 中最后一次 sink 调用总在 sink 的移除/替换之前，且任何时刻都没有事件处于「已接受未交付」状态。
- **Rust 行为**（`src/pilotdeck-gateway/src/runtime/turn/coordinator.rs:21-37` 模块文档披露，本轮补登记并在该处加账本指针）：(a) `retain_terminal`/替换性 `start` 返回后，在途 drainer 仍会把此前已接受的事件交付给旧 sink（其 emitter 已拿到 `true`）；(b) sink panic 或 `dispose` 静默丢弃已接受未交付的事件，emitter 无任何信号（`dispose` 例外：在途 drainer 已弹出的那一条仍会交付，sink 句柄随队列条目出行）；(c) 重入 `emit` 会先交付并发 emitter 先记录的事件，它们的 sink panic 归因到重入调用点而非原 emitter 的调用点（port-only attribution）；(d) 交付队列无界，慢 sink 不向 emitter 施加背压（并发 emitter 入队即返回）。
- **范围**：仅多线程宿主并发调用 `TurnEventCoordinator` 时的交付边缘（`start`/`emit`/`retain_terminal`/`dispose` 的跨线程交错）。单线程顺序使用与 TS 逐字节一致（`tests/fixtures/turn-replay/oracle.json` 全部 `coordinatorCases`，含重入 emit 的 TS 返回边界与失败归因）；`record`/`snapshot`/重放缓存终态回收时机不在本条（后者见 A7.2-REPLAY-RETENTION-TIMER-001）。可观测场景：宿主在 sink 执行期间从另一线程 `retain_terminal`/`start`/`dispose`（如会话关闭/重启与事件发布并发）时，交付时序、丢弃与 panic 归因与 TS 不同。
- **机器判据**：(a) `coordinator::tests::accepted_events_reach_the_old_sink_after_retain_terminal_returns` 与 `accepted_events_reach_the_old_sink_after_a_replacing_start_returns`；(b) `dispose_drops_events_accepted_but_not_yet_delivered` 与 `sink_panic_drops_events_queued_behind_it`（四条均为本轮新增、断言消息含本编号）；(c) `nested_delivery_panic_keeps_the_queue_for_the_outer_drainer`（codex-r3 P2 #4 回归）；(d) `reentrant_emit_waits_for_concurrently_queued_events` 与 `stale_reentrant_marker_goes_through_the_restarted_queue`（断言并发 emitter 在 drainer 停留 sink 期间入队即返回 `true`）。四条新增正控在 `tests/fixtures/turn-replay/negative.py` 各配一条专属变异（`retain-terminal-invalidates-drain`、`replacing-start-invalidates-drain`、`dispose-keeps-delivery-queue`、`panic-guard-keeps-queued-events`），实测转红记录见 `_tmp/20261022-a72-pr1-replay/RESULT.md`「修复轮 6」。
- **批准记录**：无。`未批准，非已接受差异`；前置于 A7.2-pr5（生产 ConnectionGateway 宿主调度接线：宿主串行驱动 coordinator 后 (a)-(d) 的并发交错场景消失、届时可声明差异消除，或人批接受现状）。A7.2-pr3（发布管线）只是 coordinator 的调用方，不解决本条。

### A7.2-COORDINATOR-SINK-OWNERSHIP-001 · sink 所有权模型：弱引用注册 + 调用方持强句柄（实现选择，未批准）

补登（2026-10-22 修复轮 7，本节自此含四条）：gate7 P1 #1（`_tmp/20261022-a72-pr1-replay-gate7/`）指出 sink 强捕获 `Arc<coordinator>`（重入式 sink 的自然写法）会形成 `Arc` 引用循环，coordinator 及其 replay store 的 `ThreadTimer` 工作线程在最后一个宿主句柄 drop 后永久泄漏。本轮按 gate 优先项**从设计上消除**了循环（coordinator 只持 `Weak` sink，`start` 返回强所有者 `TurnSinkRegistration`，泄漏测试钉住「drop 后 sink 释放、coordinator 回收、timer 线程退出」）；消除不了的残余是**所有权语义本身与 TS 不同**，按账本纪律登记如下。

- **类别**：实现选择（未批准；由 Rust `Arc` 无循环 GC 决定，非宿主调度可对齐）。TS 的 sink 存在普通 `Map` 里，宿主丢失回调引用不影响注册，GC 连循环一起回收；Rust 若持强引用则泄漏，故注册生命周期改为显式：宿主持有 `TurnSinkRegistration` 的最后一个克隆期间 sink 有效，全部 drop 即注销。
- **基线依据**（cc7c4e6 `src/`）：`gateway/client/GatewayTurnEventCoordinator.ts:23`（`sinks = new Map` 持 sink 强引用）、`:29-32`（`start` 先 `replayStore.start` 再 `sinks.set`，返回值无人持有；TS 单线程 + GC，无泄漏）。
- **Rust 行为**（`src/pilotdeck-gateway/src/runtime/turn/coordinator.rs:36-49` 模块文档披露，本轮登记并在该处加账本指针）：`CoordinatorState.sinks` 持 `Weak<dyn Fn>`；`start` 返回 `TurnSinkRegistration`（`Clone`，持强 `Arc`）。最后一个注册 drop 后：后续 `emit` 走 `retain_terminal` 同款拒绝路径（`false`，并顺手剪除死弱引用）——**与 TS 的差异**：TS 中宿主丢掉回调引用后 sink 仍在 Map 里、emit 照常交付，Rust 中直接拒收。已接受未交付的事件不受影响（强 sink 句柄随队列条目出行）。
- **范围**：仅 `TurnEventCoordinator` 的 sink 注册生命周期（`start`/`emit` 的注册检查）。单线程宿主持注册至会话结束（与 TS `retainTerminal`/`dispose`/替换 `start` 同跨度）时行为与 TS 逐字节一致（`tests/fixtures/turn-replay/oracle.json` 全部 `coordinatorCases`，本轮 harness 显式持注册、金样不变）；交付时序差异见 A7.2-COORDINATOR-DELIVERY-001，终态回收时机见 A7.2-REPLAY-RETENTION-TIMER-001。可观测场景：宿主提前 drop 注册（如注册被短暂绑定在块作用域内）后继续 emit。
- **机器判据**：`coordinator::tests::strongly_capturing_sink_release_drops_the_coordinator_and_stops_the_timer`（强捕获写法下 drop 注册→sink 释放（drop 标记）、coordinator 回收（`Weak` 升级失败）、生产 `ThreadTimer` 工作线程退出（worker 返回前置位的 `exited` 标志，5 s 轮询期限）；断言消息含本编号）与 `dropping_the_registration_unregisters_the_sink_but_keeps_the_coordinator`（drop 注册→emit `false`→重新 `start`→emit `true`）。`tests/fixtures/turn-replay/negative.py` 配三条专属变异（`start-retains-strong-sink`：往交付队列强持 sink 重建循环、`timer-exit-flag-never-set`、`registration-drop-accepted`），实测转红记录见 `_tmp/20261022-a72-pr1-replay/RESULT.md`「修复轮 7」。
- **批准记录**：无。`未批准，非已接受差异`；泄漏本身已消除（不登记），登记的是所有权语义偏离；人批另行。

**修复轮 8 追加更正（2026-10-23，codex-r8 P1 #1/#2，`_tmp/20261022-a72-pr1-replay/rv-codex-r8.md`；本块为承诺范围更正，上文原文字保留不改，范围以本块为准）**：

1. 「最后一个注册 drop 后 emit 拒收」在轮 7 的实现里**并未成立**：`emit` 以「sink 的强引用是否还在」代理注册存活，而队列条目与在途交付都持强 sink——另一线程停在 sink 内时 drop 最后一个注册，随后 `emit` 仍返回 `true` 并记录（单线程下 sink 自 drop 注册后重入 `emit` 同理）。轮 8 起注册存活标记与交付用强 sink 分离：`CoordinatorState.sinks` 改持 `Weak<SinkRegistration>`（注册记录），交付队列只持其内层 `Sink`；`emit` 的注册检查只看注册弱引用，已接受事件仍由队列条目的强 sink 交付完。上文「Rust 行为」一行按此更正。
2. 「泄漏已从设计上消除、不登记」的承诺范围收窄为**仅 coordinator 侧的环**（sink 强捕获 coordinator）。宿主侧结构（评审反例：`Arc<Host>` 同时保存 coordinator 与注册句柄、sink 强捕获 `Arc<Host>` 以调用宿主方法）形成 `Host → registration → sink → Host`，drop 全部外部句柄后 sink、coordinator 与 `ThreadTimer` 均不释放——Rust `Arc` 无环 GC，任何库 API 原理上无法从外部断开宿主内部环，与 TS 闭包可泄漏同类（差别仅在 TS GC 最终能收环）。本轮已删除代码注释与 FROZEN 登记注中「已从设计上消除」的过度承诺，改为文档化所有权约定（`start`/`TurnSinkRegistration`/模块文档）：sink 需回调宿主必须弱捕获（`Weak<Host>`）；会话结束由宿主显式 drop 最后注册或调用新增的显式注销 `unregister(&session_key)`（只摘 map 中注册、后续 `emit` 拒收，不动重放缓存/终态保留，注册句柄仍在宿主手里也生效），不依赖 drop 顺序。宿主侧残余环属本条目登记的已知残余（非「已消除」）。
3. 机器判据追加（均在 `coordinator.rs` tests、断言消息含本编号）：`registration_check_fails_while_a_delivery_is_in_flight`（停在 sink 内时 drop 最后注册→emit `false` 且不记录，已接受事件交付完）、`sink_dropping_its_own_registration_rejects_the_reentrant_emit`（sink 内自 drop 注册后重入 emit→`false` 且不记录）、`unregister_detaches_the_sink_without_touching_the_replay`（注册仍存活时 `unregister`→emit `false`、重放缓存/终态不动、随后 `start` 恢复交付）、`weakly_capturing_host_releases_the_sink_coordinator_and_timer_on_drop`（按约定弱捕获：drop 外部句柄→sink 释放、host/coordinator 回收、生产 timer 线程退出）、`strongly_capturing_host_leaks_on_drop_and_releases_through_explicit_unregister`（按反例强捕获：drop 全部外部句柄→泄漏被钉住〔host/coordinator/sink/timer 均存活〕，经显式注销+释放注册后同样全部释放）。`negative.py` 各配专属变异（`emit-accepts-while-in-flight`、`emit-accepts-while-queued`、`unregister-keeps-sink`、`weak-host-release-blocked-by-strong-sink`、`strong-host-release-blocked-by-strong-sink`），实测转红记录见 `_tmp/20261022-a72-pr1-replay/RESULT.md`「修复轮 8」。

## A7.3 interaction 边界条目更正（#516）

### A7.3-INTERACTION-BOUNDARY-001-R1 · pending 输入类型域与线程映射（六字段更正重述，边界登记，非豁免）

> 本条是 issue #516 的只追加更正：上方同名条目（`##` 级标题，字段名为「TS 依据 / Rust 范围 / 线程映射 / 批准状态」）不符本账本标准六字段，本条按六字段重述同一内容；原条目原文逐字保留，不改判分语义、不新增豁免。「线程映射」并入「Rust 行为」；机器判据按 issue #516 后的在树测试更新（金样 46→48 场景、268→281 步，新增跨线程注册竞速与注入分支判据）。

- **类别**：producer 输入适配尚未实现；不是已批准的行为差异。
- **TS 依据**（cc7c4e6）：`src/interaction/InteractionReplay.ts:9-30` 接纳 unknown，Date 转 ISO、BigInt 转字符串，函数等值按 replay 规则处理；`GatewayPermissionBus.ts:118-150`、`GatewayElicitationBus.ts:127-137` 的同步回调可以重入 live Map，`rejectSession` 的耗尽循环与 `bySession.delete` 为相邻语句（`GatewayElicitationBus.ts:127-138`）。
- **Rust 行为**：pending 总线仅接纳 `JsValue`，可表示 undefined、非有限数字、UTF-16 字符串、对象和数组，不能直接表达 Date、BigInt、函数或循环宿主对象；producer 接入前须按 TS 规则转换，不可把不支持值静默丢弃。线程映射：TS 没有跨线程重叠的 bus 调用；Rust 在共享 mutex 内记录 bulk 结算线程，其他线程不得 consume 或重复 bulk 结算已领取条目，同线程同步回调重入保留 TS 已拒条目可再次取出的结果；`reject_session` 的末次扫描与末尾删桶在同一临界区完成，与末次扫描竞速的跨线程 register 要么被同桶扫描结算、要么在删除后新建桶存活、替换桶按 TS 基线丢弃（真实 TS 探针见 issue #516 RESULT）。这是同步语义的实现约束，不是放宽行为验收。
- **范围**：该适配前置于 A7.3 host/turn 接线验收，不涵盖 producer 宿主对象的全部转换路径，也不代表 A7.3 完整轮次验收完成。
- **机器判据**：`runtime::interaction::tests::interaction_real_ts_stepwise_oracle`（48 场景、281 步，含两种 kind 的同步嵌套 reject 与 consume、`injectedOptions` 全量注入）；`interaction_reject_cross_thread_response_and_bulk_have_one_winner` 与 `interaction_register_racing_reject_window_matches_ts_map_semantics`（channel 固定交错，含 delivered observer 与两种注册竞速交错）；`interaction_injected_options_share_the_supplied_components`（注入组件外部句柄可观测）；`tests/fixtures/interaction-bus/README.md` 为复现与类型边界。
- **批准记录**：无行为豁免授权；类型域外的 producer 适配待后续接线补齐。本记录不允许忽略 Date/BigInt 等适用输入，也不允许把跨线程双结算登记为接受。

## 已知未对齐（未批准，待人批）· A7.5 替换协调器

### A7.5-REPLACEMENT-ACTIVE-GETTER-PANIC-001 · 超时回滚时 hasActiveTurn 端口失败：TS 拒绝，Rust 告警后重新约时

- **类别**：Rust 侧 panic 恢复（#518 要求端口 panic 后协调器可恢复）。
- **TS 依据**（cc7c4e6）：`src/gateway/client/GatewayTurnReplacementCoordinator.ts:274` 超时回调里 `hasActiveTurn` 的调用在 try 之外，getter 抛错时 rollbackExpired 以该错误拒绝。
- **Rust 行为**：`src/pilotdeck-gateway/src/runtime/replacement.rs` 捕获该端口的 panic，记告警并重新约定超时；getter 正常返回时行为与 TS 一致。
- **范围**：仅超时路径上 session 端口 `has_active_turn` panic；不影响其他端口失败、正常超时回滚与 active 推迟的对拍。Rust 端口签名返回 bool，没有与 JS throw 对应的 Err 通道，只有 panic 会走到这里。
- **机器判据**：`runtime::replacement` 单测中 #518 新增的 panic 场景（证据 `_tmp/20261023-issue-518-replacement/RESULT.md`）。
- **批准记录**：无；实现方按 #518 任务实现并给出候选登记文字；**已知未对齐（未批准）**，待人批。

## 已知未对齐（未批准，待人批）· D2 CLI chat search

### D2-CHATSEARCH-REGEX-BACKTRACK-001 · `--regex` 灾难性回溯：TS 长时间不返回，Rust 有界失败

- **类别**：安全方向偏离（基线无界执行 → 有界资源失败）；CLI 范围，记入本账本是沿用 #502 全仓自述差异集中登记的做法。
- **TS 依据**（cc7c4e6）：`src/session/search/NodeSessionSearchPort.ts:85`、`:87` 直接执行无超时的 V8 RegExp。真实 Node v22.22.0 对 `\b(a+)+$` 与 `(a|aa)+\bc$`，输入 `"a".repeat(100)+"!"`，各在 2,000 ms 内未返回，由探针超时终止（证据 `_tmp/20261023-d2-pr2-chat-search/backtrack-probe.cjs`、`backtrack-ts.log`）；只证明「超过 2 s 未返回」，不断言永久挂起。
- **Rust 行为**：`src/pilotdeck-cli/src/chat_search/matcher.rs` 保留 fancy-regex 默认回溯上限 1,000,000，超限时 exit 1、stdout 为空、stderr 精确为 `Chat search regex exceeded backtracking limit\n`。
- **范围**：仅 `pilotdeck chat search --regex` 执行时回溯超限；不放宽其他正则编译错误、匹配结果、snippet 或输出的逐字节对拍。
- **机器判据**：`src/pilotdeck-cli/tests/chat_search_exit.rs` 的 `chat_search_backtracking_limit_is_a_bounded_error`（exit、空 stdout、stderr 原文、耗时 < 10 s）；负控「改坏回溯超限文本」转红。
- **批准记录**：无；PR #513 评审（Grok r1 #4）建议保留上限并登记；**已知未对齐（未批准）**，待人批是否接受为向安全偏离。


### D2-CHATSEARCH-REGEX-GIANTQUANT-001 · 巨量词的 V8 饱和语义与 Rust 编译限制

- **类别**：CLI 正则编译边界，已知未对齐；非已接受差异，不产生对拍豁免。
- **TS 依据**（cc7c4e6）：`src/session/search/NodeSessionSearchPort.ts:85-91` 使用原生 `RegExp`，`a{2147483648,2147483647}` 在 Node v22.22.0 编译成功，当前真实 CLI 夹具返回 exit 0、JSON `matches: []`。V8 在量词顺序检查前把边界饱和到 2^31-1；可用 `src/pilotdeck-cli/tests/fixtures/chat-search/generate.mts <输出目录> giant-quantifier`（相同 Node loader 与 PDRS_BASELINE 设置）独立重采，沿用 HEAD、工作区和源码 SHA-256 守卫。两次真实输出及哈希见工作区 `_tmp/20261023-d2-pr2-chat-search/RESULT.md`「修复轮 5」。
- **Rust 行为**：`src/pilotdeck-cli/src/chat_search/matcher.rs` 顺序检查使用饱和边界，但向 fancy-regex 传入原始巨量词，编译被拒绝；同查询 exit 1、stdout 为空、stderr 为 `Invalid regular expression: /a{2147483648,2147483647}/i: Invalid regular expression\n`。
- **范围**：至少覆盖上述巨量词；未穷举 V8/fancy-regex 的数值与编译尺寸边界。不得推广为所有大数模式均不匹配，或因此放宽其他正则结果和错误文本。
- **机器复算**：生成器 `giant-quantifier` 模式保存真实 TS argv、exit、stdout、stderr；用相同 files/env/argv 执行 Rust 可复现分歧。该模式独立于通过的 39 原场景与 edges 矩阵，不能被计作对拍通过。
- **批准记录**：无；按 PR #513 GLM r3 P2 #1 由实现方在修复轮 5 补登，协调者复核后保留，**已知未对齐（未批准，待人批）**。后续须完成语义对齐或取得明确接受差异授权，D2 完成前不得据此宣称全量一致；本轮只补台账，不改变匹配行为或判分语义。

## 已知未对齐（未批准，待人批）· A7.2 submit 编排

### A7.2-SUBMIT-FAILURE-GUARD-001 · runPolicy 启用 FailureGuard 时 Rust 显式拒绝（运行时未移植）

- **类别**：未移植运行时依赖（显式能力拒绝，非静默跳过）。
- **TS 依据**（cc7c4e6）：`src/gateway/client/InProcessGateway.ts:1346-1383、1702-1706、1776-1778、1841-1843`，依赖 `src/agent/policy/FailureGuard.ts:14-91` 与 `classify.ts:3-32`：启用 failureGuard 时计数模型 / 工具错误，trip 后发 `run_policy_stopped` 并以 run_policy 原因 abort。
- **Rust 行为**：`src/pilotdeck-gateway/src/runtime/turn/submit.rs:786-789` 在 refresh 后发现 FailureGuard enabled 即发并持久化 `CAPABILITY_UNAVAILABLE`（`FailureGuard runtime is unavailable; requires A7.2-pr4b.`），拒绝本轮 native submit 并释放资源；enabled=false 时走正常 submit 路径。
- **范围**：仅启用该 run policy 的提交；不得据此宣称 guard 已对齐，也不放宽其他 submit 路径的对拍。推迟到 A7.2-pr4b（FailureGuard / classify 移植与 submit 的 observe / trip / active 生命周期接线），前置于 A7.2-pr6 生产宿主接线验收与 A7.2 关闭；补齐后移除此能力拒绝，须有真实 TS guard 金样与计数 / trip 负控。
- **机器判据**：`src/pilotdeck-gateway/src/runtime/turn/submit_tests.rs:805` `unavailable_capabilities_are_explicitly_refused` 的 `failure-guard-unavailable` 用例（:811）。
- **批准记录**：无；PR #533 Grok r1 F1 要求登记，实现方在修复轮 1 给出候选，协调者核对引用后登记；**已知未对齐（未批准，待人批）**。

## A6.7 hooks 执行器状态更新与新增条目（p2-hooks，10-23）

- **A6.7-HOOK-HTTPS-001 当前状态（追加，不改原条目）**：已实现。HTTP hook 复用 model 传输的 webpki TLS 连接器，https 初始 URL 与 http→https 重定向可执行；机器判据 `src/pilotdeck-gateway/tests/hooks_https.rs` 的 `http_hooks_over_tls_match_real_ts`（本地 TLS 服务回放真实 TS 金样）与 `hooks_https_fixtures_regenerate_identically_from_ts`；负控「拒绝 https」转红。原条目所述差异消除；残余的信任源差异见下方 A6.7-HOOK-TLS-TRUST-001。

### A6.7-HOOK-OUTPUT-001 · command hook stdout / stderr 按 V8 字符串上限封顶

- **类别**：有界 containment（与 A6.5-BASH-OUTPUT-001 同规则：整块丢弃并继续排空）。
- **TS 依据**（cc7c4e6）：`extension/hooks/execution/CommandHookExecutor.ts:5、28、33` 经 `commandRunner.ts:118、130-132`，输出超过 V8 字符串上限抛 `RangeError: Invalid string length`，CLI 未捕获而 `shutdownAndExit(1)`（`cli/pilotdeck.ts:267-270`）。
- **Rust 行为**：`src/pilotdeck-gateway/src/runtime/hooks/exec.rs` 单流按 `MAX_STRING_LENGTH`（UTF-16 码元）封顶（:469），正常结束时溢出为 `non_blocking_error`、stderr `Invalid string length`、stdout 空、无 exitCode；timeout / cancel 保留原结果，exit 2 保留 blocking。TS 是整个进程退出，Rust 只让该 hook 失败。
- **范围**：仅 command hook 输出超过 V8 字符串上限。
- **机器判据**：`src/pilotdeck-gateway/tests/hooks_runtime.rs:293` `command_output_over_string_limit_is_a_range_error`；负控「忽略输出上限」转红。
- **批准记录**：无；**已知未对齐（未批准，待人批）**。

### A6.7-HOOK-TLS-TRUST-001 · https hook 的信任源与代理不同于 Node

- **类别**：未实现（信任源 / 代理）。
- **TS 依据**：Node `fetch` 读取 `NODE_EXTRA_CA_CERTS` 扩展信任；TS CLI 启动时安装 undici proxy dispatcher（`cli/pilotdeck.ts:28`）。
- **Rust 行为**：HTTP hook 只信任 webpki（Mozilla bundle），不读 `NODE_EXTRA_CA_CERTS`，不走 TS 的代理 dispatcher；私有 CA 的 https hook 在 Rust 报 `fetch failed`。与 model 传输一致。
- **范围**：用私有 CA 或依赖代理的 https hook。扩展信任源会改变安全边界，需产品决定。
- **机器判据**：暂无（实现方在 `_tmp/20261023-p2-hooks/RESULT.md`「范围外」记录）。
- **批准记录**：无；**已知未对齐（未批准，待人批）**。

## 已知未对齐（未批准，待人批）· A7.4 user_dialog HTTP store

以下九条来自 PR #535 修复轮 1 实现方的候选（`_tmp/20261023-a74-pr2-http-store/RESULT.md` §8 C1–C12），协调者核对引用后登记；C2（spawn_blocking，输出字节不变）、C5（重复请求头取首个，与 Node 默认一致）、C6（类型层不可表达的入参，无运行时行为）不是可观察差异，未登记。TS 依据均指 `src/gateway/user-dialog/HttpGatewayUserDialogStore.ts`（cc7c4e6）与 Node v22.22.0 `http.createServer` 默认值。

### A7.4-HTTPSTORE-AUTH-BEFORE-BODY-001 · 鉴权与长度检查先于读请求体（向安全偏离）
- **类别**：处理顺序 / 资源边界。**TS 依据**：`:515-523` 先收完整 body 再进 handler，401 在 `:194-204`。**Rust 行为**：405 / 404 / 401 与 Content-Length 检查在读 body 之前，未认证的大 body 直接 401。**范围**：HTTP server 包装层；handler API 仍接收完整 body。**机器判据**：`src/pilotdeck-gateway/src/user_dialog/http_store_tests.rs` `request_body_limit_rejects_oversized_content_length_before_reading`。**批准记录**：无；**已知未对齐（未批准）**。

### A7.4-HTTPSTORE-BODY-LIMIT-001 · 请求体 2 MiB 上限，超限 413（向安全偏离）
- **类别**：新增资源上限。**TS 依据**：`:522-523` 无上限收集，仅受 V8 字符串上限约束。**Rust 行为**：`DEFAULT_MAX_BODY_BYTES = 2 MiB`，Content-Length 预检与 chunked `Limited` 均回 413。**范围**：所有 POST body；合法夹具 1,048,709 B 仍通过。**机器判据**：`src/pilotdeck-gateway/src/user_dialog/http_store_tests.rs` `request_body_limit_rejects_oversized_content_length_before_reading`、`request_body_limit_rejects_oversized_chunked_bodies`、`request_body_at_the_limit_is_accepted`；金样 `huge-valid-json` 为 200。**批准记录**：无；**已知未对齐（未批准）**。

### A7.4-HTTPSTORE-TIMEOUTS-001 · 请求头 60 s + 请求体 300 s 两段超时（Node 为单一 requestTimeout 覆盖整段）
- **类别**：超时语义。**TS 依据**：Node 默认 `headersTimeout = 60000`、`requestTimeout = 300000`（后者覆盖整段请求），`:236` 用默认值。**Rust 行为**：`header_read_timeout(60 s)` 加 body `timeout(300 s)`，极端情况下总时长约 360 s。**范围**：仅慢客户端。**机器判据**：`src/pilotdeck-gateway/src/user_dialog/http_store_tests.rs` `partial_headers_are_closed_after_headers_timeout`、`slow_request_bodies_are_closed_after_request_timeout`；360 s 总时长无测试。**批准记录**：无；**已知未对齐（未批准）**。

### A7.4-HTTPSTORE-KEEPALIVE-001 · 未实现 keepAliveTimeout 5 s
- **类别**：连接生命周期。**TS 依据**：Node 默认 `keepAliveTimeout = 5000`。**Rust 行为**：未单独实现，空闲 keep-alive 连接由 60 s 请求头超时兜底关闭。**范围**：空闲 keep-alive 连接；单请求不受影响。**机器判据**：暂无（实现方建议后续补）。**批准记录**：无；**已知未对齐（未批准）**。

### A7.4-HTTPSTORE-MAX-CONNECTIONS-001 · 连接数上限 64（向安全偏离）
- **类别**：新增资源上限。**TS 依据**：Node `maxConnections` 默认无限。**Rust 行为**：`DEFAULT_MAX_CONNECTIONS = 64` + Semaphore，满额时暂停 accept。**范围**：单进程 server。**机器判据**：`src/pilotdeck-gateway/src/user_dialog/http_store_tests.rs` `connection_semaphore_caps_in_flight_connections`。**批准记录**：无；**已知未对齐（未批准）**。

### A7.4-HTTPSTORE-431-408-SHAPE-001 · 431 / 408 响应可能多出 content-length: 0
- **类别**：错误响应字节。**TS 依据**：Node 原始 socket 实测为 `HTTP/1.1 431 Request Header Fields Too Large\r\nConnection: close\r\n\r\n` 与 `HTTP/1.1 408 Request Timeout\r\nConnection: close\r\n\r\n`（空 body）。**Rust 行为**：空 body + `Connection: close`，状态码与 Node 一致；hyper 可能补 `content-length: 0`，未逐字节比对。**范围**：仅超限 / 超时路径。**机器判据**：`src/pilotdeck-gateway/src/user_dialog/http_store_tests.rs` `oversized_headers_are_rejected_like_node`、`slow_request_bodies_are_closed_after_request_timeout`（断言状态、空 body、connection: close）。**批准记录**：无；**已知未对齐（未批准）**。

### A7.4-HTTPSTORE-KEY-ORDER-001 · handler 透传 store 结果时键序固定
- **类别**：JSON 字节。**TS 依据**：`:210` 直接 `JSON.stringify` store 返回对象，保留其插入顺序。**Rust 行为**：`StoredAnswer` / `StoredDialog` 固定为 `requestId,result,submittedAt` 与 `request,createdAt`。**范围**：自定义 store 用别的插入顺序时字节不同；File store 一致。**机器判据**：handlerCases 的 `claim-ok` 等金样；自定义顺序 store 无测试。**批准记录**：无；**已知未对齐（未批准）**。

### A7.4-HTTPSTORE-CLIENT-ERROR-CLASS-001 · 客户端网络 / 中止错误不保留 JS 错误类名
- **类别**：错误对象形状。**TS 依据**：`:154-167` 只把 `response.json()` 失败吞成 undefined，网络 / 中止异常仍是 TypeError / AbortError。**Rust 行为**：`HttpStoreTransportError` 只有 message 与 invalid_response 标志，统一为 `fetch failed` 或 invalid-response 文案。**范围**：client 错误面；成功路径不受影响。**机器判据**：clientRequests 的 message 金样（类名不可观察）。**批准记录**：无；**已知未对齐（未批准）**。

### A7.4-HTTPSTORE-CLIENT-BODY-UNITS-001 · 客户端响应体上限按 UTF-8 字节计（TS 按 UTF-16 码元）
- **类别**：字符串上限语义。**TS 依据**：`Response.text()` 受 V8 `MAX_STRING_LENGTH`（UTF-16 码元）约束。**Rust 行为**：复用 `fetch_redirect::read_body`，按 UTF-8 字节累计，多字节文本可能更早触发上限并映射为 invalid response。**范围**：仅接近 512 MB 的超大响应。**机器判据**：暂无（需构造多字节 512 MB 边界）。**批准记录**：无；**已知未对齐（未批准）**。

## A7.4 user_dialog HTTP store 条目更正（PR #535 修复轮 2；原条目保留在上方）

### A7.4-HTTPSTORE-MAX-CONNECTIONS-001-R1 · 更正

- **类别**：新增资源上限 / 过载响应。
- **TS 依据**：Node v22 `maxConnections` 默认无限，`net.Server` 不因连接数停止 accept。
- **Rust 行为**：保留 `DEFAULT_MAX_CONNECTIONS = 64` + Semaphore；满额时 accept 不阻塞，立即 503 + `Connection: close`，最多 drain 100 ms；释放后继续服务。旧条目“满额时暂停 accept”已不成立。
- **范围**：单进程 server；攻击者仍可占满 64 个 permit，合法请求得到 503（不是成功），故仍是“向安全/可用性偏离”的未对齐项。
- **机器判据**：`http_store_tests.rs:665 connection_cap_answers_overflow_with_503_and_recovers`、`:714 overflow_connections_do_not_block_the_accept_loop`；负控恢复阻塞 accept 后 1 s 超时红。
- **批准记录**：无；候选，待协调者改条目文字与判据，**未批准**。

### A7.4-HTTPSTORE-431-408-SHAPE-001-R1 · 更正

- **类别**：错误响应字节 / 头部计数。
- **TS 依据**：Node v22.22.0 头部超时实测 `HTTP/1.1 408 Request Timeout\r\nConnection: close\r\n\r\n`；`maxHeaderSize` 按 name+value 计数（2,500×`aaaaa: x` 通过、2,800 失败；16,350 B 值通过、16,380 失败）。
- **Rust 行为**：`header_read_timeout` 到期由 hyper 直接关闭连接，**没有 408**；408 只出现在 body `requestTimeout` 路径。431 阈值按原始头字节（含请求行/分隔符）求和，可比 Node 更早拒绝；431/408 是否带 `content-length: 0` 尚未逐字节比对。
- **范围**：仅头超时、body 超时、>16 KiB 头路径。
- **机器判据**：现有 `partial_headers_are_closed_after_headers_timeout`、`idle_keep_alive_connections_close_at_headers_timeout`、`oversized_headers_are_rejected_like_node`、`many_small_headers_are_not_capped_at_the_documented_two_thousand`；头部 408 形状与 name+value 计数需新增对照（当前会红）。
- **批准记录**：无；原条目“状态码与 Node 一致”与实测矛盾，需改写；**未批准**。

### A7.4-HTTPSTORE-KEEPALIVE-001-R1 · 更正

- **类别**：连接生命周期。
- **TS 依据**：Node 默认 `keepAliveTimeout = 5000`。
- **Rust 行为**：仍未实现独立 5 s keep-alive 超时，空闲连接由 60 s 头超时兜底；`close()` 会主动关闭空闲连接。
- **范围**：空闲 keep-alive 连接；单请求不受影响。
- **机器判据**：`http_store_tests.rs:519 idle_keep_alive_connections_close_at_headers_timeout`（200 ms 配置验证兜底）、`:489 close_closes_idle_keep_alive_connections`；负控删除 header timeout 后 2 s 红。原“暂无”应替换为这两条。
- **批准记录**：无；仍为未对齐项，**未批准**。

### A7.4-HTTPSTORE-CLIENT-ERROR-CLASS-001-R1 · 更正

- **类别**：错误对象形状。
- **TS 依据**：`:140-168` 只把 `response.json()` 失败吞成 undefined；网络/中止异常仍是 TypeError / AbortError（类名不可观察）。
- **Rust 行为**：`HttpStoreTransportError` 现在只有 `message`，已删除 `invalid_response` 标志；**响应头之后**的 body 失败不再算 transport 错误（body=undefined，按状态码出文案），只有头之前的网络/中止仍是 message-only。
- **范围**：client 错误面；成功路径与 body-failure 文案已对齐。
- **机器判据**：`clientRequests` 的 message 金样；`http_store_tests.rs:955 client_body_failures_follow_typescript_status_order`（六例状态优先）。原条目提到的 `invalid_response` 标志已不存在，需改写。
- **批准记录**：无；建议缩小为“网络/中止不保留 JS 类名”，**未批准**。

### A7.4-HTTPSTORE-CLIENT-BODY-UNITS-001-R1 · 更正

- **类别**：字符串上限语义。
- **TS 依据**：`Response.text()` 受 V8 `MAX_STRING_LENGTH`（UTF-16 码元）约束（`:154`）。
- **Rust 行为**：`fetch_redirect::read_body` 按 UTF-8 字节累计；2xx 超限 → `returned an invalid response.`，非 2xx → `failed: HTTP n`（状态优先）。
- **范围**：仅接近 512 MB 的超大响应，多字节文本可能更早触发。
- **机器判据**：`http_store_tests.rs:1000 client_body_limit_counts_utf8_bytes_not_utf16_units`（64 B/17 CJK 字符），负控放大上限后 `expect_err` 红；`clientBodyFailures` 的 oversized 六例。原“暂无”应替换。
- **批准记录**：无；仍为未对齐项，**未批准**。

### A7.4-HTTPSTORE-AUTH-BEFORE-BODY-001-R1 · 更正（可选项，按候选登记）

- **类别**：处理顺序 / 资源边界。
- **TS 依据**：`:515-523` 先收完整 body 再进 handler；401 在 `:194-204`。
- **Rust 行为**：405/404/401 与 Content-Length 检查先于读 body；未知/未认证的大 body 立即 401。
- **范围**：HTTP server 包装层；handler API 仍接收完整 body。
- **机器判据**：原有 `request_body_limit_rejects_oversized_content_length_before_reading`；建议追加 `http_store_tests.rs:576 anonymous_oversized_body_is_rejected_before_reading`（4 GiB CL，150 ms 内 401）。负控把 precheck 后移即 413 红。
- **批准记录**：无；行为未变，仅补强判据，**未批准**。

## A6-JSVALUE-DEPTH-001 追加说明（#495 / PR #539，10-23）

- **追加（不改原条目）**：#495（PR #539）修复轮 2 起，`CachePlan.ts:buildCachePlan` 的 stableSerialize 深度
行为按入口分开，均为各入口本 PR 之前的既有行为：runtime 路径
（`PromptCacheCoordinator.create_plan` 与消息指纹）保持 `js_value::MAX_ARRAY_DEPTH`
的 2000 容器门限与 `RangeError: Maximum call stack size exceeded` 原文，故 1998 层
嵌套数组 `inputSchema`（2001 容器）被拒绝而真实 TS 经真实入口实测成功（最深实测
2398 层数组成功，fp `b84cd74c…e87467`）；host 路径
（`finalizePreparedModelRequest`）保持无门限的显式栈，2398 层数组成功（与 TS 一致），
2100 层 `{x:…}` 对象成功序列化而真实 TS 两入口重复实测均报
`RangeError: Maximum call stack size exceeded`（V8 栈深随帧大小与容器形状变化，
数组 2398 层成功、对象约 2100 层溢出）。V8 的真实边界无法用常数逐字复刻；两边钉住
的是 Rust 既有行为（期望值取自本 PR 之前的代码），不是 TS 对齐。机器判据：
`src/pilotdeck-gateway/src/runtime/context/cache.rs` `runtime::context::cache::tests::depth_boundaries_match_pre_pr_rust_behavior` 与
`src/pilotdeck-gateway/src/host/modules/context/request.rs` `host::modules::context::request::tests::deep_input_schema_fingerprints_reach_the_host_entry`。 判据依据决策卡 `_tmp/20261023-loop/card-v8-stack-depth.md`（AI 代批，待人复核）。**批准记录**：无；**已知未对齐（未批准）**。
- **A6-JSVALUE-DEPTH-001 追加说明的更正（追加，不改上文；PR #539 Grok r3）**：上文「对象约 2100 层溢出」不准、且 runtime 路径也是**双向**差异。Grok r3 新进程实测：嵌套 `{x:…}` 对象 1887 层成功、1888 层抛 `RangeError: Maximum call stack size exceeded`。因此 runtime 路径（2000 容器门限）有两段差异：① 约 1888–1997 层对象 TS 溢出、Rust 正常出指纹；② 2000 层以上数组 TS 可成功（实测至 2398 层）、Rust 拒绝。host 路径（无门限）只有「约 1888 层以上对象 TS 溢出、Rust 成功」一段。以上层数是本机、指定入口、指定形状的观测；不同入口、调用栈深度与 JIT 预热会让边界下移，不能当常数使用（决策卡 `_tmp/20261023-loop/card-v8-stack-depth.md`）。

## 人批记录 · #505（#502）补登的 32 条（2026-10-23）

> 只追加，不改上文各条目原文。人批原话见 `state/decisions-20261023.md`「#505 补登 32 条批复」。「接受」= 承认差异、知情接受，**不豁免判分**，相关场景照常判分；「修」的条目修完后再追加更正。

- **已消除、关闭**：`A6.7-HOOK-HTTPS-001`（PR #546 已实现，判据 `tests/hooks_https.rs::http_hooks_over_tls_match_real_ts`）。批准记录：人批 2026-10-23。
- **要求修** `A6.5-MCP-HTTPS-001`：修：复用 hook 已有 TLS 连接器，正由 #361-3 实现。批准记录：人批 2026-10-23（未修子项接受为已知差异，不豁免判分）。
- **要求修** `A6.5-MCP-HTTP-LIFETIME-001`：代理部分修（另开单）；关闭即丢流、首连失败不重试按已知差异接受。批准记录：人批 2026-10-23（未修子项接受为已知差异，不豁免判分）。
- **要求修** `A6.5-BASH-OS-001`：setsid 部分修（另开单）；stdio 为 pipe 而非 socketpair 按已知差异接受。批准记录：人批 2026-10-23（未修子项接受为已知差异，不豁免判分）。
- **要求修** `A6.5-BACKGROUND-PERSIST-001`：修（另开单）：spill + 快照持久化。批准记录：人批 2026-10-23（未修子项接受为已知差异，不豁免判分）。
- **要求修** `A7-HOST-RESOURCE-001`：默认 300 秒本地超时修（另开单）；队列上限按已知差异接受。批准记录：人批 2026-10-23（未修子项接受为已知差异，不豁免判分）。
- **接受（附运维约束）** `A6.2-TRANSCRIPT-LOCK-001`：切换前停掉 TS writer；不对 transcript 做硬链接。批准记录：人批 2026-10-23，接受为已知差异，不豁免判分。
- **接受为已知差异（不豁免判分）**，中风险 14 条、低风险 11 条，批准记录：人批 2026-10-23：`A4-MIXED-BATCH-ORDER-001`、`A6-COLLATION-PUA-001`、`A6-JSVALUE-DEPTH-001`、`A6.2-TRANSCRIPT-COLLATION-001`、`A6.2-TRANSCRIPT-DOMAIN-001`、`A6.3-CONFIG-POLL-001`、`A6.4-CLOCK-TIMECLIP-001`、`A6.4-COUNT-CACHE-DEPTH-001`、`A6.4-JSONREPAIR-DEPTH-001`、`A6.4-MODEL-PROTOTYPE-001`、`A6.4-OPENAI-URL-001`、`A6.5-BASH-OUTPUT-001`、`A6.5-BASH-WNOWAIT-001`、`A6.5-MCP-DISPOSE-001`、`A6.5-MCP-OUTSCHEMA-SUPPORT-001`、`A6.5-TOOLS-CANCEL-001`、`A6.5-TOOLS-SCHEDULE-001`、`A6.6-FILEHISTORY-IO-001`、`A6.6-SNAPSHOT-CLOCK-001`、`A6.6-SNAPSHOT-COLLATION-001`、`A6.6-SNAPSHOT-IO-001`、`A6.7-HOOK-RESOURCE-001`、`A7-ADMISSION-FENCE-001`、`A7-LIFECYCLE-NFKC-001`、`A7-PROJECT-DISPOSE-001`。

## 已知未对齐（未批准，待人批）· A7.2 turn 读面

### A7.2-READ-LASTSEQ-DEPTH-001 · run_events 对极深嵌套 lastSeq 不复刻 V8 栈溢出

- **类别**：运行时边界（V8 调用栈深度），与 A6-JSVALUE-DEPTH-001 同族。
- **TS 依据**（cc7c4e6）：`src/gateway/client/InProcessGateway.ts:2541-2542` 对 `lastSeq` 做 JS 关系比较（ToPrimitive / ToNumber 递归）。PR #536 实现方真实 gateway 实测：包裹数字 1 的嵌套数组 2530 层成功（gap:true），2540 层抛 `RangeError: Maximum call stack size exceeded`；该临界层数随帧大小与容器形状变化，不是常数。
- **Rust 行为**：`src/pilotdeck-gateway/src/runtime/turn/read.rs:164、183` 的读面转换用显式迭代、不设深度门限：约 2530 层以内与 TS 一致；更深时 Rust 成功转换，TS 抛 RangeError。Rust 自身不会栈溢出。
- **范围**：仅持久化 run registry 快照中 `lastSeq` 为 2500 层以上嵌套容器的病态输入（正常为数字）；不放宽其他 lastSeq 取值（字符串、null、布尔、ToPrimitive 抛错等）的对拍。
- **机器判据**：读面测试中 2000 / 2001 层场景与 TS 一致；病态深度只断言 Rust 有界返回、不崩溃，不钉 V8 临界层数（见 `_tmp/20261023-loop/card-v8-stack-depth.md`）。
- **批准记录**：无；PR #536 codex r3 P1 指出，协调者按决策卡（AI 代批、待人复核）登记为已知未对齐；**已知未对齐（未批准）**。
- **A7.2-READ-LASTSEQ-DEPTH-001 判据更正（追加）**：上一条「病态深度只断言 Rust 有界返回、不崩溃」在登记时尚无对应测试——现有只有 `run_events_deep_array_last_seq`（2001 层，对真实 TS 金样）。病态深度的有界测试由 PR #536 修复轮 3 补；补上之前，该句不构成机器判据。
- **A7.2-READ-LASTSEQ-DEPTH-001 判据更正二（追加）**：病态深度有界测试已由 PR #536 修复轮 3 补上——`src/pilotdeck-gateway/src/runtime/turn/read_tests.rs:623` `run_events_pathological_depth_last_seq_is_bounded`（10 万层嵌套，断言返回 gap:true 且有界、不崩溃；协调者沙箱外实跑 1/1，0.04 s）。上一条更正所说「该句暂不构成机器判据」自此不再适用。
- **A7.2-READ-LASTSEQ-DEPTH-001 判据更正三（追加；PR #536 codex r4）**：更正二引用的进程内 2 s `tokio::time::timeout` 约束不了不让出执行权的同步转换，不构成「有界」判据。修复轮 4 起机器判据为 `src/pilotdeck-gateway/src/runtime/turn/read_tests.rs` 的 `run_events_pathological_depth_last_seq_is_bounded`：父测试以子进程运行 `run_events_pathological_depth_last_seq_child`（10 万层），由父进程施加可终止期限；同步自旋与改回递归两条负控均转红。**范围更正**：真实 TS 的 2530 / 2540 层观测是经 `runEvents` + 内存 registry 端口取得；经文件 registry 时 `structuredClone` 在约 2000 层已先失败，所以本差异只在非文件 registry 的端口下可观察。
- **A7.2-READ-LASTSEQ-DEPTH-001 判据更正四（追加，不改上文；PR #536 gate5 boundary P1/P2）**：(1) 更正三「本差异只在非文件 registry 的端口下可观察」不成立：经生产文件 registry 时，TS `FileRunRegistry.get`（`src/gateway/run/RunRegistry.ts:271`）对整条记录 `structuredClone`，约 2000 层即抛 RangeError；Rust `read_snapshot`（storage/run_registry.rs:595）用无深度界的迭代 codec 成功返回记录，读面随后正常算出 gap / 返回投影——即文件 registry 路径上同样是「TS 抛、Rust 成功」，只是 TS 的失败点从比较提前到 get，属同一已登记差异在生产路径上的表现。(2) 更正三所称「同步自旋与改回递归两条负控均转红」此前只在树外临时执行；修复轮 5 已作为 `lastseq-conversion-spin`、`join-string-recursive` 两条 mutant 进仓内 `src/pilotdeck-gateway/tests/fixtures/turn-read/negative.py`，目标均为 `runtime::turn::read::tests::run_events_pathological_depth_last_seq_is_bounded`（实跑：自旋 10.02 s 因父进程期限失败，递归子进程栈溢出失败）。状态不变：已知未对齐（未批准），按 V8 栈深判据卡（AI 代批，待人复核）处理。

## 向安全偏离（人批）· A6.5 MCP 工具结果自动读图

### A6.5-MCP-IMAGE-READ-SANDBOX-001 · MCP 工具结果自动读图限制

- **类别**：向安全偏离。
- **TS 依据**：固定 `cc7c4e6` 的 `src/mcp/runtime/PluginToToolBridge.ts:184,195-202` 只按点开头图片 Markdown 目标做 lexical resolve 并无界 readFileSync，不检查 cwd 边界、普通文件或大小；`src/context/attachments/AttachmentResolver.ts:37` 的普通图片附件默认上限为 5 MiB。本单新增真实 TS 金样验证正常 cwd 内图片的完整输出字节。
- **Rust 行为**：候选与授权目录 realpath 后按路径组件判包含，只允许真实 cwd 或可信宿主明确配置的白名单绝对目录；默认白名单空。只允许普通文件且 ≤5 MiB；逐级 NOFOLLOW 打开并复核句柄元数据与身份；实际读取 ≤cap+1 并拒绝超限。被拒绝的候选按“不是图片”处理，原工具结果文本/data/metadata 原样保留，不加错误。
- **范围**：仅 stdio 有非空 cwd、无 inline image 的工具结果本地图片自动投影；保留既有点开头正则、后缀 MIME/base64 规则。白名单配置位于 bridge 的 `CreateToolDefinitionsOptions.image_read_allowed_roots` 宿主 API，默认空；不新增 mcp.json 字段，不修改 MCP client/http、inline image 或其他附件处理。
- **机器判据**：`bridge/tests.rs` 覆盖正常内链/链式链接/符号链接 cwd、`../`、绝对目标、前缀碰撞、根外链接及链接链、白名单内外及无效项、目录/FIFO/设备、超限与恰好上限、打开前替换叶/祖先、打开后身份/类型/大小、fstat 后增长与实际读取上界；打开/读取探针断言不安全候选零打开/零读取。`tests/mcp_runtime.rs` 对完整正常 TS 输出逐字节比较，并验证宿主白名单接线和安全拒绝的文本/data/metadata；各类负控输出见本目录验证记录。
- **批准记录**：人批 2026-10-23，用户本人采纳建议 A；原话“按照建议 A 向安全偏离 TS。读图前先规范化真实路径（realpath），只允许 cwd 及其子目录（另可配置白名单）；拒绝符号链接越界”。决策卡主仓 `_tmp/20261023-loop/card-mcp-image-read.md`；实现 issue #508。交协调者登记，本树未修改 accepted-diffs.md / FROZEN.md。 补注（协调者）：5 MiB 大小上限与只读普通文件属卡中建议 A 原文所列（卡第 7 行），人原话未逐项复述；取值 5 MiB 为实现方按 TS 图片附件上限所定，AI 判断，待人复核。

## A7.2 turn replay 条目状态更新（A7.2-pr5，10-23）

- **A7.2-REPLAY-RETENTION-TIMER-001 当前状态（追加，不改原条目）**：在新的生产 TurnGateway 宿主内**已消除**。`src/pilotdeck-gateway/src/runtime/turn/gateway.rs` 实现宿主串行计时器：终态回收到期只排队，宿主处理完当前步骤后再回收（人批 10-22 选 B）。机器判据：`src/pilotdeck-gateway/src/runtime/turn/gateway_tests.rs:631` `serial_retention_matches_real_ts_host_steps_and_dispose`、`:741` `serial_timer_cancels_queued_callbacks_and_replacements`（对真实 TS 宿主步骤金样）；负控 `expiry-runs-on-clock-thread`（让到期在时钟线程直接回收）转红，见 `tests/fixtures/turn-gateway/`。**适用范围**：仅经 TurnGateway 宿主的回收；裸 `TurnReplayStore` 默认 `ThreadTimer` 保持原库级行为，脱离宿主时原条目描述的差异仍在，不能宣称库本身已有 JS 步骤边界。

## 已知未对齐（未批准，待人批）· A6.6 EdgeClaw memory entries

### A6.6-EDGECLAW-ENTRY-PATH-CONTAIN-001 · entry id 不得越出 store 根（向安全偏离）

- **类别**：向安全偏离（基线允许路径穿越 → 拒绝越界）。
- **TS 依据**（cc7c4e6）：`src/context/memory/edgeclaw-memory-core/src/core/file-memory.ts:665-668` 的首个判断放行规范化后的 `../`，PR #537 Grok r1 实测可删改 store 根外文件；缺条目时的错误见 `:1224-1225`。
- **Rust 行为**：`src/pilotdeck-gateway/src/runtime/memory/edgeclaw/file_memory.rs` 对 entry id 拒绝 `..` 段、绝对路径与解析后越出对应 store 根的符号链接，返回 `Memory entry not found: <relative>`（:397），整批 mutation 先预检。
- **范围**：workspace / global / 外部 workspace store 的读、写、edit、delete、deprecate、restore 与 repository 对应入口；不改变 store 根本身的可信身份；合法 id 的行为与 TS 对拍不变。
- **机器判据**：`src/pilotdeck-gateway/tests/edgeclaw_entries.rs:723` `entry_paths_reject_escape_without_touching_outside_files`（各入口 × 越界形式，断言根外文件与全树原样）；负控：去掉校验 / 去掉符号链接检查 / 去掉批量预检均转红。
- **批准记录**：无；协调者按决策卡 `_tmp/20261023-loop/card-edgeclaw-entry-traversal.md` 选 A 自决（依据人批先例 #496），AI 代批、待人复核；**已知未对齐（未批准）**。与 #508（MCP 读图越出 cwd）同类，#508 仍待人定。

### A6.6-EDGECLAW-ENTRY-IDS-SHAPE-001 · 按 ids 批量操作的四个入口不接受非字符串 / 非数组 ids（接口边界）

- **类别**：接口边界（已知未对齐，未批准）。
- **TS 依据**：真实 TS 实测（PR #537 修复轮 5，金样 `tests/fixtures/edgeclaw-entries/oracle.json` 的 idAudit 部分）。TS 的 get 对六种非字符串元素返回 `[]`；delete/deprecate/restore 对 undefined/null 元素分别抛 `Cannot read properties of undefined (reading 'replace')` / `Cannot read properties of null (reading 'replace')`，其余四种抛 `relativePath.replace is not a function`（均 TypeError）。对这四个入口，TS ids 参数本身缺失/null 时分别抛 `Cannot read properties of undefined (reading 'filter')` / `Cannot read properties of null (reading 'filter')`；number/object/bool 参数抛 `ids.filter is not a function`，均 TypeError。Rust slice 参数无法表达这些形状。
- **Rust 行为**：`runtime/memory/edgeclaw/storage/entries.rs` 的 `get_memory_records_by_ids`（:91）、`delete_memory_entries`（:223）、`deprecate_memory_entries`（:226）、`restore_memory_entries`（:229）签名为 `&[JsString]`，类型上无法收到上述形状；对应动态输入由将来的路由 / 适配层决定如何拒绝，本 PR 不在测试侧强转冒充同输入对拍。archive 与 edit 的动态形状已全部对拍通过，不在本条。
- **范围**：仅上述四个入口的 ids 参数形状；字符串 ids 的行为已与 TS 对拍一致。接入客户端动态入口时须按本条补对拍，或在入口按 TS 文案抛 TypeError。
- **机器判据**：暂无（强类型签名本身即边界）；接线时由接线 PR 以 idAudit 金样补判据。
- **批准记录**：无；PR #537 修复轮 5 实现方列为残余差异候选，协调者核对后登记；**已知未对齐（未批准，待人批）**，不新增豁免、不改判分语义。

### A6.6-EDGECLAW-ENTRY-PATH-CONTAIN-001 · 范围补充：根内悬挂符号链接（追加，不改上文；PR #537）

- **类别**：向安全偏离（范围补充）。
- **TS 依据**：TS 对根内 `link.md -> real.md`（real.md 缺失）会跟随链接：读得 ENOENT、写会创建 real.md（PR #537 gate5 boundary 第 2 条）。
- **Rust 行为**：`src/pilotdeck-gateway/src/runtime/memory/edgeclaw/file_memory.rs:418-455` 的 `within` 逐级解析已存在的祖先与叶，悬挂链接无法证明目标在根内，一律按 `Memory entry not found: <id>` 拒绝（读、写、删都一样）。
- **范围**：上文条目只写「解析后越出根」；本补充把「根内悬挂链接也拒绝」明确纳入，宽于 TS。
- **机器判据**：暂缺专门用例，补测由 #563 跟进（前置于 A6.6 关闭）。
- **批准记录**：无；与上文同一政策（AI 代批，待人复核），不新增豁免、不改判分语义。

### A6.6-EDGECLAW-SNAPSHOT-CPSYNC-ERROR-001 · 快照 rename 回退的目录复制失败时错误形状与 Node cpSync 不同（已知未对齐）

- **类别**：错误文本 / 错误对象形状未对齐（仅失败路径）。
- **TS 依据**：Node v22.22.0 目录 `cpSync` 失败时抛 `Error`，`syscall` 为 `cp`，`errno` 为正数，只有 `path`（指向目标）没有 `dest`，message 形如 `EACCES, Permission denied '<dst>/x'`（PR #537 Grok r6 第 1 条，同一组沙盒场景 Node 与 Rust 探针逐字对比实测）。
- **Rust 行为**：`src/pilotdeck-gateway/src/runtime/memory/edgeclaw/storage/snapshots.rs:197-236` 的 `copy_tree` 按逐个系统调用包装：目标不可写时报 `EACCES: permission denied, mkdir '…'`，源目录内有不可读文件时报 `…, copyfile '<src>' -> '<dst>'`。
- **范围**：仅快照 rename 失败（EPERM / EACCES）后走 cpSync 等价回退、且回退本身失败的路径；成功路径与其他快照 IO 已对齐（修复轮 6–7 的 io / copy-io 金样）。这是生产上唯一能走到的该回退路径。
- **机器判据**：暂无（修复轮 7 的 copy-io 金样只覆盖逐调用包装；cp 风格整体错误未做对拍）。
- **批准记录**：无；实现方在修复轮 7 RESULT「范围外」披露，Grok r6 要求登记，协调者登记；**已知未对齐（未批准，待人批）**，不新增豁免、不改判分语义。

## 已知未对齐（未批准，待人批）· A6.3 插件 loader

### A6.3-PLUGIN-MARKDOWN-CYCLE-001 · markdown 目录遍历跳过指向祖先目录的符号链接回边（向安全偏离）

- **类别**：向安全偏离（基线继承性挂起与资源放大 → 有界遍历）。
- **TS 依据**（cc7c4e6）：插件 commands / skills / output-styles 的 Markdown 目录遍历跟随符号链接、无防环。PR #484 实现方实测：单条 `commands/link0 -> ..` 由系统 ELOOP 截止，返回 32 个重复路径的命令；两条 `link0 / link1 -> ..` 的分叉回边连续两次在 3000 ms 探针截止时仍未返回（限时观察，不等于已证明无限）。证据：`src/pilotdeck-gateway/tests/fixtures/plugin-loader/regressions.json` 的 cycleCases、`_tmp/20261023-issue-484-plugins/ts-cycle-initial.log`。
- **Rust 行为**：`src/pilotdeck-gateway/src/runtime/plugins/loader/markdown.rs` 记录 DFS 祖先目录，遇指向祖先的符号链接回边时跳过该子树，保留已加载与其他非循环贡献、保留非循环别名各自的贡献。可观察差异：循环路径产生的重复贡献不再返回，分叉回边在有限时间内返回。
- **范围**：仅符号链接构成祖先回边的目录；无环目录与非循环别名的结果与 TS 一致。
- **机器判据**：`src/pilotdeck-gateway/tests/plugin_regressions.rs:132` `plugin_markdown_cycle_is_bounded`（子进程运行、带超时）、`:193` `plugin_markdown_acyclic_aliases_are_preserved`；负控见 `tests/fixtures/plugin-loader/negative-controls.py`。
- **批准记录**：无；依据人批先例 #496（TS 有挂起 / 越权缺陷时 Rust 向安全偏离并登记）由协调者在 #484 任务书中授权实现，**AI 代批，待人复核**；**已知未对齐（未批准）**。同类：#541（skills import tree_files）。

### A6.3-PLUGIN-MARKDOWN-CYCLE-001-R1 · 更正首版（原文保留在上方）：markdown 遍历全局目录去重与总遍历预算（向安全偏离）

- **类别**：向安全偏离（目录互链 / 别名图的路径放大，以全局目录去重与总遍历预算限制）。
- **TS 依据**（cc7c4e6）：`src/extension/plugins/loading/PluginCommandLoader.ts:79-102` 跟随目录链接、不去重、不核插件根。真实 TS 金样 `src/pilotdeck-gateway/tests/fixtures/plugin-loader/traversal.json`：兄弟目录两两互链 k=3/7/10/20 均在 1000 ms 截止时未返回（只登记观测，不声称证明永久挂起）；无环的物理目录别名 left / right 返回两条；depth-65 与 entries-4097 成功。
- **Rust 行为**：`src/pilotdeck-gateway/src/runtime/plugins/loader/markdown.rs` 对每个配置贡献目录独立使用全局 `(dev, ino)` 访问集，已访问的物理目录不再展开；最多枚举 4096 条目、进入 64 层，超限报 `Plugin markdown traversal limit exceeded.`，配置入口沿用 TS 的每目录 error → [] 边界（丢弃该目录全部结果）。命名与路径保留首次词法访问。
- **范围**：插件 commands / skills / output-styles 与 style-only 的每个配置目录遍历，含祖先回边、兄弟互链、重复物理目录别名、超出预算的无环树。**可观察差异**：① 无环的物理目录别名 TS 两条、Rust 一条（首次路径）；② 4097 条目以上或 65 层以上的无环树 TS 成功、Rust 该目录为空；③ 成环 / 互链目录 TS 不返回、Rust 有界返回。Unix 用 dev+ino，非 Unix 回退规范化路径（未验证）。不同配置目录之间不共享访问集与预算。
- **机器判据**：`src/pilotdeck-gateway/tests/plugin_traversal.rs` 的 `plugin_markdown_clique_03/07/10/20`（各为 1 s 上限的子进程）、`plugin_markdown_alias_first_path`、`plugin_markdown_entry_budget`、`plugin_markdown_depth_budget`；负控（恢复只查祖先的算法实际超时，alias / entry / depth 各自命中）见 `tests/fixtures/plugin-loader/negative-controls.py`，协调者 rebase 后沙箱外整跑 58/58 检出（`_tmp/20261023-issue-484-plugins/coord-negative-postrebase/summary.json`）。
- **批准记录**：无；依据人批先例 #496 由协调者在 #484 任务书中授权实现，**AI 代批，待人复核**；**已知未对齐（未批准）**。更正（PR #544 修复轮 1）：本条首版只按 DFS 祖先防环、引用 `plugin_regressions.rs` 中已移走的测试，并写有「非循环别名各自贡献、与 TS 一致」——Grok r1 实测互链按阶乘增长，codex 修复轮 1 真实 TS 实测别名 TS 两条、Rust 一条，均已按上文改正。同类：A6.8-SKILLS-TREEFILES-001（#541）。

### A6.3-PLUGIN-MARKDOWN-ROOT-001 · 插件 markdown 不经符号链接读取插件根外文件（向安全偏离）

- **类别**：向安全偏离（阻止插件贡献经目录 / 文件符号链接读取插件根外 Markdown）。
- **TS 依据**（cc7c4e6）：`PluginCommandLoader.ts:89-99` stat 跟随链接，`:68` 按词法路径 readFile，不核插件根。真实 TS `traversal.json` 的 boundary-commands / skills / output-styles、boundary-plugin、boundary-style-only 均读到根外 SECRET（PR #544 Grok r1 同样实测读到 `outside/secret/leak.md`）；不可读的 stat / readdir 条目在 `:82-95` 跳过。
- **Rust 行为**：`markdown.rs` 规范化插件根、配置根和每个条目，要求按路径组件仍位于规范化插件根内；越界、断链或无法规范化的条目跳过，不读根外内容，也不因一个越界条目丢掉安全的兄弟条目；配置 base 的 `..` 或越界符号链接同样不能绕过。首次词法命名保留，读取经核准的规范化文件。
- **范围**：上述三类插件贡献与 style-only；`load_plugin_commands(plugin, base)` 直接入口以 base 为安全根，配置入口以实际插件根为界；插件根本身可以是合法的符号链接（以其物理根为界）。只覆盖本次加载时解析的链接，不提供对抗并发改写、特殊文件或整个进程磁盘读取的沙箱。
- **机器判据**：`src/pilotdeck-gateway/tests/plugin_traversal.rs` 的 `plugin_markdown_root_boundary`（相似文本前缀、目录链接、文件链接、链接链、SKILL.md 链接、三个贡献族、style-only、越界配置 base、安全兄弟保留）；目录 / 文件 / 链接链 / 配置根四条独立负控实际读到越界内容并命中断言。原 oracle 中两条已知根外贡献只在测试中精确排除，原金样未改。
- **批准记录**：无；依据人批先例 #496 由协调者在 #484 修复轮 1 任务书中授权，**AI 代批，待人复核**；**已知未对齐（未批准）**。同类：#508（MCP 读图越出 cwd，仍待人定）、A6.6-EDGECLAW-ENTRY-PATH-CONTAIN-001。

### A6.3-PLUGIN-MARKDOWN-CYCLE-001-R2 · 更正 R1 的 TS 环行为与预算口径，补充指数别名 DAG（首版与 R1 原文保留在上方）

- **类别**：向安全偏离（目录互链与别名图的重复展开 → 每物理目录一次；Markdown 候选与深度设限，并对拒绝结果给出诊断）。
- **TS 依据**（cc7c4e6）：`PluginCommandLoader.ts:79-102` 递归跟随目录链接、不去重，stat 遇 ELOOP 等错误跳过；`PluginLoader.ts:93` 静默对每配置目录 error→[]。原 traversal.json 的 k=3/7/10/20 互链团只证明 1000 ms 截止时未返回，不能泛化为所有环永久不返回。新增 budget.json 的简单互链成功返回 64 条、2 个唯一正文；12 层无环双别名 DAG 成功返回 16369 条、13 个唯一正文；含 4800 个非 Markdown 资产的插件返回 3 个 skill。两次真实 TS 输出字节一致。
- **Rust 行为**：每配置贡献目录独立全局目录身份去重，保留首次词法访问；预算只计通过根检查的 `.md` 候选，最多 4096 个、进入目录深度最多 64 层，目录和非 Markdown 文件不计候选预算。超限仍返回 `Plugin markdown traversal limit exceeded.`，配置入口只丢弃该目录全部结果，并在 stderr 输出被丢弃目录、原因及两项限额；同配置数组其他目录继续保留。
- **范围**：插件 commands / skills / output-styles 与 style-only；Unix dev+ino，非 Unix 规范化路径回退未验证；不同配置目录不共享访问集或预算。**可观察差异重述**：① 非循环别名可以是任意多路径，含指数别名 DAG；TS 按路径重复贡献，Rust 每物理目录只展开一次（本次 DAG 16369→13，并保留各目录首次词法路径）。② 超过 4096 个 `.md` 候选或 64 层的目录树 TS 可成功，Rust 出错配置目录为空且额外输出诊断；超过 4096 个纯非 Markdown 资产本身不触发拒绝。③ **更正 R1 第三处**：成环/互链的 TS 行为取决于拓扑与系统链接解析限制；简单回边可以在 ELOOP 截止后成功返回重复条目，分叉互链可在探针截止时仍未返回，不再写「成环 / 互链目录 TS 不返回」。Rust 均以全局物理目录去重有界返回；简单环本次 64→2，词法路径可能不同于 TS 反复回边后得到的首次路径。候选预算不再充当总枚举量/目录数量的上限。
- **机器判据**：`src/pilotdeck-gateway/tests/plugin_traversal.rs:284` 资产插件完整 TS 回放；`:329` DAG 首次路径回放与 24 层独立压力；`:389` 简单环去重；`:418` 两种超限、四入口诊断、目录边界与兄弟保留；原有 entry/depth/clique/alias 判据仍通过。`negative-controls.py:144` 起四项新负控和五项相关旧负控实测 9/9 命中；金样双跑 SHA-256 与上文一致。最终 fmt / clippy / 相关测试原始输出见本 RESULT。
- **批准记录**：没有新增人批或豁免。本轮用户修复任务书明确授权代码修复、候选预算与可观察诊断；账本由协调者按只追加写 R2，**AI 代批，待人复核；已知未对齐（未批准）**。本交付不修改冻结文件，不将候选当成已批准差异。

### A6.3-PLUGIN-MARKDOWN-CYCLE-001-R3 · 修复轮 3 去掉候选预算与深度上限（首版、R1、R2 原文保留在上方）

- **类别**：向安全偏离（循环/互链及别名图按物理目录去重），并消除 R2 的预算/诊断偏离。
- **TS 依据**（cc7c4e6）：`PluginCommandLoader.ts:79-103` 按路径递归跟随目录、不去重；`PluginLoader.ts:85-103` 每配置目录 catch→[]，没有 Markdown 候选、枚举条目或 64 层深度上限。简单环可因 stat 遇 ELOOP 后返回重复贡献；k=3/7/10/20 互链团的真实探针仅证明在 1000 ms 截止时未返回，不断言永久不返回。真实依赖树、混合 README/JS、80 层无环输入及 65 层/4097 条目均有真实 TS 金样。
- **Rust 行为**：每配置贡献目录独立维护全局物理目录身份集合，每物理目录最多展开一次，保留首次词法访问的命名与路径；显式栈迭代。没有条目、Markdown 候选预算或深度上限，没有 `Plugin markdown traversal limit exceeded.` 错误及对应 stderr 诊断。既有 ROOT-001 插件根边界单独保持。
- **范围**：插件 commands / skills / output-styles 与 style-only；Unix dev+ino，非 Unix 原路径身份回退未验证，不同配置目录不共享访问集。CYCLE-001 的可观察差异只剩：① 别名（含无环指数 DAG）TS 按路径重复，Rust 每物理目录一次；② 成环/互链 TS 不一定在期限内返回，Rust 通过去重有界返回；简单环 TS 可返回重复贡献，其首次词法路径与 Rust 的首次物理访问路径可能不同。R2 ②关于 >4096 个 .md 或 >64 层时 Rust 丢弃整目录并额外诊断的差异整条消除。
- **机器判据**：`plugin_traversal.rs` 的宽/深成功回放、真实依赖树与 README/JS 全字段回放、深 node_modules 四入口全字段回放、互链团子进程截止、DAG 首次路径及独立压力、简单环去重；恢复候选预算/深度上限以及移除全局去重的负控与原始输出见本节。
- **批准记录**：无人批。删除条目预算与深度上限、保留全局物理目录去重与 ROOT-001，依据的是协调者修复任务书（Grok r3 P1 后的协调者判断，**AI 代批，待人复核**），不是人类授权；不改判分语义、不新增豁免。

### A6.3-PLUGIN-MARKDOWN-ROOT-001-R1 · 补根外不可读 Markdown 的错误路径范围与判据（首版原文保留在上方；PR #544 修复轮 4）

- 类别：既有根包含策略的错误路径补证，无新增产品行为、无新豁免。
- TS 依据：PluginCommandLoader.ts:92 的 stat 失败仅跳过该条目；但叶文件 chmod 000 不阻止 stat，`:68` readFile 返回 EACCES，PluginLoader.ts:93 会把该配置目录整体变成 []。本轮原生权限探针与真实 TS 记录区分了这两条路径，不能统称“TS stat 失败”。
- Rust 行为：规范化条目越界即跳过，不读取外部文件；因此该外部 Markdown 虽不可读，根内安全贡献仍保留。预期保留对象来自同输入去掉外部链接后的真实 TS control。
- 范围：commands / skills / output-styles / style-only 的每配置目录；适用于规范化根外叶文件/目录及配置 base symlink。插件根自身为 symlink 时仍保留合法词法贡献；绝对配置遵守 TS Node join，不扩大插件根。并发替换链接的 TOCTOU 不由这些静态测试证明。
- 机器判据：四个新边界回归全字段回放、真实 TS 双跑和上述专属负控；ROOT-001 未扩大为整个 loader 的安全沙箱承诺。
- 批准/登记状态：无人批。实现行为沿用 ROOT-001 既有状态（AI 代批，待人复核）；本条只补错误路径的范围与判据，依据协调者修复任务书（不是人类授权），不新增豁免、不改判分语义。

### A6.3-PLUGIN-MARKDOWN-CYCLE-001-R4 · 机器判据更正：互链团有界测试不再用 1 s 墙钟（首版与 R1–R3 原文保留在上方；PR #544 gate6 sibling P1）

- **更正**：R1 机器判据所写「互链团各为 1 s 上限的子进程」已不成立。修复轮 4（提交 `91ad75bc`）起，`src/pilotdeck-gateway/tests/plugin_traversal.rs` 的 `wait_for_traversal`（:85）只施加 20 s 父进程可终止期限、仅用于防挂起；有界性改由 `assert_directory_expansions`（:101）以确定性计数断言（每个物理目录恰展开一次，展开数 = 唯一目录数），不依赖墙钟。四个互链团用例 `plugin_markdown_clique_03/07/10/20` 照此判定；协调者沙箱外 4×4 并行压测 clique_03、clique_20 各 16/16。
- **TS 依据不变**：TS 探针里的 1000 ms 只是截断未返回的 TS 探针的预算，不是 Rust 通过上界（fixture README 已写明）。
- **批准记录**：无；判据更正，不改判分语义、不新增豁免；R1–R3 的状态（AI 代批，待人复核）不变。

### A6.5-MCP-HTTPS-001 · 当前状态（追加，不改上文；PR #361-3）

- **当前状态**：已实现。streamable HTTP 与 SSE 的 https 初始 URL 复用 `runtime/model/http.rs` 的共享 webpki 连接器，connect 阶段（TCP + TLS 握手）包 10 s 期限（与 undici 默认 connectTimeout 一致；`ConnectDeadline` 从 hooks 移到共享位置，模型连接器自身行为不变）。
- **机器判据**：`src/pilotdeck-gateway/tests/mcp_https.rs` 的 `replays_ts_mcp_https_over_local_tls`（本地 TLS 上回放真实 TS 金样：initialize / tools/list / tools/call、重定向、握手卡住、端口关闭）、`production_roots_do_not_trust_the_fixture_ca`、`mcp_https_fixtures_regenerate_identically_from_ts`（ignored，真实 TS 两次生成逐字节一致）。
- **批准记录**：人批 2026-10-23「A6.5-MCP-HTTPS-001 … 修」；本条为修复完成的状态追加。


### A6.5-MCP-HTTPS-001 · 状态追加的更正（追加，不改上文；#361-3 修复轮 1）

- **类别**：状态追加更正（补齐字段/可追溯批准，修正「机器判据」覆盖范围）。
- **TS 依据**：`mcp/client/McpClient.ts:163-205`；SDK `sse.js:70`、`streamableHttp.js:89,306,443`；undici `connect.js:69`（10 s connectTimeout）、`util.js:904-918`（timeout 文案）、eventsource `dist/index.js:48-49,98`（cause 展平）。
- **Rust 行为**：`runtime/mcp/http.rs` 的 `parse_endpoint` 接受 http/https，`HttpLink::with_config` 用共享 `ConnectDeadline` 罩 10 s 整连；SSE 首连失败文本已对齐四类失败（自签、握手卡住、显式拒连、443 默认端口）；streamable 仍按 `strip_cause` 只报 `fetch failed`。
- **范围**：MCP streamable_http / sse 的 http(s) 端点；残余的信任源与证书文案差异见 B、D。非 http(s) 见 C。
- **机器判据**：`src/pilotdeck-gateway/tests/mcp_https.rs` 的 `replays_ts_mcp_https_over_local_tls`（12 场景 / 42 步 / 30 请求，oracle sha256 `92e1759a3b60501d8a20cc0d82ce2f59c0ce2398e3812c9da07ceebc2a77272b`）、`production_roots_do_not_trust_the_fixture_ca`、`mcp_https_fixtures_regenerate_identically_from_ts`（ignored，真实 TS 重生成一次并与入库比对）。**账本该行括注「真实 TS 两次生成逐字节一致」应改为「重生成一次与入库比对；两次独立运行见 RESULT §3」**（`tests/common/baseline.rs:106-126` 每次只 spawn 一次生成器）。
- **批准记录**：人批 2026-10-23「A6.5-MCP-HTTPS-001 … 修」，原话见 `state/decisions-20261023.md`「#505 补登 32 条批复」（PR #550 合入）；本条为修复完成后的状态更正。

### A6.5-MCP-TLS-TRUST-001 · MCP https 的信任源与代理不同于 Node（未批准）

- **类别**：未实现（信任源 / 代理）。
- **TS 依据**：Node `fetch` 在进程启动时读取 `NODE_EXTRA_CA_CERTS` 扩展信任（本夹具生成器正是靠它让真实 TS 信任夹具 CA，`generate.mts:46-67`）；TS CLI 启动时安装 undici proxy dispatcher（`cli/proxy.ts`，与 A6.7-HOOK-TLS-TRUST-001 同源依据）。
- **Rust 行为**：`McpClient::new()` 只信 webpki（Mozilla bundle），不读 `NODE_EXTRA_CA_CERTS`、不读 OS 信任库、不走代理 dispatcher；私有 CA 的 https MCP 在 Rust 报 `fetch failed`、TS 成功。
- **范围**：用私有 CA、OS 信任库或依赖代理的 https MCP。扩展信任源会改变安全边界，需产品决定。
- **机器判据**：暂无（`production_roots_do_not_trust_the_fixture_ca` 只证明夹具 CA 不进生产；生成器的 `NODE_EXTRA_CA_CERTS` 是 TS 会读该变量的反证）。
- **批准记录**：无；**已知未对齐（未批准，待人批）**。

### A6.5-MCP-SCHEME-001 · 非 http(s) 的失败时机不同（未批准）

- **类别**：错误时机 / fail-closed 差异。
- **TS 依据**：`McpClient.buildTransport()` 只做 `new URL(spec.url)`，不检查协议；非 http(s) 要等 `fetch` / `EventSource` 才失败（`sse.js:70`、`streamableHttp.js` 的 `fetch`）。
- **Rust 行为**：`runtime/mcp/http.rs:parse_endpoint` 在 buildTransport 同步阶段立即返回 `mcp_unsupported_transport`（模块文档已披露为故意差异）。
- **范围**：仅非 `http:`/`https:` 的 MCP URL。
- **机器判据**：`runtime::mcp::http::tests::http_and_https_urls_are_served`（ftp → `mcp_unsupported_transport`；无 TS 金样）。
- **批准记录**：无；**已知未对齐（未批准）**。

### A6.5-MCP-TLS-CAUSE-001 · SSE 证书失败 cause 文案的两处残余（未批准）

- **类别**：错误文本未对齐（仅失败路径）。
- **TS 依据**：实测 Node v22.22.0：自签叶子 `self-signed certificate`；私有 CA 签发但不被信任 `unable to verify the first certificate`；主机名不匹配 `Hostname/IP does not match certificate's altnames: Host: localhost. is not cert's CN: 127.0.0.1`。
- **Rust 行为**：rustls 0.23 对所有不可验证链只给 `CertificateError::UnknownIssuer`，`io::Error` payload 里不带叶子证书，故统一映射为 `self-signed certificate`（私有 CA 场景文案因此偏）；`NotValidForName*` 拿不到证书 CN，未映射，SSE 仍报裸 `fetch failed`。
- **范围**：legacy SSE 首连的证书失败；成功路径与 streamable（`strip_cause`）不受影响。
- **机器判据**：`https-sse-certificate` 钉住自签文案；私有 CA 与主机名不匹配**无金样**（Rust 侧无法正确合成 Node 文案，未伪造期望值）。
- **批准记录**：无；fixture README「已知未对齐」已如实写明。

### A6.5-MCP-HTTPS-001 · 状态更正二（追加，不改上文；#361-3 修复轮 2 后 Grok r3）

- **更正**：上一条「状态追加的更正」机器判据里的金样计数与哈希（12 场景、oracle sha256 `92e1759a…`）已被修复轮 2 重新生成的金样取代：现入库 `src/pilotdeck-gateway/tests/fixtures/mcp-https/oracle.json` sha256 `f30e5d63c1ca0f52db8812db3e52c142f4434279a03d10287c86b38764af4155`（README 记两次独立运行一致），新增 `https-sse-refused-ipv6` 场景后共 13 个场景（计数取自 PR #555 Grok r3）。判据测试名不变。
- **批准记录**：同上文（人批 2026-10-23 要求修）；本条只更正判据引用，不改判分语义。

### A6.5-MCP-CONNECT-HOSTNAME-001 · 拒连 / 超时详情里主机名与 TS 的「解析后地址」不同（已知未对齐）

- **类别**：错误文本未对齐（仅失败路径）。
- **TS 依据**：undici 对主机名 URL 报实际尝试连接的解析地址，多地址时按地址族用逗号拼接（PR #555 GLM r3 第 1 条，已实测）。
- **Rust 行为**：`src/pilotdeck-gateway/src/runtime/mcp/http.rs` 的 `target_address`（约 :213-225）用 URL 里的主机名与端口拼接，主机名 URL 报 `localhost:P` 而非解析地址；IP 字面量（含 IPv6 去方括号）已与 TS 一致。单测 `:2562-2565` 对 `example.com:8443` 的期望值无 TS 依据，只是 Rust 自洽断言。
- **范围**：MCP streamable_http / sse 连接失败时 cause 文本里的地址部分，仅当 URL 用主机名时；成功路径不受影响。
- **机器判据**：暂无（回放金样只覆盖 IP 字面量；主机名分支变异下回放全绿，GLM r3 实测）。
- **批准记录**：无；**已知未对齐（未批准，待人批）**，不新增豁免、不改判分语义。

### A7.2-SUBMIT-FAILURE-GUARD-001 · 当前状态（追加，不改上文；#558）：原能力拒绝已消除

- **状态更正**：原条目保留。FailureGuard/classify 已移植，refresh 后 resolve 的启用配置不再返回 `FailureGuard runtime is unavailable; requires A7.2-pr4b.`；执行原生 submit，按真实 TS 对模型、工具和子代理错误计数，trip 后记录/发布 `run_policy_stopped` 并以 `run_policy:<code>` abort。新 turn 重置窗口，成功事件不清零；记录/abort 异常的 best-effort 行为与 TS 一致。
- **TS 依据**：`FailureGuard.ts:14-91`、`classify.ts:3-32`、`InProcessGateway.ts:1346-1383,1702-1706,1776-1778,1841-1843,2080-2086`，均为固定 cc7c4e6。
- **机器判据**：`runtime::turn::failure_guard::tests::real_ts_failure_guard_and_classify` / `real_ts_failure_guard_regenerates_identically`；`runtime::turn::submit::tests::real_ts_submit_scenarios` / `real_ts_turn_submit_regenerates_identically` 的 `guard-*` 14 场景，比较完整可见 JSON 字节流、状态记录字节、全部事件/宿主调用与重开后的 registry 持久化。夹具生成器两次一致，SHA 见本报告；负控必须检出阈值 ±1、分类错误、模型/工具/子代理漏 observe、trip 未 abort、计数残留、刷新前读配置和 wire 字段顺序变异。
- **范围**：消除本条“启用 FailureGuard 即能力拒绝”的差异；其余 submit 行为不获豁免。A7.2-pr6 仍须验生产 host 提供配置快照、真实 LiveSession 的 abort/terminal 与宿主全链路，不能据本条宣称 A7.2 或生产宿主已整体验收。
- **批准记录**：无；本条为状态更正（原能力拒绝已消除），原条目状态（已知未对齐，未批准）随之失效；不新增豁免、不改判分语义。
