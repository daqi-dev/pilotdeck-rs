# #748 写入顺序实测

`generate.mts` 用 Node v22.22.0、固定 PDRS_BASELINE、live-e2e 的真实 TS sidecar、隔离 HOME 和本机 HTTP mock。`probe.mts` 包装真实类的方法记录调用/完成顺序，不重写 TS 源码，不读取 Rust 输出。状态钩子位于 writer 已入队的 append sink 内，释放点为八次 setImmediate；durable 钩子延迟真实回调，另外两格注入明确写入错误。context-cancel 格在挂起状态写时发取消，确认取消尚未完成后才释放前驱。

五格金样为 text、tool、assistant-error、tool-error、context-cancel。证实 context_budget 先排入 writer 队列并先于 turn_result 完成；assistant_message/tool_results_projected 在回调开始前已 yield；取消也不能越过 TS writer 的挂起前驱。生成文件用 recursive mkdir 与 wx 独占写入。

Rust 消费者为 submit_tests 的有序/有界/取消三格、live_stream_tests 的两类消息成功/失败四格和 submit_host_tests 的 ask 归一化。`generator_provenance::live_write_order_regenerates_identically` 是 ignored 再生守卫。无限挂起时 Rust 按 #748 用户约束用现有 abort budget 限制 callback 等待，差异登记见 `specs/gateway-parity/diffs/A7.2-TURN-STATUS-001.md`。已经进入的阻塞存储操作仍归存储层所有，本切片不承诺中断内核 I/O。

负控在 `../live-e2e/negative_controls.py`：六个生产库开关只在 cfg(test) + pdrs-mutants 下启用，绑定完整 lib 测试名和 nonce 失败回执；四个派生期望开关绑定 live_early_final。运行器逐项编译、正控、负控、恢复并最终汇总。HEAD-only 守卫适用于完整宿主执行，未提交的新文件不绕过。
