# Command hook 输出上限（#363）

固定基线 cc7c4e6，Node v22.22.0。生成器先逐 blob 校验只读 TS source tree，未复制/修改 TS 实现。

```sh
PDRS_BASELINE=/absolute/path/to/PilotDeck node --import \
  /absolute/path/to/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/hooks-output/generate.mts /fresh/output/directory
```

- `large`：真实默认 CommandHookExecutor 经 shell 分别输出 stdout/stderr 64 MiB 零字节，完整保留，exit 0、success、无截断标记。记录长度、SHA256、output、结果键；不存大正文。
- `boundaries`：真实 NodeShellCommandRunner 的 spawn 注入 EventEmitter，TextDecoder.decode 确定性返回惰性字符串；仍执行原 TS listener 和真实 V8 字符串拼接。MAX_STRING_LENGTH=536870888 个 UTF-16 单元，先填到 limit-1、拒绝 2 单元、随后 1 单元可填满；双流独立。这不是 512 MiB 实际管道传输。
- `hookCatch`：相同确定性注入，经真实 CommandHookExecutor。data listener 越界后 promise 未结算；由探针捕获异常并 close 后为 success，证明异步异常不会到 executor catch。
- `uncaught`：独立 Node 子进程不捕获 listener 异常，stdout/stderr 均 exit 1、RangeError: Invalid string length；没有 hook 结果。
- `decoding`：真实 TS decoder 的分块 BOM / emoji 与不完整 UTF-8 EOF 输出。

Rust 的局部 hook 失败与 TS 进程退出不同，沿用已人批 `A6.7-HOOK-OUTPUT-001` / `-R1`，不新增差异。保留整块拒绝后仍容许后续较小块的现有行为，timeout/cancel/exit 2 的优先级不变。

`hooks_output.rs` 回放真实 64 MiB 与 8192 单元注入上限；`exec_output_tests.rs` 检查真实 reader 留存容量（≤6×上限，非 RSS）、排空到 EOF、Unicode 与末尾 flush。64 MiB 变异也只有限分配 64 MiB，不做 OOM。

负控串行运行（证据目录须不存在；hash 必须由审核过的当前 exec.rs 获取，脚本拒绝失配，不替换 HEAD）：

```sh
python3 src/pilotdeck-gateway/tests/fixtures/hooks-output/negative.py \
  _tmp/20261026-issue-363/negative <exec.rs-sha256>
```

9 个变异分别去掉 cap、按字节计数、边界排除、漏记已用量、溢出后永久丢弃、漏标记、漏 EOF 检查、提前停止 drain、改默认上限。每项要求运行期断言失败，随后恢复同一份源字节并重跑全绿；编译失败不算检出。复用 `negative_process` 的树锁与有界进程组回收。
