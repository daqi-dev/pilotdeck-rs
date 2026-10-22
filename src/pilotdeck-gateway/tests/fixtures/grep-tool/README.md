# 内置 `grep` 工具夹具（A6.5-pr4b）

`oracle.json` 来自固定 TS 基线（`cc7c4e6`）的真实运行：用真实
`createGrepTool()` 在独占临时项目目录中跑同一组 setup 与调用，逐条记录
`execute` 的 `content/data/metadata`（错误只记 `code`/`message`）。

```sh
PDRS_BASELINE=/abs/fixed/PilotDeck LANG=en_US.UTF-8 \
  node --import /abs/fixed/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/grep-tool/generate.mts /abs/output-parent
```

每次运行在输出父目录下 `mkdtemp`，`flag:'wx'` 写入；连续两次输出字节一致才更新
正本。场景 51 个、调用 51 次。`sourceSha256` 由 `sourceClosureSha256` 覆盖
`src/tool/builtin/grep.ts` 的相对 import 闭包（含 `filesystem/ripgrep.ts`、
`pathSafety.ts`、`NodeFsPort`、`SubprocessPort`、`bash/commandRunner` 等 650 个
文件），`generatedFrom` 绑定基线工作树。

## ripgrep 依赖与定位

TS 的 `grep` 不自己匹配内容：`filesystem/ripgrep.ts:89` 用
`require("@vscode/ripgrep").rgPath` 解析 `@vscode/ripgrep-<platform>-<arch>/bin/rg`，
`ripgrep.ts:27` 用该绝对路径 `executeFile`；解析失败或 spawn ENOENT 时抛
`unsupported_tool`（`ripgrep.ts:106`），消息为
`grep requires the bundled ripgrep binary from @vscode/ripgrep, but it is not available for <platform>-<arch>. …`。
本机基线根里该可选依赖不在顶层 `node_modules/@vscode/`，但可由 pnpm 布局解析到
`node_modules/.pnpm/@vscode+ripgrep-darwin-arm64@1.18.0/…/bin/rg`（ripgrep 15.0.0）。

Rust 侧同样不自带匹配引擎，行为按 TS 拆分：`grep.rs` 生成同一参数向量并执行
ripgrep，再复刻 TS 的解析、忽略段过滤、排序、分页与错误映射。二进制定位顺序：

1. `PILOTDECK_RIPGREP_PATH`（运维/测试固定引擎，类比 `bash` 的
   `PILOTDECK_SHELL_PATH`）；
2. `PATH` 上第一个可执行的 `rg`（Windows 为 `rg.exe`）；
3. 都没有 → 与 TS 同文案的 `unsupported_tool`，不伪造结果。

本夹具用基线自带 ripgrep 15.0.0 录制；同一组场景在系统 ripgrep 15.2.0 下回放也
逐字节一致。没有 `rg` 的机器上 `grep_outputs_match_real_ts` 会打印跳过原因；
`PDRS_REQUIRE_RG=1` 时改为测试失败，避免对拍假绿。

## 场景

- content：单文件匹配、`-n:false`、`-C`/`context`/`-B`/`-A` 与优先级、`--`
  分隔、正则、`-i`、多行、超长行省略、`-` 开头 pattern、Unicode、文件目标路径
  重定基、offset/head_limit（含 0、负数、越界）、无匹配、二进制目标报错。
- files_with_matches（默认）：mtime 倒序、glob（`*.ts`、`*.{ts,js}`、逗号、
  空白分隔、无匹配）、`type`（ts/js/py）、隐藏文件、ignored 目录、目录/尾斜杠
  路径、二进制目标、head_limit/offset。
- count：全树计数、glob 过滤、head_limit、二进制、无匹配。
- 路径与错误：不存在路径、工作区外绝对路径、工作区内绝对路径、非法正则。

## 已知边界

- **content 多文件顺序**：TS 不排序，直接沿用 ripgrep 多线程输出顺序；实测同一
  棵树连续 10 次出现 4 种不同顺序。因此所有 content 场景都指向单个文件，避免
  金样随 ripgrep 调度漂移。files/count 两种模式 TS 自己排序，可安全覆盖多文件。
- **`isIgnoredPath` 过滤是防御分支**：ripgrep 的 `--glob '!dist'`/`!node_modules`
  会先剪枝，正常输出不会含这些段；夹具无法从真实 TS 触发该分支，改由
  `grep.rs` 单测 `workspace_paths_rebase_and_ignore_baseline_segments` 钉住。
- **超时/abort**：TS 固定 20 s 超时与 `tool_aborted` 分支无法在夹具里稳定制造，
  未纳入 oracle；Rust 按 `ripgrep.ts` 的 code/文案实现，未做真实计时对拍。
- **环境变量**：TS 把 `context.env` 整体作为子进程 env（Node 是替换而非合并），
  Rust 在 host 提供 `env` 时同样 `env_clear` 后逐项设置；夹具因此只注入
  `HOME`/`PILOT_HOME`。
- **rg 版本**：参数与输出格式以 15.0/15.2 实测为准；更老或未来版本若有格式变化，
  需要重新录制 oracle。
