# HTML → Markdown：默认 Turndown 金样

## #454 内存排队实测

`probe-memory.mts` 在 Node 22 上调用真实 `getURLMarkdownContent`，分别生成恰好
10 MiB 的 `<br>` 或 `a<br>` 正文；fetch hook 只替代 HTTP I/O，转换仍走生产
domino + Turndown。先核固定提交源码树、import 闭包和两包版本/文件哈希。
每种形状单独进程，外包 `/usr/bin/time -l timeout -k 10s 900s`；退出 124
记为 15 分钟未完成，不能推断成功。示例（在仓库根运行）：

```sh
PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck \
  /usr/bin/time -l timeout -k 10s 900s node \
  --import /Users/da/ws/pilotdeck-rs/resources/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/html-markdown/probe-memory.mts br
```

第二种形状把末参数换成 `a-br`。摘要含输入/输出 SHA256、成功或错误、转换耗时；
进程峰值 RSS 取 time 输出。不要将带耗时的摘要当作金样。输出兼容性继续复用下述
五套真实 TS 金样；排队语义由 `html_memory::tests` 的可注入小预算测试，以及
`fetcher_html_conversion_waits_for_global_memory_and_preserves_ts_output` 的生产入口
接线测试确定性验证。后者占满全局预算后，经注入 HTTP transport 调用真实
`get_url_markdown_content`：先交还一份，断言两份权重的转换已排队、尚未启动；
全部交还后逐字节比较真实 TS 的 `ul-255` 金样，并核对缓存。测试使用显式 poll 和
份额交接，不依赖墙钟；去掉 fetcher 的预算接线、改回直接 `cpu::run` 时必须失败。

真相源是固定 TS 基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的
`src/tool/builtin/web/urlFetcher.ts`。它用 `new Turndown()`，没有 GFM 或其他插件：
H1/H2 是 setext，代码块缩进四空格，表格降成块文字，script/style 标签内的文字保留，
template 内容不输出。不能按常见 Markdown 转换器的偏好改这些行为。

`cases.mjs` 只有输入；`record.mts` 通过真实 `getURLMarkdownContent` 和注入的 fetch hook
录下 108 条 Markdown、105 条解析器兼容性样例，以及真实 `createWebFetchTool` 的两次 raw 调用
（第二次缓存命中）。没有网络请求或硬编码期望值。`oracle.json` 中还绑定两包的版本与
`lib/`、`package.json` SHA256，以覆盖原有源码闭包守卫不追踪的 npm 外部导入。

`generate.mts` 是正式入口：先用共享 `bindSourceTree` 断言整个 TS 源码树与固定提交一致，
再用 `sourceClosureSha256` 绑定与 web-tools 相同的 import 闭包，最后录制。默认 locale
必须为 `en-US`。输出目录由调用者独占，输出文件用 `wx`，生成器不删除任何目录。

## 生成与回放

以下命令由有权执行 Git 溯源守卫的协调者运行。`Scratch` 会为两次生成分别建独占目录，
校验输出逐字节相同，清理时核对 owner token：

```sh
cd src
PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck \
  cargo test --locked --offline -p pilotdeck-gateway --test html_markdown \
  regenerates_twice_identically -- --ignored
```

常规回放：

```sh
cargo test --locked --offline -p pilotdeck-gateway --test html_markdown
cargo test --locked --offline -p pilotdeck-gateway --test web_tools \
  web_fetch_html_default_and_cache_match_real_ts
```

回放先调用 `baseline::oracle(ORACLE)`，精确断言 108 条，按顺序逐字节比较。另测
预算边界（含隐式 HTML/BODY/包装元素的 256 层）span 在 256 KiB 栈上的转换与析构。web_fetch 测试注入 HTTP transport，
逐字节比较请求字段、完整工具输出、UTF-8 bytes、UTF-16 length 与缓存命中结果。

## 生成证据的接续

