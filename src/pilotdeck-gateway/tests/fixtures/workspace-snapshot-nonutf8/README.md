# WorkspaceSnapshot non-utf8 负控装置（#283）

本目录是 `workspace-snapshot` 金样 non-utf8 场景的**负控与 Linux 验证装置**，不是独立金样：金样本体仍在
`../workspace-snapshot/`（28 场景版 `oracle.json`，SHA-256
`1c93b732a818dabdb49c736119e07921f1dadf4fb760c7d45f4a8d986b0828b1`，2026-11-01 于 cb8/ext4 生成，
溯源登记见 `../workspace-snapshot/README.md`）。产品行为由
`src/pilotdeck-gateway/src/storage/workspace_snapshot.rs` 的 `scan_visit` 实现
（`to_string_lossy` → 重编码 lstat → `io_message`（measured node_io 表）→ 失败落 `_FAILED.json` marker）。

## 被钉住的测试

`tests/workspace_snapshot.rs` 的 `non_utf8_capture_fails_with_lossy_lstat_enoent_on_linux`
（`#[cfg(target_os = "linux")]`）：从提交金样取 non-utf8 场景，raw 字节自检（`read_dir` 按
`.as_bytes()` 对意图，不依赖被测的解码），跑 Rust recorder capture，逐字段对金样断言
（snapshotId / state / phase / marker 路径 / error 文本 / marker 投影后 sha256+size / 无 manifest），
全部断言带 `#283` 专属消息——负控只认这些锚点。另断言 `non_utf8_names_writable` 为真：若覆盖在
Linux 上被静默跳过（文件系统不再接受该字节），该门响亮失败而不是让 27 场景的回放保持绿色。非
Linux 平台上的配对测试 `non_utf8_scenario_stays_skipped_where_raw_names_cannot_exist` 断言探测
必须为假（跳过即文档化行为）且金样仍含该场景名。

## 变异表（4 个，全绑 #283 专属断言）

| 变异 | 文件 | 改法 | 杀点（dedicated 断言） |
|---|---|---|---|
| decode-strip-fffd | 产品 storage | `scan_visit` 的 lossy 名 `.replace('\u{FFFD}',"")` | error 文本断言（`bad-.txt` ≠ `bad-<fffd>.txt`：lstat 目标被静默改名） |
| lstat-rust-display | 产品 storage | lstat 的 `io_message(...)` → `e.to_string()` | error 文本断言（Rust Display ≠ Node 文案） |
| lstat-skip-entry | 产品 storage | lstat `map_err` → `let Ok/else continue`（静默跳过坏名） | state 断言（committed ≠ failed：capture 悄悄少了坏名条目） |
| oracle-error-drift | 金样 oracle.json | non-utf8 error 文本 `lstat`→`stat` | error 文本断言（证明金样真驱动比较，不是自洽回声） |

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
`LC_*` 覆盖、临时目录文件系统接受非 UTF-8 文件名（字节透明，否则整条 lane BLOCKED）。流程：
no-run → 专属门 → 28 场景主回放 → regen 守卫（`generator_provenance.rs`
的 `workspace_snapshot_regenerates_identically`，#283 起 Linux 专属）→ 负控（提交前
NOT_RUN）。退出码照 `../project-files-linux-dirent/run-linux.sh`：0 = 正控过且负控全
KILLED；1 = FAILED；2 = BLOCKED（调用方环境/文件系统）；3 = BLOCKED（钉死 Node 门）；
77 = 正控过但负控 NOT_RUN（提交后重跑）。
