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

负控（#996 起为编译期开关，旧 `negative.py` 文本替换表与 #743 fix-r3 的独立 rustc 工具链已删除）：从仓库根执行 `python3 src/pilotdeck-gateway/tests/fixtures/session-domain/schemata.py <新的输出目录> [开关名 ...]`，必须设置本树的 `CARGO_TARGET_DIR`，脚本不另建 target；`--check` 只做锚点预检。49 个开关以 `crate::mutants::on("<名>")` 分支编译进 feature 库测试二进制：先 `cargo test --lib --features pdrs-mutants --no-run` 预编译一次，再逐开关以 `PDRS_MUTANT=<名>` 独立进程运行金样测试。执行器共享 `tests/fixtures/_shared/mutant_switch.py`，三相（positive / negative / restored）、KILLED 证据为具名测试 FAILED + panicked + `#743 domain transition outcome` / `#743 domain log outcome` 断言锚点；任一非 KILLED 即非零退出，全程不改工作区源码。契约见 `specs/negative-controls/README.md`。全 gateway 的 adapter 负控另验证恢复旧 Node 兜底及投影拒绝路径，原始输出见本单 RESULT。

范围：校验与写入；完整 A7.5 reducer、checkpoint 及投影读取 API 未在本单实现。

修复轮 2（#562）：生成器与 Rust 测试均精确断言 304 场景；增删场景时须同步修改两处计数与本 README，防止金样再生或合并时覆盖静默缩水。每个场景还先断言 entries 与 outcomes 等长，再逐条比较，避免 zip 截断。

修复轮 2 负控：从仓根执行 `CARGO_TARGET_DIR="$PWD/src/target" timeout 600 python3 src/pilotdeck-gateway/tests/fixtures/session-domain/fix2-negative.py`，仍只使用启动器的本树 target。两份 oracle 复制到 `/tmp`，分别通过仅测试可见的 `PDRS_TRANSCRIPT_ADAPTER_ORACLE`、`PDRS_SESSION_DOMAIN_ORACLE` 加载。依次删去各 10 个场景、截短单例 outcomes；状态安装退回 hydrate 之后的源码变异也只写 `/tmp` 的源码副本；副本使用独有包版本 `0.1.0-pdrs222-negative`，在共用 target 时隔离变异产物与原树构建身份。各项必须运行对应测试且因预期断言失败，前后完整金样测试必须全绿。脚本不改工作树、不在本树 `_tmp` 建目录；正常结束、异常和 SIGTERM 均删除临时目录，SIGKILL 只可能留下 `/tmp` 副本。每个子进程限时 240 s、整套父进程限时 580 s，串行运行且最多 4 个测试线程，原始输出打印至 stdout。

修复轮 6：Rust 金样消费统一经 `test_baseline::oracle` 校验固定 commit 与 sourceSha256；`PDRS_SESSION_DOMAIN_ORACLE` 副本也走同一入口。运行时设置 `PDRS_BASELINE` 才会将合法格式的哈希与固定基线源码逐字节核对（未设置时仍检查 commit、路径和哈希格式）。`fix2-negative.py` 新增副本 sourceSha256 / sourceCommit 篡改负控，必须设置 `PDRS_BASELINE`；`negative_oracle_guard.py` 同步登记 session-domain、transcript-adapter、run-registry。轻量探针曾包含真实共享校验模块保持与 gateway 消费路径一致；#996 迁移后金样校验只在真实 crate 内编译（负控走编译期开关），该独立工具链随之删除。
