# bash 工具夹具（A6.5-pr7）

`oracle.json` 来自固定 TS 基线（`cc7c4e6`）的真实运行：真实 `createBashTool`、
`classifyBashPermission` / `isReadOnlyShellCommand`、`PermissionRuntime` 与真实
`/bin/bash` 进程（macOS）。`sourceSha256` 覆盖 `bash.ts`、`bash/commandRunner.ts`、
`bash/permissions.ts`、`PermissionRuntime.ts`、`protocol/result.ts` 的相对 import 闭包；
生成器用 `bindSourceTree` 校验基线干净树与 Git blob，并断言 locale 为 `en-US`。

```sh
PDRS_BASELINE=/abs/fixed/PilotDeck LANG=en_US.UTF-8 \
  node --import /abs/fixed/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/bash-tool/generate.mts /abs/output-parent
```

每次运行在输出父目录下 `mkdtemp`，`flag:'wx'` 写入；临时进程目录带归属令牌，只删本次
自建的树。连续两次逐字节一致才更新正本（提交时
`sha256(oracle.json) = 2027d3ce61c813e6ef819fc354adb104b0e455a958f45e95818f7714a3823d9d`，
两次运行均为此值）。分节条数：commands 199 · predicates 10 · decisions 24 ·
scripted 118 · real 34。输入清单在 `cases.mjs`。

## 分节

- **commands**：只读判定与权限分类，含 JS 正则语义边界（`\s` 含 BOM 不含 NEL、ASCII `\b`、
  `/i` 不折叠 U+017F/U+212A、`rm` 前瞻在换行后的位置）。
- **predicates**：非字符串 / 缺失 `command` 时的 `isReadOnly` / `isConcurrencySafe` / `checkPermissions`。
- **decisions**：真实 bash 工具进 `PermissionRuntime.decide`（default/plan/bypass、allow/deny/ask/session 规则、acceptEdits）。
- **scripted**：注入 runner 的固定结果，覆盖成功文本、失败诊断（traceback / 缺模块 / command not found /
  SyntaxError / 600 字符尾截断与代理对切分）、超时参数（默认、上限、0、负数、小数、字符串、NaN）、
  后台命令拦截 82 条、非字符串命令的 TypeError、progress 事件与 runner 收到的参数。
- **real**：真实进程：echo、多行、stderr、非零退出、信号退出、UTF-8 解码（BOM、非法字节、跨块）、
  300 000 字节输出（只比哈希 + 经调度器的 `previewLimit`）、超时（含孙进程、忽略 TERM 走 3 s SIGKILL）、
  取消、预先取消、cwd 不存在 / 是文件、env、`PILOTDECK_SHELL_PATH`、stdin 关闭、NUL 命令、
  逃出进程组的作业触发 15 s 强制返回。

## 归一化与为什么

- **`durationMs`**：真实墙钟，跨运行不稳定，结果里记为 `<duration>`；时间行为改用
  `minWallMs` / `maxWallMs` 区间断言（如超时 300 ms 在 [300, 3000] ms 内返回，忽略 TERM
  在 ≥3.2 s 返回，强制返回在 [15, 25] s）。
- **临时路径**：每场景根目录记为 `<root>`；输出里的绝对路径命令先在 shell 内转成与路径无关的文本，
  避免字节数随目录名变化。
- **超大输出**：只记录归一后结果的 sha256、`stdout.length` 与 `applyResultSizeLimit` 元数据，避免 300 KB 入库。

## 已知边界

- **平台**：只在 macOS 录制与回放（`#![cfg(unix)]`）；Windows 的 cmd/pwsh/Git Bash 选择与 GB18030 解码未移植。
- **会话**：Node `detached` 调 `setsid()`；本 crate 禁 `unsafe`，子进程用 `process_group(0)` 建新进程组但仍在父会话。
  对外可见的终止语义（信号发给整组）一致；夹具里 `ps -o sess` 之类不覆盖。
- **3 s SIGKILL**：TS 在终止后 3 s 无条件再发 SIGKILL（即使已返回）；Rust 不能留分离任务，已终止且在 3 s 内
  结束的运行改为在收尾时立即补发 SIGKILL（组长此时尚未回收，PGID 不会被复用）。
