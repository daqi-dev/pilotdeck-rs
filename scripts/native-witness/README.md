# #1140 Linux 原生 exec / 出网见证

本目录是独立采集器，不是 A7 评分入口。`session_domain.py`、冻结清单和 native 格的 `NOT_RUN` 不变；A5.4 之后再接评分路径。

## 运行

只支持 Linux（内核需支持 pidfd），要求 Python 3.11+、已有 strace（支持 `--kill-on-exit`）、已编译的 Linux gateway 与 sidecar。无需 root，不 attach 用户进程。macOS 的 eslogger + root 后续另做，不回退 ps 采样。

```sh
# 从仓根构建；sidecar 属 pilotdeck-transport，两个 --bin 分别构建。
export CARGO_TARGET_DIR="$PWD/_build/cargo-target"
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
scripts/cargo-slots.sh && cargo build --manifest-path src/Cargo.toml -p pilotdeck-gateway --bin pilotdeck-gateway
scripts/cargo-slots.sh && cargo build --manifest-path src/Cargo.toml -p pilotdeck-transport --bin pilotdeck-agent-loop-sidecar

python3 -I scripts/native-witness/collect.py \
  --gateway "$CARGO_TARGET_DIR/debug/pilotdeck-gateway" \
  --sidecar "$CARGO_TARGET_DIR/debug/pilotdeck-agent-loop-sidecar" \
  --output _tmp/20261026-issue-1140/linux-native-001

python3 -I src/pilotdeck-gateway/tests/fixtures/native-witness/test_witness.py \
  --capture _tmp/20261026-issue-1140/linux-native-001/capture
```

输出目录必须不存在。默认网关 `127.0.0.1:28999`、模型 mock `127.0.0.1:29000`；`--port` 可选 28999..29008（模型为下一端口），绑定前使用 `scripts/gateway-parity/common.py::parity_lock` 的 `/tmp/pdrs-parity.lock` 目录租约，覆盖采集和 provider 清理；目录占用轮询 timeout 为 1 秒（guard 行为继承共享协议），锁忙或任一固定端口占用均报告 `BLOCKED`、退出码 2，不搜索或连接其他进程。预检和启动失败复查共用 `SO_REUSEADDR` 探针，允许 TIME_WAIT、仍拒绝活动监听。与 CLI/native-topology/全量对拍共用租约。独立 `home/`、`project/`；环境只带这次 home、固定 PATH/LANG，不读真实 key，不碰 `~/.pilotdeck` 或 18789。仅发一次本地模型轮次（产品可能另发标题请求）；driver 的每次等待共用 60 秒绝对 deadline。模型服务器在 accept 后立即设置 5 秒 socket 空闲超时，覆盖请求行、headers 和 body。

CLI 消费者判定可重试 BLOCKED 时，必须同时核对退出码 2 和 **stdout JSON 的 `status == "BLOCKED"`**。argparse 用法错误同样返回 2，但只往 stderr 写 usage、stdout 为空，不能按退出码单独归为 BLOCKED。成功状态也走 stdout。绑定竞态可能已创建输出目录，重试必须换新目录。

`collect.py` 通过真实 native opt-in 启动 gateway，完成 WS hello / submit_turn，本地 mock 返回文本，以 pidfd 核实所属 tracer 后 SIGTERM 停机，等待完整 trace 与退出码 0，再确认 sidecar exec、模型出网和 JSONL 落盘；这些检查及 provider 清理都先于 receipt 发布。`driver.json` 保留本轮 WS 字节、解码和 mock 请求。它是驱动观察，不是已冻结的完整 A7 observer。

`capture/` 保留：

- `prelaunch.json`：执行前固定的 ELF 路径、完整 argv、sha256、角色、端点与 case；`launch.json` 只追加实际 tracer 的 launcherPid。
- `trace.log`：strace 原始完整进程树日志；`stdout.log`、`stderr.log`：被测物输出与跟踪器诊断。
- `evidence.json`：processes / execWitness / networkWitness / cleanup，直接采用冻结消费者字段名；exec 包含短命后代、退出码、父子关系与线程归属；网络包含地址、端口、TID、TGID 和系统调用结果（失败的 connect 也保留）。
- `receipt.json`：只有正常完成、解析完整、smoke 验证及 provider 清理通过后才原子发布；四份输入/输出哈希与 tracer 退出码。失败只有 `failure.json`，不能算 WITNESS_CAPTURED。

## 证据边界

