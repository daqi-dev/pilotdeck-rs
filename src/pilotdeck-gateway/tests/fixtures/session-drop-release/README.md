# #1010 handle 丢弃侧的 TS 金样

固定 cc7c4e6、Node v22.22.0（`--expose-gc`），运行真实 `AgentHandle`。
仅 session 和 onDispose 的 release/close 门闩是确定性测试缝隙。

| TS case | 实测结果 | Rust 对应 |
|---|---|---|
| handle-drop-gc | collected=true，onDisposeCalls=0 | implicit Drop 属已登记差异 |
| abandoned-waiter | disposed，onDisposeCalls=1 | 丢等待者不重复启动清理 |
| process-exit | draining，onDisposeRan=false | queued、dead executor 的异步段不会执行 |
| process-exit-release-pending | onDispose 已进入，release/close 完成次数 0/0 | release 永不完成时 shutdown 取消 |
| process-exit-close-pending | onDispose 已进入，release/close 完成次数 1/0 | close 永不完成时 shutdown 取消 |

TS 进程退出后无法继续执行 JS；它没有 Rust 的 Drop、writer guard 或 file/process
fence。这里仅对齐异步 cleanup 的截止状态，额外的同步释放保障登记于
`specs/gateway-parity/diffs/A7.2-SESSION-HANDLE-DROP-001.md`（待复核，非评分豁免）。

`oracle.json` 记录源闭包哈希。ignored 守卫 `drop_release_regenerates_identically`
逐字节复算；`drop_release_preserves_unrelated_out_files` 实跑两次，核已有
`work-process-exit/keep.txt` 保留、已有 oracle 拒绝覆盖。
输出目录递归创建；oracle 使用 `flag:'wx'`；工作目录只清理本次 `mkdtempSync`
独占创建的路径；子进程 marker 先写独占临时文件再原子 rename。

Rust `factory_tests.rs` 的六个 `*_owned_session_*shutdown*` 用例使用真实
ProjectSessionFactory/OwnedSession。current-thread runtime 仅显式驱动到门闩；
在 plain thread drop 最后句柄。Q/R/C/D 四格加 PendingSession Q/D 两格都检查
lease 归零、hook 次数、旧 writer 拒写、process/file fence、取消 waiter 不解锁、
产品 BUSY 有界，以及放开 close gate 后 successor 能取得 writer。
这里的取消对象是 create waiter，后台 construct 仍按原契约运行。

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
node --disable-warning=ExperimentalWarning --expose-gc \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/session-drop-release/generate.mts OUT
```

负控清单以 `negative_controls.py` 的 cases 为唯一正本。`--audit` 只读核锚点；
`--compile OUT` 对每项先 no-run；`OUT [name...]` 跑正控、编译变异、负控、恢复。
源码变异必须在协调者提交本补丁后跑，未提交树由共享 guard 拒绝；长套件在宿主跑。
必须设置调用方的 `CARGO_TARGET_DIR`，禁止新建第二构建目录。每项独立记
KILLED / SURVIVED / INVALID，全部运行后汇总；无关 panic、编译失败、超时不计杀。
