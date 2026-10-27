# 决策卡 · worker 并发改按 cargo 进程计（2026-10-27 00:0x）

- **卡在哪**：slot-run.sh 把「同时在跑的 worker ≤ 4」当成 CLAUDE.md「资源上限」的「同时编译/测试的 cargo 进程 ≤ 4」。worker 多数时间在读码和推理，不在编译；15 单积压时排队三四个小时（另有登记表被手动追加导致的死锁，见 board 10-26 23:5x）。
- **人定**（10-26 23:5x 对话）：按 cargo 进程数计，worker 不限。另指示：这类调度口径以后由协调者自行判断决策。
- **实现**：`_tmp/20261025-claude-loop/cargo-gate/{cargo,gate.py}` 放在 worker PATH 最前：编译类子命令先取 4 个 flock 名额之一（`/private/tmp/pdrs-cargo-gate/slot0-3`），`ps` 可用时再按 `scripts/cargo-slots.sh` 同口径数全局 cargo 进程（兼容协调者与宿主核验的直接调用），然后 exec 真 cargo，名额随进程持有到退出；`fmt`/`metadata` 等不编译的直接放行。`run-worker.sh` 替代 slot-run.sh，dispatch-fix / dispatch-issue 已切换。实测：5 个并发只放行 4 个，第 5 个等到释放后才运行。
- **能否撤回**：能。dispatch 脚本改回 slot-run.sh 即可。
- **动不动尺子 / 意向**：不动。上限数值（4）不变，只改计数对象。
- **待观察**：worker 沙箱若不能写 /private/tmp，闸门退化为只按进程数核（codex 沙箱内 ps 被挡时等于不限），首批交付后核 `/private/tmp/pdrs-cargo-gate` 锁的实际持有者与负载。
