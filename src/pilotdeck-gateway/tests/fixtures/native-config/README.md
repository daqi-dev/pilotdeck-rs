# native-config 负控（#1455）

`config` 方法族（7 个 session 配置方法）与共享 `?.trim()` 守卫的变异负控装置；#1864/#2272/#2562/#2195 起接入跨族变异源（`assembly.rs` / `runtime/turn/live.rs` / `native_process/tools.rs` / `requests.rs` / `services.rs` / `methods/cron.rs`，见下「锚点」段）。

## 跑法

先提交（`mutating_sources` 要求被保护源码逐字节等于 `HEAD`，未提交的树会被拒）。运行期间这棵树必须独占：变异体写在源码位置，同树的并发 `cargo build` / `cargo test` 会读到变异字节，把环境竞争报成假 FAIL。写入与回滚都走「仓根 `.negctl-publish/` 临时文件 + `fsync` + 一次 `os.replace`」（#1692 §9）：保护来自位置——仓根不在 `_assert_input_areas` 的扫描区（`src` / `.cargo` / `rust-toolchain*`）内，SIGKILL 残留不会被下一轮 preflight 判成 untracked build input（该扫描用 `git ls-files --others`，**故意包含**被忽略文件，所以 `.gitignore` 的 `.negctl-*` 只是让 `git status` 保持干净，不是保护本身）；权限位从源文件带过去，`core.filemode` 下不留 mode-only diff。并发读者只会看到旧字节或变异字节，不会读到半截文件；但「读到变异字节」这一半仍只能靠独占树排除，不是本装置能自己保证的。再：

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

正跑也会在起 cargo 之前无条件跑同一条守卫（只读 15 个文件、秒级），坏锚当场拒跑，不会烧掉整轮编译预算；`--check` 的用处是不进变异、不起进程的预检，CI 与宿主链都不会替你跑它（gateway 夹具不在 CI 的 negative-controls 作业里）。

这条自检自己的红侧由同目录 `test_literal_anchors.py` 钉住（正控跑真 `--check` 并核输出两串计数；十条负控各破坏一个不变式，要求对应的拒绝语；其中两条钉「定点重跑只校验被选中的行、但 pin 仍按全表绑定」）：

```
python3 -m unittest -v src/pilotdeck-gateway/tests/fixtures/native-config/test_literal_anchors.py
```

除了逐条变异锚点，`--check` 还校验 `LITERAL_ANCHORS` 表（#2065）：`expect` / `expect_err` 收 `&str`、不做插值，panic 载荷里的 `{label}` 是字面量，所以这类断言串必须逐字出现在被扫描源码里；`negative_assert!` / `negative_assert_eq!` 经 `format!` 插值，**只有带 `{...}` 占位符的模板**才与载荷不同（无占位符的宏断言同样是逐字源码文本，照常登记）。扫描面是 `SOURCE_TEXTS`：被变异的源文件（含 #1604 起加入的 `methods/mod.rs`、#1860 起加入的 `runtime/session/factory.rs`、#1864 起加入的 `assembly.rs` / `runtime/turn/live.rs` / `native_process/tools.rs`、#2272 起加入的 `requests.rs`、#2562 起加入的 `services.rs`、#2195 起加入的 `methods/cron.rs`），加上 `live_stream_tests.rs` / `review_tests.rs` / `factory_tests.rs` / `management_tests.rs` / `cron_tests.rs` 五个承载 #1864 / #1860 / #2562 / #1509 断言消息的测试文件；表项路径必须落在其中，否则 `--check` 报 `outside SOURCE_TEXTS`。新增/改这类锚点时把 `(源码路径, 逐字子串)` 一并登记，否则 `--check` 报错。表项与行的绑定是**逐字相等**：某行断言必须原样等于表里的一条（取源码前缀的行，表里就存那个前缀），所以把行改回比前缀更长的自造后缀会被判 `unbound to any row`；反过来，断言逐字出现在源码里却没登记会被判 `unpinned literal anchor`。登记按 `(文件, 串)` 成对判定：同一串在两个扫描文件里各逐字出现时，两边都要各登记一条，删掉其中一条仍会报错。

## 每条变异做什么

正控（命名测试通过）→ 写入变异 → `cargo test --no-run`（编译门）→ 命名测试必须 FAILED 且 panic 载荷含该条的专属断言串 → 恢复源码 → 恢复后正控再跑一次。任何一步失败记 `INVALID`，脚本以非零退出；汇总写 `<NEW_OUTPUT_DIR>/summary.json`。

## 与共享执行器的关系

脚本复用 `negative_process`（`mutating_sources` / `run_owned` / `wait_for_slot`）与 `negative_suite`（`Case` / `classify` / `invalidate_completed` / `passed` / `publish_summary`），但自管逐案流程：每个变异前要插一次 `cargo test --no-run` 编译门，`negative_suite.run_cases` 当前没有该钩子。

三态退出契约逐字沿用 `run_cases`（#1076 fix-r2）：进入守卫前失败 rc=1；守卫展开期间失败、恢复未验证、取消、或守卫在回卷途中抛出（此时 `phase` 仍写着 `body`，靠 `body_error` 同一性区分）一律 rc=2。当前 108 条变异（以 `--check` 报的锚点数为准，写死数字随登记漂移，以该报数为准）。
