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
10,000 层 span 在 256 KiB 栈上的转换与析构。web_fetch 测试注入 HTTP transport，
逐字节比较请求字段、完整工具输出、UTF-8 bytes、UTF-16 length 与缓存命中结果。

## 本轮证据边界

2026-10-21 接续任务禁止任何 Git 命令。因此正式生成器的 clean-tree 守卫本轮**未执行**。
现有 oracle 来自两次独立进程的真实 TS 录制；每次先核对 web-tools 已有 oracle 中
650 个源码 SHA256，输出逐字节一致。此措施复用了既有锚点，不能替代一次新的整个源码树
干净断言。协调者提交前须跑上面的正式生成测试。原始证据和源码变异负控见
`_tmp/20261020-a65-pr9b/RESULT.md`。

## pr9c 待办：解析器版本差异

`parserGaps` 两条保留真实 TS 输出；对应 Rust 测试明确 `ignore`，不算通过或已接受差异：

| 输入 | domino 2.2.0 的 Markdown | 原因 |
|---|---|---|
| `<select><hr></select>` | 空串 | 新 HTML5 规则允许 SELECT 内 HR，旧 parser 忽略它 |
| `<p>a<search>b</search>c</p>` | `abc` | 新 HTML5 规则把 SEARCH 当块级树构建元素，改变 P 边界 |

不能仅靠输出后处理普遍修复树构建差异。后续需在 token/tree-building 边界有针对性对齐，
补正负样本，再启用此测试；在此之前不宣称 HTML 解析已与 domino 完全等价。
