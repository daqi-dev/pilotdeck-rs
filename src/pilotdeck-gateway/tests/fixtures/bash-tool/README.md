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
`sha256(oracle.json) = 78e44abab43d5e7cc03c7acb9b0ddc12e3959232811a13b716908d4b5893554e`，
两次运行均为此值）。分节条数：commands 199 · predicates 10 · decisions 24 ·
scripted 118 · real 49。输入清单在 `cases.mjs`。

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
  取消、预先取消、cwd 不存在 / 是文件、env（含非字符串值）、`PILOTDECK_SHELL_PATH`、stdin 关闭、
  NUL 字节在 argv / cwd / env 键与值 / shell 路径各入口（含孤立代理旁的 NUL：`\udXXX` 转义、
  代理对保持原样、env 键保留原始 UTF-16；文案走 `node_io::invalid_arg_value_message` 正本）（Node 校验顺序：args → cwd → env，env 按
  `Object.keys` 序）、`util.inspect` 76/77 单位换行阈值、返回后 3 s SIGKILL 仍按时发出（`deferred-kill`：
  忽略 TERM 且不占管道的后代在 1.5 s 写 early.txt、4.5 s 写 late.txt，6.5 s 检查为 `[true,false]`）、
  逃出进程组的作业触发 15 s 强制返回。

## 归一化与为什么

- **`durationMs`**：真实墙钟，跨运行不稳定，结果里记为 `<duration>`；时间行为改用
  `minWallMs` / `maxWallMs` 区间断言（如超时 300 ms 在 [300, 3000] ms 内返回，忽略 TERM
  在 ≥3.2 s 返回，强制返回在 [15, 25] s）。
- **临时路径**：每场景根目录记为 `<root>`；输出里的绝对路径命令先在 shell 内转成与路径无关的文本，
  避免字节数随目录名变化。
- **超大输出**：只记录归一后结果的 sha256、`stdout.length` 与 `applyResultSizeLimit` 元数据，避免 300 KB 入库。

## 平台

金样只在 macOS（`/bin/bash` 3.2）录制，生成器断言 `darwin`。回放在所有 Unix 上编译运行
（`#![cfg(unix)]`，Linux CI 也跑），前四节与平台无关；real 节中只有两条标了 `darwinOnly`、
在非 macOS 上跳过，测试精确断言跳过名单（`DARWIN_ONLY` 与夹具标记必须一致）与条数：

| 场景 | 原因 | macOS | 其它 Unix |
|---|---|---|---|
| `not-found` | bash 3.2 写 `/bin/bash: x: command not found`，bash 5 多 `line 1:` | 回放 | 跳过（计入 41+1） |
| `cancel-escaped` | 无 tty 的 `set -m` 作业控制随 bash 版本不同 | 回放 | 跳过 |

macOS：real 48 条 + 强制返回 1 条；其它 Unix：47 条，`skipped == ["not-found"]`，强制返回测试直接返回。
Linux 本机未实测（本机 colima 未运行、未拉镜像）；其余 47 条若在 Linux 上出现文本差异会如实变红，不会被跳过。

## 已知边界

- **Windows**：cmd/pwsh/Git Bash 选择与 GB18030 解码未移植。
- **会话**：Node `detached` 调 `setsid()`；本 crate 禁 `unsafe`，子进程用 `process_group(0)` 建新进程组但仍在父会话。
  终止语义（信号发给整组）一致。不同的是控制终端：网关自身有控制终端时，子进程打开 `/dev/tty` 会成功
  （TS 的 `setsid` 子进程得到 ENXIO），`sudo`/`ssh` 之类可能在用户终端提示乃至挂到超时。夹具不覆盖。
- **3 s SIGKILL**：与 TS 同，返回后仍按时发出。实现是唯一一个比结果活得久的有界任务（3 s 内），只持有
  未回收的组长僵尸和组守卫；运行时被丢弃时守卫立即杀组。这是「工具 future 不留分离任务」端口约定的一处
  有意例外（否则 `deferred-kill` 与 TS 不同），例外已登记在 `ExecutableTool` 约定注释处。平台无 WNOWAIT
  时退回收尾立即 SIGKILL。运行时在这 3 s 内整体停机时，任务被丢弃、守卫立即杀组，但组长僵尸要等进程
  退出或下一次 tokio 子进程回收才被收走（无断言覆盖）。
