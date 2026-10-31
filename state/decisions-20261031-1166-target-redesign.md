# 决策卡 · #1166 构建目录守卫：停止逐轮补丁，按收敛规格重做（2026-10-31 20:5x，AI 代批，待人复核）

| 卡项 | 内容 |
|---|---|
| 卡在哪 | 构建目录 / 产物归属这一处，r2→r6 改了 5 轮，超过「同一处修 3 次不过挂起」。cb8 r6（HEAD 6965494a）大面积失败，原因都来自前几轮自己加的规则：<br>① r4 我提议的 mtime 下界——cargo 无需重编时产物 mtime 早于本次构建开始，判 `build artifact predates this build`，假 FAIL；<br>② r5/r6 把兄弟脚本改成**只认** `<树>/_build/cargo-target`，宿主链用树外 target，round6 断言不符，workspace-pristine 24 条正控没过；<br>③ tool-budget 写死旧 base SHA 做 `git show`，cb8 克隆里没有这个提交。 |
| 谁受影响 | M6 共享负控件，以及所有经 `mutating_sources` / `build_snapshot` 的负控；#1139 排在它后面。 |
| 能否撤回 | 可以。 |
| 动不动尺子 | 动负控装置的准入规则，不动判分语义。 |
| 收敛规格（本卡定） | 1. `CARGO_TARGET_DIR` 只拒收：空白、`.`、`/`、仓库根、`src/` 内；其余（树内 `_build/…` 或宿主的树外目录）一律接受，规则只在 `negative_process._target_environment` 一处定义。<br>2. 兄弟脚本不再自写允许集合，改调这个共享函数。<br>3. 删掉 mtime 下界；保留锁内归属标记（nonce），复制后复核标记；对抗写者按 `decisions-20261031-1166-threat` 登记为已知边界。<br>4. 不写死历史 SHA：基线用 `HEAD` 或 merge-base。<br>5. 验收以宿主为准：cb8 与 macmini 各跑一次并集清单，全部 KILLED、0 INVALID。 |
| 建议 | 按规格派一轮「重做」（不算第 7 轮修补），两家非作者复核增量。 |
| 原则 → 断言 | 共享准入规则只许一处定义；兄弟脚本写死路径的机检：grep 到 `src/target` 或 `_build/cargo-target` 的字面量允许集合即失败。 |
