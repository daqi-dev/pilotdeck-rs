# #1010 handle 丢弃侧的 TS 金样

固定 cc7c4e6、Node v22.22.0（`--expose-gc`），运行真实 `AgentHandle`。
仅 session 和 onDispose 的 release/close 门闩是确定性测试缝隙。

| TS case | 实测结果 | Rust 对应 |
|---|---|---|
| handle-drop-gc | collected=true，onDisposeCalls=0 | implicit Drop 属已登记差异 |
| abandoned-waiter | disposed，onDisposeCalls=1 | 丢等待者不重复启动清理 |
| plugin-lease-last-release | 持有最后 lease 时等待；释放后 callback 一次、Promise 幂等 | 三种 lease 活 runtime 的回归 |
| handle-activity-wait | active run 未结束时 onDispose=0，结束后=1 | activity 等待期间 shutdown 单独登记差异 |
| process-exit | draining，onDisposeRan=false | queued、dead executor 的异步段不会执行 |
| process-exit-release-pending | onDispose 已进入，release/close 完成次数 0/0 | release 永不完成时 shutdown 取消 |
| process-exit-close-pending | onDispose 已进入，release/close 完成次数 1/0 | close 永不完成时 shutdown 取消 |

TS 进程退出后无法继续执行 JS；它没有 Rust 的 Drop、writer guard 或 file/process
fence。这里仅对齐异步 cleanup 的截止状态，额外的同步释放保障登记于
`specs/gateway-parity/diffs/A7.2-SESSION-HANDLE-DROP-001-R2.md`（待复核，非评分豁免）。

`oracle.json` 记录源闭包哈希。ignored 守卫 `drop_release_regenerates_identically`
逐字节复算；`drop_release_preserves_unrelated_out_files` 验证已存在的空目录、
含工作文件目录、含 oracle 目录都在准入时 EEXIST，目录项与文件字节不变。
输出目录由 `mkdirSync(output)` 独占认领（仅父目录递归创建），且先于 baseline
加载与任何子任务；oracle 保持 `flag:'wx'`。`drop_release_preserves_late_oracle`
用 preload 在真实最终写入前制造竞争文件，验证 wx 拒绝覆盖，并清理自有 work 根。
工作目录在 finally 中只清理本次 `mkdtempSync` 创建的路径；marker 仍先写独占
临时文件再原子 rename。子进程仍验证完整 pinned source tree，650 项闭包记录仅
父进程计算一次（子进程不使用该记录），避免耗尽 15s 子进程启动预算。

handle 的未 poll fallback 现在发布 Disposed 和 executor-stopped 错误；已 poll
任务的 shutdown 取消与真正 panic 分开报告，均不冒充 hook 完成。Drop 路径恢复
poisoned progress/lease。new / 首次 push 的 runtime 前提见公共 API 的 # Panics。
三个 plugin lease 在计数递减前丢失 release 时同步发布持久错误，存活 runtime 的
dispose/release 有界失败，准入关闭；计数未伪造归零、callback 未宣称完成。
详见追加更正 -R2；正常 off-runtime Drop 仍使用捕获的 executor。

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

生成器失败后请使用新的 OUT；独占认领后的空目录不作为可复用输出。
