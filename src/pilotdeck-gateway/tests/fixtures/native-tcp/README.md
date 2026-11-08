# Native TCP 双轮探针

以隔离 HOME、项目、端口和本地假 provider 运行真实 control、gateway、TCP sidecar。三格分别为同会话正常、同会话变更旧 marker、另一会话变更旧 marker。marker 变更前已新增有效的同项目 marker，实时 list_projects 确认并选用登记项目；不声称证明 legacy fallback。memory 对本测试无关，因此关闭；provider 总请求上限维持 5。

成功要求两轮真实 completed、requestId 唯一且非 default、runId 与 operationId 正确、各帧序列连续并 final、健康检查成功；结束 owned 子进程并 join relay handlers 后核对 wire 与 errors。输出目录必须不存在。

```sh
python3 two_turns.py --case normal --control /absolute/path/pilotdeck-control --sidecar /absolute/path/pilotdeck-agent-loop-sidecar --out /private/tmp/unique-tcp-normal
python3 two_turns.py --case same-marker --control /absolute/path/pilotdeck-control --sidecar /absolute/path/pilotdeck-agent-loop-sidecar --out /private/tmp/unique-tcp-same
python3 two_turns.py --case fresh-marker --control /absolute/path/pilotdeck-control --sidecar /absolute/path/pilotdeck-agent-loop-sidecar --out /private/tmp/unique-tcp-fresh
```

同一 driver 对旧 r80 的 normal 必须因第二轮 `OPERATION_REQUIRES_RECONCILIATION` 退出非零，不能将超时或启动错误算作负控。所有运行保留 result、wire、provider、stderr，另核输入与二进制 SHA 及子进程清理。SIGTERM 由 driver 捕获并进入 finally，向其自建进程组发送 SIGTERM，必要时升级 SIGKILL，关闭并 join relay，再保存记录；driver 必须非零退出。

非 normal 场景严格验证旧 marker 为 mode 000 且读取失败，否则不计通过。退出先关闭自有进程组和 socket、限时 join 所有产生错误的线程；任何清理或超时错误都使结果失败。清理和归档期间的 SIGTERM/SIGINT 被记录而不打断证据写入，结果提交前复核并以非零退出；结果文件须待进程退出后读取。SIGKILL 无法捕获。
