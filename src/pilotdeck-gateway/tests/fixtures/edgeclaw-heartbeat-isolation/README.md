# #1098 Heartbeat 用例的工作区发现隔离

`edgeclaw_heartbeat_indexer.rs` 原来把 `memory` 放在系统 temp 根下的 UUID
目录里，但 general repository 从 `memory.parent.parent` 发现其他工作区。
这使不同 UUID 的测试仍会通过原生 SQLite 连接读取对方的 `control.sqlite`。
#1082 只串行化 `EdgeClawStorage` 注册的连接，不能阻止这条目录发现路径。

测试 helper 现在保留独占 owner，并把 repository 放到其 `case/` 子目录。
发现根也因此在独占 owner 内。Drop 始终删除 owner；负控退回旧布局也不会
删除共享父目录。两个既有 constructor 都使用此 helper。

`generate.mts` 用 Node v22.22.0 和固定 cc7c4e6 实际运行 `MemoryRepository`。
HOME 与所有数据库均在输出目录内，输出用 recursive mkdir + `flag:'wx'`。
金样同时记录旧/新布局的真实项目目录，以及只读 schema 事务阻止第一次 WAL
初始化的控制实验：无写事务、无 sleep、无重试；构造返回后才释放读锁。
读锁控制证明冲突机制，不能声称重放了原 CI 的确切调度。

| 状态 × 事件 × 时机 | Node 实测 | Rust 对应 |
|---|---|---|
| 旧浅层布局，peer 项目提交后，general 扫描 | 发现 peer | 编译期开关回退 `path()`，真实 catalog 断言失败 |
| 新独占扫描根，peer 项目提交后，general 扫描 | peer 自己可见，general 看不到 peer | `heartbeat_roots_do_not_discover_sibling_repository` |
| 原生只读事务持 schema 锁时首次构造 | `database is locked`，释放后成功，timeout=0 | 诊断控制，非新增产品行为；由 TS 重生成守卫复跑 |
| 原 heartbeat 的正常业务操作 | 原有金样 | 同二进制全部既有用例与其重生成守卫 |

本次没有产品生命周期或等待策略变更，前驱取消/永不完成的产品矩阵不适用。
探针的读锁在 finally 中回滚；Rust 隔离回归没有等待或后台任务。

从仓库根执行；先设调用方 `CARGO_TARGET_DIR` 和任务指定的两个 DEBUG 环境变量。
需要 Node 的命令另设只读 `PDRS_BASELINE`。所有证据目录必须新建。

```sh
node --disable-warning=ExperimentalWarning --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/edgeclaw-heartbeat-isolation/generate.mts "$NEW_OUTPUT"
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --test edgeclaw_heartbeat_indexer
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --test edgeclaw_heartbeat_indexer isolation_regenerates_identically -- --exact --ignored --nocapture
python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-heartbeat-isolation/negative.py --check
python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-heartbeat-isolation/negative.py "$NEW_NEGATIVE_OUTPUT"
```

负控使用既有 `mutants.rs` 和 `pdrs-mutants` 编译开关，仅测试 helper 启用。
先 `cargo test --no-run` 验证变异可编译，再运行正控/负控/恢复控，执行完全部
变异后汇总 KILLED/SURVIVED/INVALID；过期锚点与编译失败均非零退出。
不原地改源码，所以无需绕过任何 HEAD 守卫；记录实际工作区哈希，前后校验。
运行沿用调用方 target，不建影子仓库或第二份 target。
