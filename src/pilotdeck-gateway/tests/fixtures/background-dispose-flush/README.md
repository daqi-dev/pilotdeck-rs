# #994 dispose 的写链等待边界

`generate.mts` 在 Node v22.22.0 上加载固定 cc7c4e6 的真实 BackgroundTaskRuntime / TaskOutputStore；不配置 snapshotStore，避免最终持久化再次 flush 掩盖 dispose 漏等。

| mode | dispose 前写入 | 观测时 | 预期 |
|---|---|---|---|
| pending | fs.appendFile 门闩挂起 | 真实 output.close 已调用，门闩仍持有 | dispose pending，ring 保留；放行后原始字节完整 |
| ready | 写链已成功 flush | dispose 收尾 | dispose 完成，ring 释放，磁盘完整 |
| cancelled | fs.appendFile 门闩挂起 | close 已调用，然后以 AbortError 拒绝写入 | 拒绝前 pending；拒绝落定后正常完成，文件不存在 |

pending 的门闩由探针独占：在释放前前驱无法完成，不用无限等待模拟“永不完成”。所有等待受 10 秒总截止保护，超时是探针失败。取消格是注入真实 FS 调用点的 AbortError，不声称进行了宿主 I/O 取消。

Rust `runtime::background::output::dispose_tests` 用已存在的写盘门闩和错误钩子重放三行。starts/kill_all/entry.done 都已落定，再直接 poll `dispose_now`：未完成时唯一 pending 点是写链；不依赖 public dispose 的 spawn 调度时差。旧 `dispose_waits_for_gated_output_write_before_returning` 继续验证 public dispose 与 snapshot 恢复。

关键 pending 格的配对负控为 background-output 中保留的 `x-dispose-no-flush-await`，分别绑定 `dispose_without_snapshot_waits_for_gated_write` 与 `dispose_without_snapshot_waits_for_cancelled_write`，只认各自完整测试名及 `#994 dispose-flush must await gated write without snapshot store`，setup panic、其他断言和超时均不算杀。ready 是该变异的等价子域，专门保留为边界正控；不得用 ready 正控要求该变异被杀。

重生成守卫：`cargo test -p pilotdeck-gateway --test generator_provenance background_dispose_flush_regenerates_identically -- --ignored --exact --show-output`，需设置 PDRS_BASELINE；生成器递归建输出目录，oracle.json 独占创建。来源哈希随金样记录。
