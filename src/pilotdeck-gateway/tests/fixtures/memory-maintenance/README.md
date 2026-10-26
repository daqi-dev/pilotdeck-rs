# Memory maintenance 调度金样

固定来源 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，
Node **v22.22.0**。生成器直接 import 原始 `ProjectMemoryMaintenanceController`、
`GatewayRuntimeRefreshBundle` 和 `EdgeClawMemoryService`；源文件 SHA-256 记录在
`oracle.json` 的 `sourceSha256`，前后 `bindSourceTree` 检查固定 HEAD 与 tracked clean。
时钟、随机数固定，HOME 使用独立临时目录，不读用户配置、不写基线。

## 状态 × 事件 × 时机

|金样格|状态及触发时机|观察 / Rust 对应|
|---|---|---|
|idle|空闲时 schedule|service 首调用早于返回；完成后 success|
|coalesce|await 中同代际重复 schedule|只补一轮|
|settling|循环耗尽到 finally 的窗口 schedule|同代际续跑；TS microtask 钩子断言真实状态|
|reject / reject_requested|await 中拒绝，无 / 有后继请求|trackError + diagnostic；有请求则续轮|
|generations|旧代际 pending 时发布新代际|同 root 的两个对象独立执行|
|disposed|旧代际 pending 时 close 真实 service，再续轮|TS 返回 Error / database is not open / ERR_INVALID_STATE；**Rust 真实 service 尚缺，不作对齐声明**|
|absent|没有 memory service|只有 resolve，无维护事件|
|resolver_error|已有 flight 时 resolver 抛错|schedule 同步抛；refresh Promise 拒绝|
|never|A 前驱永不完成，再 schedule A/B|不等待 A，B 可独立完成；Rust 另验宿主关闭释放 flight|
|cancelled|await 中以 AbortError 拒绝|正常错误报告后续轮|
|before_await|首调用立即完成，返回后追加请求|不重 poll 已完成 future，仍追加一轮|
|panic_after_await|await gate 放行后抛 Error，无后继|错误遥测与诊断，最终清 flight|
|panic_requested|await 后抛 Error，已排队请求|报告后继续旧代际下一轮|
|panic_retry|await 后抛 Error，收尾后再 schedule|重新调用并成功|
|panic_string / panic_opaque|await 后抛 Error|Rust 分别使用 String / 不透明 panic payload；后者约定 fallback message，TS 只确认同形 Error 的调度行为|

17 个 TS 格，16 个 Rust oracle 重放；另有跨线程/首 poll unwind 与真实 Tokio child.abort 测试。
后者覆盖 JoinError::cancelled 后 flight 清理与再次 schedule，不映射为 service AbortError；
TS controller 无取消 API，此格是 Rust 宿主生命周期专用检查。
`disposed` 仅保留真实 TS 观测；Rust 没有真实 NativeMemoryService 实现，已删除
从 oracle 反造错误的测试。等真实 service 移植后，再补关闭后的 name/message/code 对拍。

等待均有界：TS 每次只推进一个 setImmediate，然后检查事件与 pending；
Rust current-thread 每次最多 16 个 yield 后断言完整事件和状态。never 不等 A 完成，
关闭测试 runtime 后断言 flight 和旧代际释放。多线程用例在可控 sender 放行后
等待两轮完成，总上限 2 秒，不用 sleep 猜竞态。

## 复采与重生成守卫

从仓库根执行，输出使用新目录（生成器 mkdir `recursive:true`，文件 `flag:'wx'`）：

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
export CARGO_TARGET_DIR="$PWD/_build/cargo-target"
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/memory-maintenance/generate.mts /tmp/memory-maintenance-new
cmp /tmp/memory-maintenance-new/oracle.json src/pilotdeck-gateway/tests/fixtures/memory-maintenance/oracle.json
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/memory-maintenance/verify.mts
bash scripts/cargo-slots.sh
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib runtime::memory::maintenance
bash scripts/cargo-slots.sh
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway \
  --test edgeclaw_memory_maintenance -- --include-ignored
```

最后一条实际执行 `#[ignore]` 的 `regenerates_identically("memory-maintenance")`，
不是只读金样；重新 import TS，逐字节比较 oracle。

## 负控

独占本 worktree，不与任何编译或写入并发；输出目录必须不存在：

```sh
python3 src/pilotdeck-gateway/tests/fixtures/memory-maintenance/negative_controls.py \
  _tmp/memory-maintenance-negatives-new
```

第二个可选参数是已核验的 Git tree/commit（默认 HEAD）；第三个起为变异名称，
例如 `HEAD panic-swallowed panic-lost-request`。未提交修复可用独立临时 index/objects
生成 tree 作为基线，不修改真实 index，不把脏源码绕过校验。脚本使用共享
`mutating_sources` / `run_owned` / `wait_for_slot`；每条只变异一处，必须命中指定测试的
运行期 FAILED（exit 101），编译失败、信号退出或零测试均不算杀死。
每条保存 cargo 与 slots 原始日志，`summary.json` 汇总；恢复源码后重跑维护套件及
受变异的真实生产句柄用例，必须通过。单次 cargo 限 240 秒。
