# 会话域校验真实 TS 金样

基线固定为 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。入口为 `src/session/events/SessionDomainValidation.ts` 的 `validateSessionDomainLog` 和 `validateSessionDomainEvent`。生成器调用共享 baseline 守卫，核对 HEAD、已跟踪改动及源码 SHA-256；预期值只来自真实 TS 执行。`cases.mts` 只定义输入及末条分支断言，不生成错误文案。

```sh
PDRS_BASELINE=/absolute/PilotDeck timeout 120 node --import /absolute/PilotDeck/node_modules/tsx/dist/loader.mjs src/pilotdeck-gateway/tests/fixtures/session-domain/generate.mts src/pilotdeck-gateway/tests/fixtures/session-domain
```

304 个用例覆盖全部域类型、24 种域错误码、JS TypeError 文案、合法生命周期、顺序/重复/缺失、取消与丢弃、FIFO、历史 turn_result、权限与模型请求、全局 goal revision、计划/todo 元数据，以及非字符串 turnId。单事件失败后保持原状态；全日志校验与逐事件结果均逐字节比较。其中 303 个事件对象输入组也在 transcript-adapter 的真实存储适配器金样里重放，核对步骤结果、store 调用与磁盘字节；原始 null 仅在域校验函数入口测试。

```sh
cd src
cargo test -p pilotdeck-gateway --lib storage::session_domain::
cargo test -p pilotdeck-gateway --lib storage::transcript_adapter_tests::
```

负控：从仓库根执行 `python3 src/pilotdeck-gateway/tests/fixtures/session-domain/negative.py <新的输出目录> [变异名 ...]`，必须设置本树的 `CARGO_TARGET_DIR`，脚本不另建 target；`--compile-only` 先逐项验证变异可编译。脚本构造有界 rustc 探针，直接包含产品状态机与金样测试；依赖函数从产品源码逐字提取，避免每条负控重编整个 gateway。每次修改真正产品源码，逐项恢复并汇总 KILLED / SURVIVED / INVALID；任一非 KILLED 即非零退出。编译失败、零测试、超时和非绑定断言失败均不算检出。全 gateway 的 adapter 负控另验证恢复旧 Node 兜底及投影拒绝路径，原始输出见本单 RESULT。

范围：校验与写入；完整 A7.5 reducer、checkpoint 及投影读取 API 未在本单实现。

#743 fix-r3：轻量探针在 baseline 前使用调用方 `CARGO`（未设置则 PATH cargo）查询实际 sysroot，核对其 rustc 与 `RUSTC`（未设置则 cargo 同目录 rustc）的 `-V` 输出；不一致即 INVALID。随后用同一 Cargo 在调用方 target 中执行 gateway `--lib --no-run`，只从本次 JSON `compiler-artifact` 选取 externs，不按 rlib 修改时间猜测。版本查询、依赖构建、每项编译和执行均独立留原始日志；工具链错误不会计为 KILLED。`--compile-only` 对每个变异同时执行轻量 rustc 编译和 `cargo check --tests`，不执行变异测试；全量较长时交协调者串行执行。fix2 的临时源码副本同时携带根 `rust-toolchain.toml`。

修复轮 2（#562）：生成器与 Rust 测试均精确断言 304 场景；增删场景时须同步修改两处计数与本 README，防止金样再生或合并时覆盖静默缩水。每个场景还先断言 entries 与 outcomes 等长，再逐条比较，避免 zip 截断。

修复轮 2 负控：从仓根执行 `CARGO_TARGET_DIR="$PWD/src/target" timeout 600 python3 src/pilotdeck-gateway/tests/fixtures/session-domain/fix2-negative.py`，仍只使用启动器的本树 target。两份 oracle 复制到 `/tmp`，分别通过仅测试可见的 `PDRS_TRANSCRIPT_ADAPTER_ORACLE`、`PDRS_SESSION_DOMAIN_ORACLE` 加载。依次删去各 10 个场景、截短单例 outcomes；状态安装退回 hydrate 之后的源码变异也只写 `/tmp` 的源码副本；副本使用独有包版本 `0.1.0-pdrs222-negative`，在共用 target 时隔离变异产物与原树构建身份。各项必须运行对应测试且因预期断言失败，前后完整金样测试必须全绿。脚本不改工作树、不在本树 `_tmp` 建目录；正常结束、异常和 SIGTERM 均删除临时目录，SIGKILL 只可能留下 `/tmp` 副本。每个子进程限时 240 s、整套父进程限时 580 s，串行运行且最多 4 个测试线程，原始输出打印至 stdout。

修复轮 6：Rust 金样消费统一经 `test_baseline::oracle` 校验固定 commit 与 sourceSha256；`PDRS_SESSION_DOMAIN_ORACLE` 副本也走同一入口。运行时设置 `PDRS_BASELINE` 才会将合法格式的哈希与固定基线源码逐字节核对（未设置时仍检查 commit、路径和哈希格式）。`fix2-negative.py` 新增副本 sourceSha256 / sourceCommit 篡改负控，必须设置 `PDRS_BASELINE`；`negative_oracle_guard.py` 同步登记 session-domain、transcript-adapter、run-registry。轻量 `negative.py` 包含真实共享校验模块，保持与 gateway 消费路径一致。
