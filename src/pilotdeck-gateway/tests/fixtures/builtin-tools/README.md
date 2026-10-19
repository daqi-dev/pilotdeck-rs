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
正本。场景 50 个、调用 55 次。生成器断言默认 locale 为 `en-US`（与 `skills` 夹具
相同）。setup 使用显式 mtime（`utimes`），Rust 回放用 `File::set_modified` 设同一
值，所以 `glob --sort=modified` 的顺序不依赖文件创建时刻。

## 范围切分

本 PR 只做 `read_file`、`glob`、`get_current_time`；`grep` 留 A6.5-pr4b。任务书
允许按估算切分，grep.ts（454 行）的上下文/多行/二进制匹配需要单独的对拍预算。

## 已知边界

- **模型支持图片/PDF 时的附件路径**：TS 用 `sharp`/`mupdf` 解码并压缩成
  model-visible content；本运行时没有这两个编码器，也不允许新增 crate。没有
  multimodal 声明时 TS 本身返回文本提示，夹具覆盖该分支。声明支持时本实现返回
  同形状的 decode-failure 提示（图片）或 `result_too_large`（PDF），**不声称**与
  TS 成功附件分支对拍。
- **PDF 页数**：夹具用非 PDF 字节的 `.pdf`，TS 的 `countPdfPages` 返回
  `undefined`，因此两边都不带 `, N pages` 后缀。真实 PDF 页数统计未移植。
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