原实现轮禁止 Git 命令，当时复用 web-tools 的 650 个源码 SHA256 录制，未执行
整个源码树的 clean-tree 守卫。修复轮 1 已允许只读 Git：两套正式生成器都完成
固定提交溯源并各跑两次，与现有金样逐字节一致；原始输出见
`_tmp/20261020-a65-pr9b/RESULT.md` 的「修复轮 1（codex）」。

## #677：解析器兼容性对齐（原 pr9c 待办）

`parserGaps` 现为 105 条真实 TS 兼容性样例，全部由 `generate.mts` 从
`getURLMarkdownContent` 录制，并由 `parser_version_gaps_match_real_ts`
逐字节回放（名称保留以兼容冻结账本指针，但已不再 `ignore`）。旧版 `accepted-diffs.md:64` 的四类中，三类已在
token 边界对齐：

| 类别 | 对齐方式 | 真实 TS 样例 |
|---|---|---|
| `<select><hr>` | `html_compat` 按 domino 旧 `in select` 模式丢弃 HR | `select-hr`、`select-hr-text`、`select-hr-option` |
| `<search>` | 改名为中性行内元素 `x-pd-search`，不再是当前规范的 p-closing 块 | `search-element`、`search-empty`、`search-nested-p` 等 |
| select 内元素家族 | 旧 `in select` 只保留 text/option/optgroup/script/template，`input`/`keygen`/`textarea` 与嵌套 select 关闭 select；表内 select 按 `in select in table` 先关闭 | 原 9 条 + script/template/textarea/input/nested/table 上下文 |

改写只发生在 `<select` / `<search` 存在的输入；其余字节原样交给 html5ever。
不含这两个前缀的文档走借用快路径。宽子树 4 / 8 MiB 的 domino `Invalid array
length` 是 V8 数组长度运行时差异，不能由 token 改写复刻；仍按未批准差异登记
`specs/gateway-parity/diffs/A6.5-WEBFETCH-WIDE-RUNTIME-001.md`。

修复轮 r1（#677）把 token 边界和帧栈对齐到 tokenizer：引号只在属性值首字节
进入引号状态（未加引号值里的 `"`/`'` 是普通字节），bogus comment / CDATA 在
第一个 `>` 结束，注释同时接受 `-->` 与 `--!>`；弹帧前先确认目标帧存在，
未匹配的 `</template>` 不再弹掉外层 select。新增真实 TS 样例覆盖这两个反例及
其正负控（`select-unquoted-*`、`bogus-comment-*`、`cdata-double-quote`、
`comment-bang-close`、`select-unmatched-template-end*`、`select-template-*`）。


## 修复轮 1：资源预算与性能回归

`html_dom.rs` 在每 1024 source bytes 后检查深度：最多 256 层元素祖先
（包括 HTML/BODY/Turndown 包装元素）。重挂子树和 template 内容同步更新深度。
修复轮 2 已删除没有 TS 依据的节点数量与累计分配预算；宽度不触发深度错误。
超出深度预算返回真实 TS 栈溢出的 message，经 web_fetch
映射为 `tool_execution_failed`、`web_fetch failed: Maximum call stack size exceeded`、
`details: { stage: "http_fetch" }`。

当前只有**深度 256 一项向严差异，已人批**（2026-10-21 15:25，登记为 `specs/gateway-parity/accepted-diffs.md` A6.5-WEBFETCH-DEPTH-001）：TS 在 257 层仍成功。
真实 TS web_fetch（Node 22 默认栈）最低所测冷断点为 1051 个嵌套 PRE，
含隐式包裹约 1054 层；table 的 264 组断点约 1059 层，不能当作 264 层。
256 是保守的资源保护选择，绝非 TS 的准确断点。
完整断点、区间与取消探针原值追加在 `_tmp/20261020-a65-pr9b/RESULT.md`。
`depth-oracle.json` 记录七种结构的 255/256/257 层真实 TS 成功输出、
七种实际溢出时的完整工具错误，以及按评审列举的覆盖面另建的 40 条形状。
未取得 Grok 原 40 条输入，不把这 40 条冒充为它们的原样回放。

