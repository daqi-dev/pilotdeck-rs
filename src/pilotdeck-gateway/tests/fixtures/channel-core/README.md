# #1327 pr1 负控

新增源码未在 HEAD 时共享 `mutating_sources` 必须拒跑。本轮只提供脚本与只读锚点核验；不存在绕过守卫的手动变异、影子仓库或 KILLED 证明。协调者提交候选后运行：

```sh
export CARGO_TARGET_DIR=/absolute/path/to/owned/cargo-target
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
python3 src/pilotdeck-gateway/tests/fixtures/channel-core/negative_controls.py /new/evidence/negative
```

`--check-anchors` 只读检查；`--compile-only OUT` 逐项编译；`OUT mutation-name ...` 选样本。14 个变异来自实际分支：enabled、表顺序、nullish fallback、错误形状、缺构造器继续、停用、managed 去重、closing、迟到 start 清理、rollback、失败 stop 恢复、close 错误汇总、状态报告顺序、回调触发。

执行器复用 `negative_process` 的源码/HEAD、进程归属和槽位守卫，以及 `negative_suite` 的具名测试断言判分。每项先正控、变异编译、专属断言判杀、恢复正控；跑完所有项才原子发布 summary。过期锚点、编译失败、无关 panic、未恢复通过都是 INVALID，任一非 KILLED 非零退出。脚本保护本模块所有源码和本单两份 fixture 的所有生成器/子驱动/mock/oracle；不会修改共享执行器。
