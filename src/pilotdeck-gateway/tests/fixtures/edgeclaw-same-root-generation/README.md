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

Rust 测试与精确层次见 `specs/gateway-parity/diffs/A6.9-MEMORY-NATIVE-PROVIDER-001-R5.md`。ignored `same_root_generates_identically` 逐字节重生成。negative.py 复用 `_shared/mutant_switch.py` 与 negative_suite，13 开关编译一次，逐项正控→变异→恢复后汇总。调用方必须设置 CARGO_TARGET_DIR；源码不改写。所有完整用例名/断言见脚本 switches 表。