`generate-depth.mts` 同样先跑固定提交的只读 Git 溯源守卫，再走真实 TS 入口。
`regenerates_twice_identically` 分别运行五套生成器两次，逐字节匹配已提交金样。

性能回归在深度预算内构造 126 层 ul/li、126 层长序号 ol/li（各 9 万 BR），
再转换 10 MiB 平坦 HTML；release 的总预算 1 秒，debug 放宽为 20 秒。
列表缩进以 arena rope 延迟到最终输出，避免祖先反复扫描扩张后的字符串。
转换经 `spawn_blocking`；current-thread Tokio 回归验证转换期间仍能调度定时器，
父取消保持 TS raw 成功顺序，外层 timeout 能放弃等待。正在执行的 blocking
闭包不能被强杀，工作量由输入上限、深度保护与迭代转换实现约束；TS 的 60 秒 fetch 定时器
在响应头到达后已清除，不额外发明转换阶段的 fetch timeout。


## 修复轮 2：大页宽度回归

`generate-large.mts` 通过固定提交的只读 Git 溯源守卫，并核对原金样中 Turndown /
domino 的全部依赖哈希，再走真实 `getURLMarkdownContent`（fetch hook，无网络）。
`large-oracle.json` 只存 HTML 生成规则、真实 TS Markdown 的无损重复编码、字节数与
SHA256。编码器会展开输出并与真实 TS 原始字节比较；Rust 回放也先复核长度与哈希，
再逐字节比较完整 Markdown，没有截断到 100 KB。生成器耗时不写入金样，便于两次复录比对。

| 样本 | 生成规则 | TS 对拍范围 |
|---|---|---|
| 四列三万行表 | `<table>` + 30,000 × (`<tr>` + 4 × `<td>cell</td>` + `</tr>`) + `</table>` | 完整输入与完整输出 |
| 十万个段落 | 100,000 × `<p>paragraph</p>` | 完整输入与完整输出 |
| 10 MiB 稠密 inline | 1,023 × (`<p>` + 1,024 × `<em>x</em>` + `</p>`) + 3,079 个 `x` | 完整 10,485,760 bytes 与完整输出 |
| 单段平铺 inline | 50,000 × `<em>x</em>` | 完整输入与完整输出 |

`large_shallow_pages_match_real_ts_bytes_and_stay_fast` 覆盖以上四例，`ten_mib_of_flat_inline_siblings_stays_fast`
另把最后一例已经过 TS 逐字节验证的周期扩展到 1,048,576 个 inline 元素（恰好 10 MiB），
`reparented_wide_subtrees_stay_fast` 覆盖宽子树重挂 8 MiB；这条是 Rust 宽度压力回归，
**不声称完整平铺 10 MiB 已跑过 TS**。三个计时测试共用 `tests/common/timing.rs`
（与 `html_compat` 的 deep-frame 单测同一份）：把每例的主重复减半构造半幅输入，
半幅展开后先断言字节数在全幅的 40%–60% 之间（否则「输入翻倍」前提不成立，
比例判据退化为宽松的常数检查），再交错采样取各自最快值，断言
full ≤ 3 × half + 150 ms（二次复杂度需要约 4x）。绝对上限是**每个全幅样本**的
预算（`assert_within_per_sample_cap`：采样次数增加不分割预算，判据取最快样本，
单次被抢占既不影响比例也不影响兜底）：release 5 秒原值；debug 120 秒——
CI 慢跑机实测单样本全幅 31.5–34 秒（46033287、193e8a1e，比开发机慢约 4×），
120 秒对其留约 3.75× 余量；4fbb5f78b 之前的整子树重扫线性但慢约 36 倍，只有
该上限能抓住（release 负控实测 50.4 s > 5 s），debug 下慢机 36× 退化同样远超
120 秒。采样次数由 `PDRS_TIMING_SAMPLES` 控制（默认 3，有效 1..=9）；
`tests/fixtures/html-timing/negative_controls.py` 的负控将其降为 1——红跑证明的是
「判据能抓到变异体」（变异体每个样本都同样慢，采样更多只会更红；每样本兜底
不被采样稀释），不是复刻 CI 的 3 样本形状。
输入与期望构造不计入转换计时。错误累计后统一失败，恢复旧节点上限时四例都必须报错，
不能只靠第一例失败冒充全部负控。
为避免并行压力影响有界耗时判据，性能自证用 `--test-threads=1`。