`strace -f` 追踪 execve/execveat、clone/clone3/fork/vfork、exit/exit_group、connect、sendto/sendmsg/sendmmsg、io_uring_setup。保留 exit status，不能使用 `-qq`；`-xx` 避免 C 字符串转义歧义，过长/不认识的格式拒收。[strace 官方手册](https://github.com/strace/strace/blob/master/doc/strace.1.in) 定义这些选项。

本版本是受控 ELF / TCP profile：argv/path 必须与启动前登记一致，sha256 在启动前及采集后核对；假定受控二进制运行中不被替换后又恢复，**不是 exec 瞬间的内核镜像哈希证明**。脚本解释器、成功 execveat、同 PID 重复 exec、PID 复用、无 exec 的 fork 子进程、未知可执行文件、未退出线程、逃逸 clone flags、AF_UNIX helper、地址型 UDP/SCM_RIGHTS、成功 io_uring_setup 均拒完整。Popen 关闭继承描述符，观测范围是它启动的子树；不防恶意内核/同用户篡改整套原始证据。线程 connect 归 TGID，已连接 socket 的无目的地址 write/send 不另计新出口。孙代照实记录，但冻结审计当前只允许 host 直接子进程；不会把孙代伪装成直接子进程。

正常停机要求 gateway 在 `--drain-ms 1500` 下处理 SIGTERM 并 exit 0，tracer 同样退出 0；cb8 对 HEAD 949b385a 的原始 trace/receipt 已实证此形态。被信号杀死或其他非零退出仍拒收，不能当正常 drain。`signal.pidfd_send_signal` 自 Python 3.9 已提供（见 [Python 3.11 官方文档](https://docs.python.org/3.11/library/signal.html#signal.pidfd_send_signal)），不降级为存在 PID 复用窗口的 `os.kill`。

取租约之前安装 SIGINT / SIGTERM 处理器，直到租约退出后才恢复；预检、provider 启动、capture 前后都走取消清理。provider 身份发布和清理窗口使用共享 `deferred_signals`，provider 线程继承屏蔽掩码，主线程进入 capture/Popen 前恢复原掩码。capture 启动发布窗延迟 Python 异常直到 Popen 身份存下，清理期间重复信号延迟处理。provider shutdown 等待上限 6 秒，随后线程 join 上限 6 秒；清理失败保留 `cleanupPending` 租约，不发布成功。异常/超时杀本次持有的 tracer；`--kill-on-exit` 使内核终止其 tracees（包括改变会话的后代）。强制 SIGKILL 采集器不保证执行 Python 清理，协调者应先 SIGTERM；不声称失败路径 `allStopped=true`。

## 原有审计与漏记负控

离线 `load_capture()` 核对 receipt 哈希与启动前后合同；`checked_evidence()` 从原始 trace 重建全部进程与出网清单，再和派生产物精确对比，绝不把不完整标志提升为 true。`audit()` 将核验后的证据交给原封不动的 `session_domain.audit_native`，并调用冻结 `_verify_cli_imports()`；没有影子 git、替代 HEAD 或关闭守卫。

冻结函数本身只信 `networkWitness.complete`，`calls=[]` 且 complete=true 会被它接受；本包明确测试这个既有边界。漏记出网通过独立回放使 complete=false，然后由原函数以 `NETWORK_WITNESS` 拒绝。短命子进程漏记与 source=ps 同样由原函数拒绝。

完整 audit 还要求 observerRows、预注册 binding、capabilities、cases 和 output/disk/http 表面。真实 binding 是运行时生成的 UUID，现阶段不能伪称启动前已知。因此 `--capture` 测试把**真实 exec/network 见证**放进标为 `SYNTHETIC_CONTROL_ONLY` 的非 witness 外壳，验证接口兼容和三项拒收；输出 `REAL_WITNESS_WITH_SYNTHETIC_AUDIT_ENVELOPE`，绝不作为完整 A7 通过。全部真实报告继续标 `scoring: NOT_RUN`。

## 开发检查

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/native-witness/test_witness.py
python3 -I src/pilotdeck-gateway/tests/fixtures/native-witness/negative.py --check-anchors
# 新源码提交后，使用调用方既有 CARGO_TARGET_DIR：
python3 -I src/pilotdeck-gateway/tests/fixtures/native-witness/negative.py \
  _tmp/20261026-issue-1140/negative-committed-001
```

源码负控依赖 `negative_process.mutating_sources/run_owned`；每条变异先 `compile` 检查 Python 语法，再正控、变异、恢复复测，全部跑完汇总，共 37 个变异；可加 `--case NAME` 仅选一个样本。源码未进 HEAD 时正常拒绝，不是 KILLED。Python 测试不运行 cargo，不创建第二构建目录。不新增 TS 金样，不声称 TS 行为对齐。
