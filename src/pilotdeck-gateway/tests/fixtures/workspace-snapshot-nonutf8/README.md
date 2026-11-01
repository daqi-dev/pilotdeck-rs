# WorkspaceSnapshot non-utf8 负控装置（#283）

本目录是 `workspace-snapshot` 金样 non-utf8 场景的**负控与 Linux 验证装置**，不是独立金样：金样本体仍在
`../workspace-snapshot/`（28 场景版 `oracle.json`，SHA-256
`1c93b732a818dabdb49c736119e07921f1dadf4fb760c7d45f4a8d986b0828b1`，2026-11-01 于 cb8/ext4 生成，
溯源登记见 `../workspace-snapshot/README.md`）。产品行为由
`src/pilotdeck-gateway/src/storage/workspace_snapshot.rs` 的 `scan_visit` 实现
（`to_string_lossy` → 重编码 lstat → `io_message`（measured node_io 表）→ 失败落 `_FAILED.json` marker）。

## 被钉住的测试

`tests/workspace_snapshot.rs` 的 `non_utf8_capture_fails_with_lossy_lstat_enoent_on_linux`
（`#[cfg(target_os = "linux")]`）：从提交金样取 non-utf8 场景（capture 步骤按 `op == "capture"`
定位而非取最后一步，生成器同款），写 raw 文件前断言 `rawFiles[].dir == "workspace"`（自检按
workspace 扁平比对的前提，写错目录响亮失败），raw 字节自检（`read_dir` 按 `.as_bytes()` 对
意图，不依赖被测的解码），跑 Rust recorder capture，逐字段对金样断言
（snapshotId / state / phase / marker 路径 / error 文本 / marker 投影后 sha256+size / 无 manifest），
全部断言带 `#283` 专属消息——负控只认这些锚点。另断言 `non_utf8_names_writable` 为真：若覆盖在
Linux 上被静默跳过（文件系统不再接受该字节），该门响亮失败而不是让 27 场景的回放保持绿色。非
Linux 平台上的配对测试 `non_utf8_scenario_stays_skipped_where_raw_names_cannot_exist` 断言探测
必须为假（跳过即文档化行为）且金样仍含该场景名。#283 fix-r1 起 `tests/workspace_snapshot.rs`
里旧的无平台门 regen 守卫 `workspace_snapshot_fixture_regenerates_identically` 已删（macOS 必然
假失败，见 `../workspace-snapshot/README.md`）：本 fixture 的重生成守卫只剩
`generator_provenance.rs` 的 Linux 专属门（钉 Node v22.22.0 / ICU 78.2 / libuv 1.52.1）。

## 变异表（7 个，全绑 #283 专属断言）

| 变异 | 文件 | 改法 | 杀点（dedicated 断言） |
|---|---|---|---|
| decode-strip-fffd | 产品 storage | `scan_visit` 的 lossy 名 `.replace('\u{FFFD}',"")` | error 文本断言（`bad-.txt` ≠ `bad-<fffd>.txt`：lstat 目标被静默改名） |
| lstat-rust-display | 产品 storage | lstat 的 `io_message(...)` → `e.to_string()` | error 文本断言（Rust Display ≠ Node 文案） |
| lstat-skip-entry | 产品 storage | lstat `map_err` → `let Ok/else continue`（静默跳过坏名） | state 断言（committed ≠ failed：capture 悄悄少了坏名条目） |
| oracle-error-drift | 金样 oracle.json | non-utf8 error 文本 `lstat`→`stat` | error 文本断言（证明金样真驱动比较，不是自洽回声） |
| oracle-raw-dir-drift | 金样 oracle.json | non-utf8 `rawFiles[0].dir` `"workspace"`→`"elsewhere"` | raw-dir 断言（证明自检前提真被钉住，不是碰巧扁平比对） |
| gate-capture-locator-op | 专用门测试 | capture 定位 `op == "capture"`→`op == "list"` | 定位失败的 expect 消息（证明定位按 op 判别，不是碰巧取到） |
| guard-uv-drift | regen 守卫测试 | 守卫内 libuv 预检期望 `1.52.1`→`0.0.0` | 守卫的 libuv 断言消息（证明 uv 真被钉住） |

`negative_controls.py`：逐项先 `--no-run` 验编译（编译不过记 INVALID，不记 KILLED），全跑完汇总
（绝不因首个失败中止），锚点数≠1 记 INVALID 报错退出（绝不记 SURVIVED），每项恢复后校验
SHA256 逐字节一致，`summary.json` 逐项落盘，任何非 KILLED 以非零码退出。走
`negative_process.mutating_sources`/`run_owned`/`wait_for_slot`；因 `mutating_sources` 证明树等于
HEAD，提交前拒跑（NOT_RUN），提交后由协调者执行。

## cb8 运行

```sh
CARGO_TARGET_DIR=<dir> ./run-linux.sh /home/clawbox-08/ws/pdrs-runner/resources/PilotDeck [evidence-dir]
```

环境契约：钉死 Node v22.22.0（ICU 78.2 / libuv 1.52.1，默认
`~/opt/node-v22.22.0-icu78-linux-arm64`，可用 `PDRS_NODE` 覆盖）、`LANG=C.UTF-8` 且无
`LC_*` 覆盖（并断言 resolved locale 为 `en-US`，第三 locale 是 BLOCKED 不是 parity 失败，
同 `../project-files-linux-dirent/run-linux.sh`）、临时目录归一到 `/tmp`（`TMPDIR` **导出**
为 `/tmp` 且 `TMP`/`TEMP` 一并清掉：Node 的 `os.tmpdir()` 在 `TMPDIR` 缺失时仍读
`TMP`/`TEMP`，v22.22.0 实测 `TMP` 优先于 `TEMP`，r2 评审 P1-2；专用门 Scratch 硬编码
`/tmp`，探测与回放必须同一文件系统）、临时目录文件系统接受非 UTF-8 文件名（字节透明，
否则整条 lane BLOCKED；探针把 raw `0xff` 名字节直接拼进 Buffer 路径——`path.join` 不收
Buffer 参数，先抛 `ERR_INVALID_ARG_TYPE` 就读不出文件系统判决，失败时报真实 code+message，
r2 评审增量外提醒）。流程：
no-run → 专属门 → 28 场景主回放 → regen 守卫（`generator_provenance.rs`
的 `workspace_snapshot_regenerates_identically`，#283 起 Linux 专属）→ 负控（提交前
NOT_RUN）。退出码照 `../project-files-linux-dirent/run-linux.sh`：0 = 正控过且负控全
KILLED；1 = FAILED；2 = BLOCKED（调用方环境/文件系统）；3 = BLOCKED（钉死 Node/locale 门）；
77 = 正控过但负控 NOT_RUN（**仅提交前**：以「正在运行的 run-linux.sh 自身字节 == HEAD 里
的 blob」为判据——不等才是未提交补丁在跑；已提交的 runner 再被 `mutating_sources`
拒绝说明工作树有残留/手改，判 FAILED 而非 77，提交后重跑。判据有已知盲区：补丁未提交
+残留同时存在时仍 77，77 时会打印 `git status --porcelain` 脏集合供协调者核对是否恰为本
补丁文件，见 runner 注释，r2 opus P2）。