资源保护保留 fetcher 的 10 MiB 输入上限、深度 256、线性 arena / rope 遍历与
`spawn_blocking`，没有新增节点数量门槛或转换期 timeout。
实测、恢复节点上限的负控和完整 workspace 自证见
`_tmp/20261020-a65-pr9b/RESULT.md` 的「修复轮 2（codex）」。

## 修复轮 3：嵌套内联替换的驻留内存

`generate-depth.mts` 的 `extra` 末尾追加 `nested-code-flanks-250` 与
`nested-span-flanks-250`，左右文字保留首尾空白；两条都通过真实 TS 入口生成，
由 `nested_code_and_flanks_match_real_ts_bytes` 逐字节回放。原 40 条保持不变。
code 必须逐层重新选择不与内容冲突的反引号，flank 必须先 trim；本轮保留这些
变换，只在整串替换后迭代释放旧 Rope 子树。资源单测统计所有 Rope 字符串的
峰值 capacity（含已不可达的旧节点），断言 250 层不超过单层三倍。
这不是整次转换的 CPU 或输出大小上限；blockquote 的输出放大仍属 #409，
已启动的 `spawn_blocking` worker 也不会因宿主 future 丢弃而停止。

## 修复轮 4：V8 字符串上限

`generate-string-limit.mts` 走真实 `getURLMarkdownContent` 和 `createWebFetchTool`
（只注入 HTTP 字节，不替换 turndown），录制 n=16 的完整 655349 字节 Markdown
及 n=33 的 RangeError、完整工具错误和两次请求的缓存行为。
`string-limit-oracle.json` 可用同一生成器重放，加入了既有双次逐字节再生成测试。

Rust 按 Node 22 的 536870888 个 UTF-16 单元上限，在构建每个 replacement、
join、indent、repeat、flatten 以及转义/链接/code 字符串前验证预算。
rope 保存 UTF-8 bytes、UTF-16 units、换行数，标题下划线保留为惰性 repeat，
避免在发现超限前物化大串。检查包括最终 join 会裁去的 replacement 边界换行，
与 Turndown 先生成 replacement 的顺序一致。这项为 TS 上限平价，不新增向严差异。
资源测量、内存受限负控和完整自证见工作区 `_tmp/20261021-a65-pr9b/RESULT.md` 修复轮 4。


## 修复轮 5：字符串上限交叉点

`generate-string-limit.mts` 用真实 `getURLMarkdownContent` 从 n=1 逐个搜索
`'<h1><div>'.repeat(n) + 'x' + '</div></h1>'.repeat(n)` 的首次失败，
Node 22.22.0 / V8 12.4.254.21-node.33 实测 n=25 成功、n=26 首次抛出
`RangeError: Invalid string length`。`string-limit-oracle.json` 保存搜索结果、
n=25 / 26 两侧样本及真实工具的错误 / 缓存行为，原 n=16 / 33 保留。
n=25 的成功输出只存 UTF-16 长度 335544309 与完整 UTF-8 字节的 SHA256，
不保存大串正文；Rust 回放同样断言完整长度与 SHA256，n=25 成功、n=26 报同一错误，
未发现这两个交叉点样本上的向严或向宽偏差。两次独立生成需与金样逐字节一致。
交叉点两侧回放结果与原始日志见工作区 `_tmp/20261021-a65-pr9b/RESULT.md`「修复轮 5」。


## 修复轮 9：select 家族披露与宽子树金样

