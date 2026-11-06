# native-config 负控（#1455）

`config` 方法族（7 个 session 配置方法）与共享 `?.trim()` 守卫的变异负控装置。

## 跑法

先提交（`mutating_sources` 要求被保护源码逐字节等于 `HEAD`，未提交的树会被拒）。运行期间这棵树必须独占：变异体写在源码位置，同树的并发 `cargo build` / `cargo test` 会读到变异字节，把环境竞争报成假 FAIL。写入与回滚都走「仓根 `.negctl-publish/` 临时文件 + `fsync` + 一次 `os.replace`」（#1692 §9）：临时文件不在被扫的 `src` 区内、落在 `.gitignore` 的 `.negctl-*` 命名空间，SIGKILL 残留不会被下一轮 preflight 判成 untracked build input；权限位从源文件带过去，`core.filemode` 下不留 mode-only diff。并发读者只会看到旧字节或变异字节，不会读到半截文件；但「读到变异字节」这一半仍只能靠独占树排除，不是本装置能自己保证的。再：

```
CARGO_TARGET_DIR=<本树独立 target> \
  python3 src/pilotdeck-gateway/tests/fixtures/native-config/negative_controls.py <NEW_OUTPUT_DIR>
```

- `<NEW_OUTPUT_DIR>` 必须不存在（独占创建，不覆盖历史证据）。
- `CARGO_TARGET_DIR` 必须由调用方提供（脚本不猜，避免复用陈旧 target 把环境问题报成「变异没杀死」）。
- 只跑某几条：在输出目录后追加变异名（见脚本 `mutations` 表），可配合 `--compile-only` 只验编译。

只校验锚点、不执行：

```
python3 -I src/pilotdeck-gateway/tests/fixtures/native-config/negative_controls.py --check
```

## 每条变异做什么

正控（命名测试通过）→ 写入变异 → `cargo test --no-run`（编译门）→ 命名测试必须 FAILED 且 panic 载荷含该条的专属断言串 → 恢复源码 → 恢复后正控再跑一次。任何一步失败记 `INVALID`，脚本以非零退出；汇总写 `<NEW_OUTPUT_DIR>/summary.json`。

## 与共享执行器的关系

脚本复用 `negative_process`（`mutating_sources` / `run_owned` / `wait_for_slot`）与 `negative_suite`（`Case` / `classify` / `invalidate_completed` / `passed` / `publish_summary`），但自管逐案流程：每个变异前要插一次 `cargo test --no-run` 编译门，`negative_suite.run_cases` 当前没有该钩子。

三态退出契约逐字沿用 `run_cases`（#1076 fix-r2）：进入守卫前失败 rc=1；守卫展开期间失败、恢复未验证、取消、或守卫在回卷途中抛出（此时 `phase` 仍写着 `body`，靠 `body_error` 同一性区分）一律 rc=2。当前 26 条变异。
