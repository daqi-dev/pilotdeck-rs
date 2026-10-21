# HTML → Markdown：默认 Turndown 金样

真相源是固定 TS 基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的
`src/tool/builtin/web/urlFetcher.ts`。它用 `new Turndown()`，没有 GFM 或其他插件：
H1/H2 是 setext，代码块缩进四空格，表格降成块文字，script/style 标签内的文字保留，
template 内容不输出。不能按常见 Markdown 转换器的偏好改这些行为。

`cases.mjs` 只有输入；`record.mts` 通过真实 `getURLMarkdownContent` 和注入的 fetch hook
录下 108 条 Markdown、2 条解析器版本缺口，以及真实 `createWebFetchTool` 的两次 raw 调用
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

## pr9c 待办：解析器版本差异

`parserGaps` 两条保留真实 TS 输出；对应 Rust 测试明确 `ignore`，不算通过或已接受差异：

| 输入 | domino 2.2.0 的 Markdown | 原因 |
|---|---|---|
| `<select><hr></select>` | 空串 | 新 HTML5 规则允许 SELECT 内 HR，旧 parser 忽略它 |
| `<p>a<search>b</search>c</p>` | `abc` | 新 HTML5 规则把 SEARCH 当块级树构建元素，改变 P 边界 |

不能仅靠输出后处理普遍修复树构建差异。后续需在 token/tree-building 边界有针对性对齐，
补正负样本，再启用此测试；在此之前不宣称 HTML 解析已与 domino 完全等价。


## 修复轮 1：资源预算与性能回归

`html_dom.rs` 在每 1024 source bytes 后检查预算：最多 256 层元素祖先
（包括 HTML/BODY/Turndown 包装元素）、100,000 个累计分配节点（包括文档、
隐式元素、文本、注释、脱离节点与 template fragment）。重挂子树和 template
内容同步更新深度。超出任一预算返回真实 TS 栈溢出的 message，经 web_fetch
映射为 `tool_execution_failed`、`web_fetch failed: Maximum call stack size exceeded`、
`details: { stage: "http_fetch" }`。

此为**向严差异，待批**，不是已批准差异：TS 在部分超出预算的输入仍成功。
完整断点、区间与取消探针原值追加在 `_tmp/20261020-a65-pr9b/RESULT.md`。
`depth-oracle.json` 记录七种结构的 255/256/257 层真实 TS 成功输出、
七种实际溢出时的完整工具错误，以及按评审列举的覆盖面另建的 40 条形状。
未取得 Grok 原 40 条输入，不把这 40 条冒充为它们的原样回放。

`generate-depth.mts` 同样先跑固定提交的只读 Git 溯源守卫，再走真实 TS 入口。
`regenerates_twice_identically` 现在分别运行两套生成器两次，逐字节匹配已提交金样。

性能回归在深度预算内构造 126 层 ul/li、126 层长序号 ol/li（各 9 万 BR），
再转换 10 MiB 平坦 HTML；release 的总预算 1 秒，debug 放宽为 20 秒。
列表缩进以 arena rope 延迟到最终输出，避免祖先反复扫描扩张后的字符串。
转换经 `spawn_blocking`；current-thread Tokio 回归验证转换期间仍能调度定时器，
父取消保持 TS raw 成功顺序，外层 timeout 能放弃等待。正在执行的 blocking
闭包不能被强杀，结束时间由默认转换器预算控制；TS 的 60 秒 fetch 定时器
在响应头到达后已清除，不额外发明转换阶段的 fetch timeout。
