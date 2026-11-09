# macOS arm64 隔离进程抓栈

仅用于自有、隔离运行的 macOS arm64 进程诊断。它绕开 `sample` / LLDB attach 的 task-port 路径；无需 sudo，也不关闭 SIP。不要用于生产服务或正式性能计时。

## 构建与运行

每步先运行 `df -g .`。本任务要求可用空间少于 20G 时立即停止。

使用系统 Command Line Tools 从源码构建，不下载预编译库：

```sh
SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk \
  clang -dynamiclib -g -O0 selfstack-safe.c -o libselfstack-safe.dylib
mkdir -p /absolute/path/to/stack-evidence
DYLD_INSERT_LIBRARIES=/absolute/path/to/libselfstack-safe.dylib \
PDRS_PROFILE_DIR=/absolute/path/to/stack-evidence \
SELFSTACK_DELAY_MS=100 \
SELFSTACK_ROUNDS=20 \
SELFSTACK_INTERVAL_MS=10 \
  /absolute/path/to/isolated-debug-binary <原有启动参数>
```

在采样窗口内触发待诊断请求。默认延迟100ms、20轮、轮间10ms；每轮逐线程串行发信号。`SELFSTACK_ROUNDS` 最多100。短负载应调整延迟或反复触发请求，不能把空闲栈当作热点证据。子进程继承注入环境，各写 `safestack-<pid>.txt`。正式计时应完全移除以上注入环境变量。

## 查看栈

日志开头列出 PID 和每个 Mach-O image 的运行时 header、slide、路径。每个 `ROUND ... THREAD ... BOUNDS ...` 后的 `STACK` 行依次是被中断 PC、LR、FP 链返回地址。根据对应 image 的运行时 header 离线符号化：

```sh
atos -o /absolute/path/to/the-exact-binary \
  -l 0x运行时IMAGE头地址 0x采集地址 0x另一采集地址
```

保留该次二进制、提交号、SHA256和 debug 信息。不要拿其他构建的符号解释当前地址。跨 image 地址分别使用各自 image；无法解析的帧保留原始地址，不猜函数名。

## 实现与限制

worker 在线程外预先查询目标栈边界，串行发送 SIGUSR2，并等待 lock-free atomic 回执。handler 从 ucontext 读取 PC/LR，按预先取得的边界追踪 arm64 FP，用栈上缓冲区手工写十六进制地址并调用 `write`。handler 不做符号化、分配、格式化或 pthread 查询。100ms 未收到回执便停止采样，避免继续更换共享边界。

原 SIGUSR2 处理器不是默认值时拒绝注入，避免覆盖既有处理语义。FP 链受编译优化、缺帧指针、PAC和线程退出影响，可能截断；首个 PC 来自中断上下文，缺帧不能解释为函数未执行。采样线程及信号会干扰调度，样本数量不能直接当作耗时比例。本工具不会绕过 hardened runtime 或系统进程注入限制；只对允许 DYLD 注入的自有隔离进程使用。

不要在信号 handler 内加入 `backtrace_symbols_fd`、`dladdr`、`snprintf` 或其他可能分配/取锁的函数。旧方案在真实 gateway 曾因信号打断持锁线程而卡住；符号化必须放到进程外完成。