上文「2 条解析器版本缺口」与「parserGaps 两条」为原始样例计数；当时（修复轮 9）为
**3 类已发现的解析器差异、11 个 parserGaps 样例**（原 2 个 + 本轮 9 个），
另有 1 类宽子树运行时差异，已知未对齐表共 4 条，全部未批准、非已接受差异。
select 家族未穷举：实测 div / p / li / table / h1 / ul / img / br / option>div
与 TS 不同，真实 TS 原值由 `generate.mts` 录入 `oracle.json`；
span 和纯文本两例一致，原值仅记工作区 RESULT「修复轮 9」。
当时的 `parser_version_gaps_match_real_ts` 仍 ignore；issue #677 已把三类解析器
差异对齐，同名测试已启用（见上文
「#677：解析器兼容性对齐」）。

`generate-large.mts` 追加 `reparent-wide-2mib`：240 个属性各异的 `<b a=i>`、
`<p>`、重复 `<span>x</span>`、补齐 x、240 个 `</b>`，精确 2 MiB。
`large-oracle.json` 保存真实 TS 完整返回值的无损重复配方、字节数与 SHA256；
生成器展开配方逐字节核对真实输出，Rust 的
`large_shallow_pages_match_real_ts_bytes_and_stay_fast` 比较完整 Markdown 字节。
原 4 个大页样例保持原值；8 MiB 性能测试保留，新增
`reparented_wide_four_mib_succeeds` 仅断言 Rust 转换成功。
4 / 8 MiB 的 TS `Invalid array length` 仍是未批准的向宽差异，
不新增豁免，也不改变深度批准或判分语义。

## pr9c：截断投影与 #409 / #454 内存实测

`generate-truncate.mts` 走真实 `getURLMarkdownContent`、真实 `createWebFetchTool`
（只注入 HTTP 字节，不替换 turndown）和两次 raw 调用，录制 18 条输入：
99999 / 100000 / 100001 单元边界、CJK 与代理对切点、悬空 Markdown 转义、
250 层 blockquote 的行前缀切点、CJK h1 下划线、宽段落页、尾部空白 trim、
trim 后 `rawLength=0` 的全空白页、NBSP-only blockquote（真实 TS `">"`），
以及 `<code>` / `<span> <code>` 包住 16 层嵌套标题（真实 TS 655,357 单元）、
被 flank trim 成前缀的 `<span>` 尾部的反引号（真实 TS 655,358 单元，决定
`<code>` 的两反引号定界符），以及同一形状后跟兄弟反引号的合并边界
（真实 TS 655,360 单元，决定 padding 是否可见）。
另有 `code-closing-wrapper-window`：内容 99,994 单元、定界符 5 反引号 + pad
（wrapper=6），逻辑长度 100,006；物化前缀必须包含闭合 wrapper，否则
100,001 的 take 会短 1 个单元（真实 TS 100,006，带截断后缀 100,038）。
输入用重复配方保存，展开后先核对 `htmlSha256`；期望值全部是真实 TS 输出，
只存 UTF-16 长度和 SHA256（代理对切断后是孤立代理，不能按 UTF-8 哈希）。
每条还存完整 trimmed 文本前 100,001 单元的 `prefix.utf16Length` / `sha256`；
Rust 的 bounded 前缀按此回放（切在代理对中间时允许 1 个单元的额外低代理）。
`windows` 另录 9 个短输入、22 个任意 k 切点（Grok r2 的 3 个输入 / 6 个 k：
39、40、80–82、10；gate2 的闭合 wrapper 小窗口 k=24；`<pre>` 内 `&#13;` +
flank-trim partial 的 k=10；GLM r3 P1 的 CRLF / 单 CR / 单 LF 混排 3 个输入 /
10 个 k；GLM r4 P1 的 CRLF 接缝窗口 k=19、20、22、28），期望值是真实 TS
`content.slice(0, k)`。生成器实际运行两次，输出：

