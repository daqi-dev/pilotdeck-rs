# #1082 同 rootDir 两代矩阵

Node 必须是 v22.22.0，`PDRS_BASELINE` 指向 cc7c4e6 只读 checkout。

```sh
PDRS_BASELINE=/abs/PilotDeck node --disable-warning=ExperimentalWarning \
  --import /abs/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-same-root-generation/generate.mts NEW_OUTPUT
```

输出目录递归建立，oracle.json 用 `wx`，work/ 为隔离 SQLite/文件树。

- `serial`：真实 repository 同根双连接互读、关闭与重开。Rust 执行相同操作，比较整组输出。
- `threaded`：真实 worker 写事务控制。当前 schema 的 repository 构造可成功；service 构造另写 workspaceDir，立即 locked。外部锁行为不被进程内串行化更改。
- `lifecycle`：12 格真实 registry lease + memory bundle/factory/service。staged/disposing × flush/retrieve × stage/first-write 8 格，以及 never predecessor、取消 retrieve/release 观察者。两代 workspace 和 rootDir 都相同。disposing 的确定性 gate 位于 registry resource owner，不冒称 native bundle 自带等待；实际 bundle.dispose 在 gate 释放后调用。LLM 使用内存离线响应，不发网络请求。

TS Promise 不可取消，取消格用 race 放弃观察者；Rust 取消等待任务，内部 continuation/cleanup 继续。前驱 gate 在新代完成前始终不释放，随后释放用于清理。gate 10 秒上限；SQL 竞争用真实 try_lock 的 WouldBlock 事件，不用 sleep 推断已交叠，5 秒未握手是 INVALID。

Rust 测试与精确层次见 `specs/gateway-parity/diffs/A6.9-MEMORY-NATIVE-PROVIDER-001-R6.md`。ignored `same_root_generates_identically` 逐字节重生成。negative.py 复用 `_shared/mutant_switch.py` 与 negative_suite，17 开关编译一次，逐项正控→变异→恢复后汇总。调用方必须设置 CARGO_TARGET_DIR；源码不改写。所有完整用例名/断言见脚本 switches 表。

`closeCallbacks`（fix-r2）在真实 repository 的 `Date.prototype.toISOString`
时钟边界同步关闭 peer，记录正常插入、抛错及不同数据库的可见结果。
TS 无 Rust `insert_l0_session_if_new` 公开 predicate；early 格只作为 Rust
提前返回的 guard 清理回归，不生成伪 TS callback 金样。Rust 回调分别覆盖 normal / early /
panic / other-root / explicit-close，连续释放两个 peer，且比较真实行与后继写入。

进程锁按当前线程识别重入；只有最外层 ProcessGuard 持有真实 mutex 并清理标记。
不同代连接仍各有 connection mutex；同一连接自身的递归调用不在本单修复范围。
`CloseContended` / `DropContended` 均由相应路径真实 try_lock 的 WouldBlock 产生；
Drop 还检查 close 完成事件及 repository Weak 失效。所有 worker join 有 5 秒上限，
超时记 INVALID；重入负控收到明确自竞争事件后立即专属断言失败，不靠超时判杀。
