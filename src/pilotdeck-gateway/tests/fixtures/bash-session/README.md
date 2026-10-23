# bash 会话夹具（#552，A6.5-BASH-OS-001 setsid 部分）

`oracle.json` 来自固定 TS 基线（`cc7c4e6`）的真实运行：真实 `NodeShellCommandRunner`
（`bash/commandRunner.ts`）在**父进程持有控制终端**的前提下 spawn `detached` 的
`/bin/bash` 子进程，记录该子进程自述的会话 / 进程组 / 控制终端 / `/dev/tty`。
`sourceSha256` 覆盖 `commandRunner.ts` 的相对 import 闭包（含
`runtime/commandShell.ts`）；生成器先 `assertBaseline` 校验基线 commit 与干净状态，
再经 `sourceClosureSha256` 对 Git blob 校验闭包哈希。

```sh
PDRS_BASELINE=/abs/fixed/PilotDeck \
  node --import /abs/fixed/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/bash-session/generate.mts /abs/output-parent
```

## 为什么包一层 `script`

`generate.mts` 用 `script -q /dev/null` 运行 `probe.mjs`：script 的子进程拿到一个
新 pty 作为控制终端，模拟「网关从交互终端启动」。没有这一层（生成器本身无 tty）时，
旧实现（无 setsid）也会给出 `ctty=none` / `devtty=errno6`，探针就失去区分度。
`parentDevTty: "open"` 记录在 oracle 里并由生成器断言，证明录制确实带着控制终端
（TS 的 `detached` 子进程依然 ENXIO）。探针结果走文件：script 的 pty 会把 stdout
CRLF 化。script 的 stdin 是 `/dev/null`（script 对 socket 做 tcgetattr 会失败）。

## 四条探针（macOS）

| name | 判据 | TS（setsid） | 旧 Rust（仅 process_group(0)） |
|---|---|---|---|
| session | `getsid(0) == $$` | own | inherited |
| pgroup | `getpgrp() == $$` | own | own |
| ctty | `ps -o tty= -p $$` | `??`（none） | 父有 tty 时 present |
| devtty | `open("/dev/tty")` | ENXIO（errno 6） | 父有 tty 时 open |

`ps -o sess=` 在 macOS 不是 POSIX session id（实测恒为 0），会话一律用
`getsid(2)`；`$$` 经 argv 传入 python3 探针（探针进程是 shell 的直接子进程，
不能拿自己的 pid 比较）。探针命令全部走绝对路径（`/usr/bin/python3`、`/bin/ps`），
runner 的 env 固定 `PATH=/usr/bin:/bin`。

## 稳定性与回放

只记录可复现字段：`exitCode` / `stdout` / `stderr` / `timedOut`；`durationMs` 是
墙钟，不入库。两次真实运行逐字节一致才更新正本（提交时
`sha256(oracle.json) = 02c2f9da4d842bb29ab1387f2349f51c149a701c65556caf0f64a94fc11b6ca2`，
两次运行均为此值）。金样只在 macOS 录制（`??` 记法与 ENXIO=6 都是 macOS 观测；
`/usr/bin/python3` 也非所有 Unix 预装），`tests/bash_session.rs` 的回放仅在
macOS 运行；`fixtures/bash-session/generate.mts` 断言 darwin。

Rust 侧对同一命令逐条重放（同一台机器、同一 `/bin/bash`），断言与金样完全相等；
另有不经金样的直接断言（`getsid(pid) == pid`、组回收）在 `tests/bash_session.rs`。