```text
ae9dbb6285a0e9b782b774f49d106444cbebf77738a067993632b40cb3b47e42  truncate-oracle.json
ae9dbb6285a0e9b782b774f49d106444cbebf77738a067993632b40cb3b47e42  truncate-oracle.json  (second run)
```

Rust 的 `Turndown::convert_bounded(html, MAX_MARKDOWN_LENGTH + 1)` 只把
`min(trimmed, 100001)` 个单元落到 `String`；完整 trimmed UTF-16 / UTF-8 长度由
rope 的 `Size` 组合算出，尾部空白由 `measure` 的 O(1) 前缀栈算术得出。行内
替换（`<code>` 与 flank trim）也只物化到窗口为止，不会先 flatten 整个子树；
flank trim 按行内 code 的折叠单位计数，CR LF 对折成一个单元，所以它物化到折叠后
至少 `prefix_units + 4` 个单元（原始单元数最多翻倍）；`<code>` 的 inner 不足窗口
时连闭合定界符/pad 一起物化，保证物化量至少是 `min(逻辑长度, prefix_units + 4)`
（否则闭合 wrapper 会让 take 短几个单元）。`FetchedEntry::raw_markdown_length` 保留完整长度，
`rawLength`、`truncated`、100 KB 后缀和 50 MiB 缓存预算仍按完整 Markdown
计算。边界回放 `bounded_prefix_and_truncation_match_real_ts` 对 18 条逐条比较
完整长度、前缀 SHA256、`truncated`、`returnedLength`、UTF-16 SHA256 和
第二次缓存命中；`bounded_code_windows_match_real_ts` 回放 22 个任意 k 切点。
`web_tools.rs` 另有两次端到端 `WebFetchTool` 调用（缓存
命中、rawLength/returnedLength）和 335 M 单元超预算不缓存的用例。

release 实测（`tests/html_budget_probe.rs` + `rss-probe.sh`，`/usr/bin/time -l`）：

| 输入 | 路径 | 峰值 RSS | 耗时 | 输出 |
|---|---|---|---|---|
| 250 层 blockquote + 2.5 MiB `a<br>` | 全量 `convert` | 1,234,714,624 B | 0.92 s | 264.2 M 单元 |
| 同上 | `convert_bounded` | 1,008,091,136 B | 0.44 s | 前缀 100,001 单元 |
| 250 层 blockquote + 10 MiB `a<br>` | `convert_bounded` | 3,456,286,720 B | 1.56 s | 推断 V8 `Invalid string length`（未对 10 MiB 输入单独跑 TS） |
| 10 MiB `<br>`（宽页） | `convert_bounded` | 1,922,695,168 B | 1.57 s | trim 后 0 |
| 10 MiB `a<br>`（平铺） | `convert_bounded` | 3,996,598,272 B | 1.98 s | 8.4 M 单元 |

RSS 数字由 `rss-probe.sh` 单进程测量；脚本先取探针子进程状态再 grep 证据行，
探针 panic 时打印完整输出并以 1 退出，不会把崩溃进程的 `real`/RSS 行当成通过证据。
`html_budget_probe.rs` 的
`bounded_peak_rss_is_lower_than_full` 和 2.5 MiB 深层 prefix 用例标了
`#[ignore]`（进程级 RSS 在并行测试下不稳定，后者 debug 约 1 GB），需要时用
`cargo test --release -p pilotdeck-gateway --test html_budget_probe -- --ignored`
或 `rss-probe.sh` 手动跑。

因此提前停止只解决“输出/UTF-16 副本”放大：2.5 MiB 深嵌套少约 227 MB、
耗时减半，`flatten` 容量由 264 MB 降到 400 KB（单测断言）。
10 MiB 宽页/平铺页的 RSS 仍由 DOM + rope 每节点成本决定（约 2–4 GB），
不是截断投影能覆盖的。评估候选节点/内存预算见
`_tmp/20261022-a65-pr9c/RESULT.md`「#454：实测、评估与候选」；本轮未改
`specs/`、`accepted-diffs.md` 或解析器语义，也未新增节点门槛。
