# locale-adaptive 负控（#978 第三批）

覆盖三处：

1. `tests/common/oracle_locale.rs` 的 locale 子进程无输出预算从固定 30 s
   改为按 `load / cpus` 自适应（上限 180 s）。两个编译期开关：
   `locale-idle-nonadaptive`（退回固定 30 s）与 `locale-idle-unbounded`
   （24 h，模拟永不超时）。
2. 同文件的 macOS `sysctl` 负载采样加 2 s 等待上界（超时 kill + wait 回收后
   返回 `None`，走既有 30 s 回退）。开关 `locale-sampler-unbounded` 去掉
   上界，绑定 `hanging_sampler_is_reaped_and_falls_back_to_base_budget`。
3. `tests/common/timing.rs` 的 `cpu_duration` 在开关 `html-cpu-wall-clock`
   下改读 `wall_seconds`，重新注入 #978 fix-r0 的墙钟回归。判据仍是
   `#978 CPU sample excludes preemption`；真实二次实现由基线已有的
   `tests/fixtures/html-timing/negative_controls.py` 的 `materializing-join`
   证明，本脚本不重复文本变异。

`GUARDED` 同时包含判杀断言所在的
`src/runtime/tools/builtin/web/html_compat.rs`：该文件或任一开关/绑定文件
有未提交改动时 `require_head()` 拒绝启动，`source.json` 与结束校验也覆盖它。

开关是 `crate::mutants::on("<name>")` 分支，随
`--features pdrs-mutants --lib` 编译一次；`PDRS_MUTANT` 只由执行器设置，
不从调用方继承。源码不改写，三相为 positive / negative / restored，逐项
记 KILLED / SURVIVED / INVALID，任一非 KILLED 即非零退出。

## 运行

```bash
export CARGO_TARGET_DIR=/absolute/tree/target
python3 src/pilotdeck-gateway/tests/fixtures/locale-adaptive/negative_controls.py --check
python3 src/pilotdeck-gateway/tests/fixtures/locale-adaptive/negative_controls.py NEW_OUT
```

`NEW_OUT` 必须不存在。`--check` 不要求 HEAD；实际运行要求受保护文件与
HEAD 一致（先提交本批改动）。脚本使用调用方 `CARGO_TARGET_DIR`，不另建
target；本树未提交时由协调者在提交后的宿主执行。
