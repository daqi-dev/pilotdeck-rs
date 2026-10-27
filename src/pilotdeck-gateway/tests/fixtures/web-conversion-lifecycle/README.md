# Web conversion lifecycle — #940

修复的是 detached worker 的测试终态等待。产品 `runtime/model/cpu.rs` 与
`web/html_memory.rs` 已在工作闭包内持有许可，本单不改它们。

`web_tools::html_conversion_admission_survives_dropped_requests` 使用两阶段 Condvar：
先阻塞四个 worker 并 abort 调用方；stage 1 仅允许一个原 worker 结束，四个排队
请求顺序复用该槽；此时确定性断言 active=3。current-thread Tokio 的清理任务
在 drain 首次 yield 后放行其余 worker；10 秒内必须 active=0，peak 必须仍为 4。
worker 自身等待上限 20 秒，失败或提前退出时 ReleaseOnDrop 放行全部 worker。
这覆盖了取消调用方、前驱暂不结束、单槽连续复用、最终 drain 四种时序。

TS 金样通过固定基线的真实 web_fetch + turndown 生成，HTTP 字节由已有测试钩子注入，
不访问网络。包装真实 turndown 的入口/出口，在入口注入 parent abort；正常与 abort
两格均先 conversion-exit 后 result，完整 raw 结果由 Rust 既有测试逐字段回放。
TS 没有 spawn_blocking 或四槽准入，detached worker 的容量格属于 Rust 策略，不声称
与 TS 并发实现相同。生成器使用 Node v22.22.0、基线 provenance 守卫、递归建目录与
wx 独占写入；忽略测试 `web_conversion_lifecycle_regenerates_identically` 验证逐字节重生成。

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
# 在本树按任务指定设置 CARGO_TARGET_DIR 和两个 debug profile 环境变量。
scripts/cargo-slots.sh
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway \
  --test web_tools web_conversion_lifecycle_regenerates_identically -- --exact --ignored --nocapture
python3 src/pilotdeck-gateway/tests/fixtures/web-conversion-lifecycle/negative.py <新证据目录>
```

负控复用 negative_process 的源码守卫/进程所有权和 negative_suite 的精确断言分类。
每项先正控，再应用变异、no-run 编译、负控、恢复正控。八项跑完汇总，任一
SURVIVED / INVALID 即非零；旧锚点是 INVALID，编译失败不算 KILLED。
CARGO_TARGET_DIR 必须由调用方传入，不创建第二构建目录、不覆盖 dirty 源码。

| 变异 | 必须失败的证据 |
|---|---|
| early-permit-release | #940 a fifth worker bypassed conversion admission |
| fifth-slot | 同上 |
| skip-detached-drain | #940 final active workers |
| active-never-drains | #940 detached workers must drain within the deadline |
| raw-result-oracle | #940 real TS raw conversion result |
| missing-cases | #940 normal and abort cases must both exist |
| missing-abort-case | #940 lifecycle cases must be normal then abort |
| regeneration | oracle.json differs from the regenerated |

源码须等于 HEAD；提交前新增金样/生成器或已修改测试的六项会被守卫拒绝，须提交后在宿主执行。
