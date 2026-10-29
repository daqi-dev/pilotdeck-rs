# #994 共享执行器元负控

`negative_controls.py NEW_OUTPUT_DIR` 对 11 项共享执行器变异逐项执行正控、Python 语法编译、
绑定断言负控和恢复正控；全部执行后汇总，任一 SURVIVED / INVALID 即非零退出。
调用方仍须提供 `CARGO_TARGET_DIR`，但该套只启动 Python，不调用 Cargo、不建立构建目录。

入口用 `mutating_sources(root, [])` 保持与其他负控相同的树锁及产品 HEAD 校验，
在锁内另将 `_shared/negative_control.py`、`negative_process.py` 和 `test_negative_adaptive.py`
逐字节与真实 HEAD 比对，之后才复制。空写集合避免通用 guard 在退出时重写真实文件；
这里只读真实输入，正常退出再次核字节与 HEAD。没有替代基线或影子 Git。

每项仅复制这三个 Python 输入到新证据目录内的临时子目录，保留相对布局以隔离 `__file__` 导入。
变异和恢复只写副本。SIGTERM / SIGKILL 即使不运行 finally，也只能留下副本，不能污染真实共享件；
不完整运行的临时副本保留在该次证据目录，不应据此认定套件完成。
脏输入、过期锚点、语法错误、非绑定断言失败、恢复字节不一致或恢复测试失败均不能判 KILLED。

回归：`python3 -B src/pilotdeck-gateway/tests/fixtures/test_negative_adaptive_isolation.py`。
信号探针等待原子发布的 ready 文件（最多 10 秒）后发 SIGTERM / SIGKILL，核真实输入不变，
再用真实 HEAD 守卫重新跑一项；no-op 变异必须 SURVIVED 并退出 1，no-op 恢复必须 INVALID。
