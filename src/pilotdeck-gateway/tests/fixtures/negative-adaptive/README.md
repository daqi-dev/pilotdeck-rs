# #994 共享执行器元负控

`negative_controls.py NEW_OUTPUT_DIR` 对 11 项共享执行器变异逐项执行正控、Python 语法编译、
绑定断言负控和恢复正控；全部执行后汇总，任一 SURVIVED / INVALID 即非零退出。
调用方仍须提供 `CARGO_TARGET_DIR`，但该套只启动 Python，不调用 Cargo、不建立构建目录。

入口先独立调用 Git 固定 HEAD OID、枚举并逐字节核验输入，再从已核验的内存字节加载共享执行器，
包括闭包内的本地依赖；对这些模块不导入待检工作树代码，也不信任旧 `sys.modules` 缓存。
随后用 `mutating_sources(root, [], baseline=commit)` 保持与其他负控相同的树锁及产品校验，
锁内再核输入，退出再次核字节与 HEAD OID。空写集合避免通用 guard 重写真实文件。
没有替代基线或影子 Git；此 bootstrap 的私有锁实例不能与外层另一模块实例的树锁嵌套使用。

每项以共享执行器和绑定的 `test_negative_adaptive.py` 为入口，从同一 HEAD OID 解析 `import` / `from`
的传递闭包（含函数内部导入、相对导入、包初始化），在 `fixtures/` 顶层共享 `.py` 与 `_shared/` 包中定位依赖。
新增共享模块无需更新硬编码清单；bootstrap 拒绝未登记的 `import` / `from` 本地导入，标准库仍由 Python 提供。
当前没有拦截 `importlib.import_module` 对闭包外已缓存模块的访问，不能据此宣称动态导入完全隔离。
复制到新证据目录内的临时子目录，保留相对布局以隔离 `__file__` 导入。
子进程清除调用方的 `PYTHONPATH` / `PYTHONHOME`，避免从真实树或外部目录回退导入。
变异和恢复只写副本。SIGTERM / SIGKILL 即使不运行 finally，也只能留下副本，不能污染真实共享件；
不执行 finally 的信号中断可能在该次证据目录留下副本；普通异常会清理，不应据此认定套件完成。
脏输入、过期锚点、语法错误、非绑定断言失败、恢复字节不一致或恢复测试失败均不能判 KILLED。

回归：`python3 -B src/pilotdeck-gateway/tests/fixtures/test_negative_adaptive_isolation.py`。
CI 的必需 `metadata-observer` job 自动运行；其现有 `EnvironmentControls` 独立守卫该步骤及缺失负例。
`isolation_negative.py NEW_OUTPUT_DIR [control...]` 对本轮守卫做 17 项内存变异，逐项编译并绑定完整测试名与消息，
正控/变异/恢复日志分别留存；不写原源码，不创建 Git 副本或 target。
信号探针等待原子发布的 ready 文件（最多 10 秒）后发 SIGTERM / SIGKILL，核真实输入不变，
再用真实 HEAD 守卫重新跑一项；no-op 变异必须 SURVIVED 并退出 1，no-op 恢复必须 INVALID。
