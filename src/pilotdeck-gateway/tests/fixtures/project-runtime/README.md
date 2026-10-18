# ProjectRuntime generation TS 夹具

来源：固定 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的 `src/cli/ProjectRuntimeRegistry.ts`。9 个场景、59 个操作，逐项比较 generation 身份、所有保留引用的状态/lease 数、错误树和 stage/shutdown/dispose 事件序列。

生成器用共享 `../_shared/baseline.mts` 断言基线 tracked tree 干净，按 Git blob 复算 `sourceSha256` 并核对工作树。esbuild 只在内存转译原文件；通过依赖注入替换 `ProjectRuntimeResourcesBundle` 与 `BackgroundSubagentRuntime`，不改写 `buildRuntime` 或生命周期方法。`Object.create` 跳过无关 constructor 接线，仅初始化这些方法所需字段。其余业务模块 import 是空对象，意外调用会失败。每轮使用 `mkdtemp` 的独占项目根，输出文件 `wx` 创建，不覆盖已有夹具。真实资源包、session wiring、activation/recovery 回调不在本骨架范围。

复现（每次创建新的输出目录，不删除固定路径）：

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
out=$(mktemp -d "${TMPDIR:-/tmp}/pdrs-project-oracle.XXXXXX")
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/project-runtime/generate.mts "$out"
# 重新创建 out 再跑一次，比较两份 oracle.json 与本目录原件。
cd src
cargo test --locked --offline -p pilotdeck-gateway runtime::project
```

覆盖：resolve 缓存与 lexical path、invalidate 原始精确 key/全量失效、重复 lease release、retire 后拒绝 acquire、带 lease reload、stage 失败保留旧代并清理 partial 与成功候选、原始错误与清理错误聚合、失败后重试、已完成旧代错误不重报、dispose 失败结果复用、空 registry。

Rust 另外以真实并发/可控清理闸门验证 acquire/reload/invalidate/dispose 竞争、RAII lease 释放、取消 reload/release/dispose 后仍完成清理、清理失败/任务 panic 不跳过其它资源组、关闭后 reload 不与 lease 排空互锁。每次测试使用 UUID 独占临时目录。

任务明确要求的差异：TS `disposePublishedRuntimes` 会跳过仍有 lease 的 generation，`resolveSessionDrain` 从未注册；Rust dispose 等待所有 lease，包括已被 invalidate 移出当前 map 的旧代。该差异用 Rust 测试验证，不伪造 TS oracle。资源创建仍同步，遵循 TS stage 事务；资源清理异步。单代保证 shutdown 在 dispose 之前；多线程下不同 retired generation 的异步清理可交错，当前未持 lease 项目的 registry dispose 则按插入顺序逐一等待。真实 TS 顺序回放在单线程 executor 执行；多线程测试验证所有权、关闭边界、只清理一次及无遗失。
