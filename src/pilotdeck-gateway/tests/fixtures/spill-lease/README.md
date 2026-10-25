# Spill wait lease regression

固定 cc7c4e6 的真实 ToolResultBudget 与原生文件描述符。两格并行，在第一条写腿后实际停顿 30.05 秒：无 follower / 同 key follower。TS 两格 leader 都成功；follower 的 alias 为半写，Rust 拒半写/接管见未批准差异 A7.4-TOOLBUDGET-CONTEXT-ISOLATION-001-R1。

生成器运行前核基线源闭包、Node v22.22.0；输出目录 recursive:true，oracle.json flag:wx，45 s watchdog。Rust 以每票据年龄钩子跨过真实 30 s 常量，在确定性写 gate 处验证，不修改全局等待上限。

重生守卫：设 PDRS_BASELINE 后运行 `cargo test -p pilotdeck-gateway --test tool_budget spill_lease_regenerates_identically -- --ignored --nocapture`。

负控与恢复后验证：兄弟 `../spill-isolation/negative_controls.py`（leader-expiry/claim-expiry/late-publication）；原始证据 `_tmp/20261026-issue-638-690/fix-r1/`。
