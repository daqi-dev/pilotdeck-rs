# FailureGuard / classify 真实 TS 金样

`generate.mts` 直接 import 固定 `cc7c4e6` 的 `FailureGuard.ts` 和 `classify.ts`，共享 `assertBaseline` / `baselineSourceHashes` 核验 HEAD、tracked 工作区和已提交源码 SHA-256。没有复制实现，也没有手写期望。Node v22，所有输出写指定目录；不修改基线与 node_modules。

```sh
PDRS_BASELINE=/absolute/PilotDeck node --import /absolute/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/failure-guard/generate.mts /tmp/failure-guard-generated
```

9 个场景、58 个动作：正常累计、跨阈值 trip、成功不重置、重建 guard 重置、trip 锁存、模型/工具独立计数、零阈值、错误码分类、精确工具名优先、最长 `__` 前缀、共享子代理计数、own `__proto__`、缺标签/空标签和孤立 UTF-16 标签/模型。每次 trip 保存六种模型元数据组合下的真实文案；另存分类函数的直接边界输入/输出。

Rust 回放：`runtime::turn::failure_guard::tests::real_ts_failure_guard_and_classify`；ignored 重生成守卫：`real_ts_failure_guard_regenerates_identically`。完整 submit 对拍在相邻 `turn-submit` 夹具中；guard 生命周期属于单次 pump，依靠 completion 与 timeout 约束观察权限，在任何出口释放，不共享跨轮计数器。

```sh
CARGO_TARGET_DIR="$PWD/_build/cargo-target" PDRS_BASELINE=/absolute/PilotDeck \
  python3 src/pilotdeck-gateway/tests/fixtures/failure-guard/negative.py \
  _tmp/20261023-a72-pr4b-failure-guard/negative
```

独占运行，脚本临时修改产品 guard/submit，要求不变的真实 TS 金样出现指定断言失败；编译失败、信号、超时、空筛选都不算有效负控。脚本核验唯一构建目录为本树 `_build/cargo-target`，保存变异、原始输出、SHA-256 与恢复证明；每个子进程组最多 240 秒、整轮最多 30 分钟。可追加精确负控名以有界重跑。不要与另一源码写入/构建并发运行。
