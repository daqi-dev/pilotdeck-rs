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

负控：从仓库根执行 `python3 src/pilotdeck-gateway/tests/fixtures/session-domain/negative.py`，要求 `CARGO_TARGET_DIR` 恰为本树 `src/target`。脚本构造有界 rustc 探针，直接包含产品状态机与金样测试；依赖函数从产品源码逐字提取，避免每条负控重编整个 gateway。每次修改真正产品源码，执行失败后恢复；编译失败、零测试和超时均不算检出。全 gateway 的 adapter 负控另验证恢复旧 Node 兜底及投影拒绝路径，原始输出见本单 RESULT。

范围：校验与写入；完整 A7.5 reducer、checkpoint 及投影读取 API 未在本单实现。
