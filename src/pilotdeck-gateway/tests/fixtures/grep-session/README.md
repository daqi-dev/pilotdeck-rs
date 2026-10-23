# grep 会话夹具（#552 修复轮 1，rg 子进程 setsid）

`oracle.json` 来自固定 TS 基线（`cc7c4e6`）的真实运行：真实
`createNodeSubprocessPort().executeFile`（`SubprocessPort.ts:59-65`，即
`filesystem/ripgrep.ts:39-46` 驱动的 spawn，`detached: process.platform !==
"win32"`）在**父进程持有控制终端**的前提下直接 spawn 探针可执行文件（shell
脚本），记录该子进程自述的会话 / 进程组 / 控制终端 / `/dev/tty`。
`sourceSha256` 覆盖 `SubprocessPort.ts` 的相对 import 闭包（含
`commandRunner.ts` 与 `runtime/commandShell.ts`）；生成器先 `assertBaseline`
校验基线 commit 与干净状态，再经 `sourceClosureSha256` 对 Git blob 校验闭包
哈希。

```sh
PDRS_BASELINE=/abs/fixed/PilotDeck \
  node --import /abs/fixed/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/grep-session/generate.mts /abs/output-parent
```

## 为什么包一层 `script`

`generate.mts` 用 `script -q /dev/null` 运行 `probe.mjs`：script 的子进程拿到
一个新 pty 作为控制终端，模拟「网关从交互终端启动」。没有这一层（生成器本身
无 tty）时，旧实现（仅 `process_group(0)`）也会给出 `ctty=none` /
`devtty=errno6`，探针就失去区分度。`parentDevTty: "open"` 记录在 oracle 里
并由生成器断言，证明录制确实带着控制终端（TS 的 `detached` 子进程依然
ENXIO）。探针结果走文件：script 的 pty 会把 stdout CRLF 化。script 的 stdin
是 `/dev/null`（script 对 socket 做 tcgetattr 会失败）。

## 四条探针（macOS）

| name | 判据 | TS（detached / setsid） | 旧 Rust（仅 process_group(0)） |
|---|---|---|---|
| session | `getsid(0) == $$` | own | inherited |
| pgroup | `getpgrp() == $$` | own | own |
| ctty | `ps -o tty= -p $$` | `??`（none） | 父有 tty 时 present |
| devtty | `open("/dev/tty")` | ENXIO（errno 6） | 父有 tty 时 open |

`$$` 是运行探针脚本的 shell——executeFile 直接 spawn 的那个进程——的 pid；
每个 python3 探针是它的直接子进程，比较对象是那个 shell。`ps -o sess=` 在
macOS 不是 POSIX session id（实测恒为 0），会话一律用 `getsid(2)`。探针命令
全部走绝对路径（`/usr/bin/python3`、`/bin/ps`），executeFile 的 env 固定
`PATH=/usr/bin:/bin`。

## 稳定性与回放

只记录可复现字段：`exitCode` / `stdout` / `stderr` / `timedOut`；
`durationMs` 是墙钟，不入库。两次真实运行逐字节一致才更新正本（提交时
`sha256(oracle.json) = e4d9de7f4884a9e9b836d6b07d1995f29e4775f8d9b70902df3bbb634c823439`，
两次运行均为此值）。金样只在 macOS 录制（`??` 记法与 ENXIO=6 都是 macOS
观测；`/usr/bin/python3` 也非所有 Unix 预装），回放测试
（`grep.rs` 单元测试 `detached_ripgrep_session_matches_real_ts`）仅在 macOS
运行；`generate.mts` 断言 darwin。

Rust 侧对同一探针脚本（oracle 内嵌 `script` 字段，逐字节重建）经生产入口
`run_ripgrep_with_timeout`（`PILOTDECK_RIPGREP_PATH` 覆盖，grep 与 glob 共用
该 spawn）逐条重放，断言 stdout 与金样完全相等；另有不经金样的直接断言
（`getsid(pid) == pid`、组回收）`detached_ripgrep_leads_its_own_session`。
