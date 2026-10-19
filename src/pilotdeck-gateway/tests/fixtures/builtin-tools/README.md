# 内置只读文件工具夹具（read_file / glob / get_current_time）

`oracle.json` 来自固定 TS 基线（`cc7c4e6`）的真实运行：用真实
`createReadFileTool` / `createGlobTool` / `createGetCurrentTimeTool` 在独占临时目录中
跑同一组 setup 与调用，逐条记录 `validateInput` 与 `execute` 的返回值（错误只记
code/message，细节由 A6.5-pr3 的夹具覆盖）。`sourceSha256` 覆盖三个入口的相对
import 闭包；生成器用 `bindSourceTree` 校验基线干净树与 Git blob。

```sh
PDRS_BASELINE=/abs/fixed/PilotDeck LANG=en_US.UTF-8 \
  node --import /abs/fixed/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/builtin-tools/generate.mts /abs/output-parent
```

每次运行在输出父目录下 `mkdtemp`，`flag:'wx'` 写入；连续两次输出字节一致才更新
正本。场景 55 个、调用 63 次。生成器断言默认 locale 为 `en-US`（与 `skills` 夹具
相同）。setup 使用显式 mtime（`utimes`），Rust 回放用 `File::set_modified` 设同一
值，所以 `glob --sort=modified` 的顺序不依赖文件创建时刻。带 `modelMultimodal` 的
调用把该 context 片段记入 `calls[].context`，回放时注入同一字段。

## 范围切分

本 PR 只做 `read_file`、`glob`、`get_current_time`；`grep` 留 A6.5-pr4b。任务书
允许按估算切分，grep.ts（454 行）的上下文/多行/二进制匹配需要单独的对拍预算。

## 已知边界

- **模型支持图片/PDF 时的附件路径**：TS 用 `sharp`/`mupdf` 解码并压缩成
  model-visible content；本运行时没有这两个编码器，也不允许新增 crate。没有
  multimodal 声明时 TS 本身返回文本提示，夹具覆盖该分支。声明支持时：
  - 完整 JPEG（EOI 且 >1 KiB、未超字节预算）的 fast path 不需要 sharp，已按 TS
    原样实现并用真实 TS 夹具覆盖；
  - 非 JPEG 解码、JPEG 修复/压缩仍需要 sharp，返回同形状的 decode-failure 文本；
  - PDF 原始字节的 document block 不需要解码器，已实现并覆盖；PDF 页数统计与
    `pages` 的 mupdf 渲染仍不可用（没有 mupdf，也没有新增 PDF 解析 crate）。
- **PDF 页数**：夹具用非 PDF 字节的 `.pdf`，TS 的 `countPdfPages` 返回
  `undefined`，因此两边都不带 `, N pages` 后缀。本实现始终不统计页数，所以真实
  PDF 的页数后缀与 `>10 pages` 拒绝分支仍是已知偏离。
- **无 limit 的尾随孤立 CR**：TS `split(/\r?\n/)` 只在 `\r` 后跟 `\n` 时剥除；
  `read-text-eof-lone-cr` 钉住 `abc\r` 保留 CR 的行为。有 limit 的 readline 仍把
  孤立 CR 当行终止符（`read-text-lone-cr-with-limit`）。
- **notebook 畸形输入**：`cell_type` 的 `String(value)` 转换与 `cells` 为非数组对象时
  的 `cellCount` 省略已有夹具；`cells` 为字符串/类数组对象时的 `length` 索引语义、
  `outputs` 非数组的 TypeError 文案与 `.ipynb` 目录名仍留作跟进。
- **时区数据**：TS 用 ICU；Rust 用系统 TZif（经 `pilotdeck-transport` 的既有
  `tz-rs` 依赖），只支持系统 zoneinfo 中存在的 IANA 名与 `±HH[:]MM` offset。
  `Intl` 接受但系统缺失的 ICU 专名会报 invalid timezone。默认时区从 `TZ` 或
  `/etc/localtime` 推导，夹具不测环境默认值。
- **glob 精确同 mtime 的次序**：ripgrep `--sort=modified` 对完全相同 mtime 的
  文件没有路径次序保证；本实现用路径字节序兜底，保证输出稳定。夹具用不同 mtime
  避开该差异。
- **read_file 大文件自动分页**：`isOverTextBudget` 复用 `pilotdeck-tokenizer`
  的 o200k_base 计数，与 TS 的 `countTokens` 同源；80 KB 以下/200 KB 以上不数
  token。
