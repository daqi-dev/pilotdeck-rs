# Session operation ledger TS fixtures

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。`oracle.json` 保存实际读取源码的 SHA-256、输入以及 TS reader/runtime/ledger 的恢复输出；JSONL 由真实 `SessionAgentLoopOperationLedger` → `JsonlTranscriptWriter` → `JsonlSessionPersistence` 生成，无实现源码复制。

在仓库根目录，Node 22（本次 v22.22.0）执行：

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/operation-ledger/generate.mts src/pilotdeck-gateway/tests/fixtures/operation-ledger
```

| 文件 | 来源与受控变更 |
|---|---|
| normal | 真实 start → accept → terminal，固定时钟和 entry ID；包含 UTF-16 孤立代理、键序、seed projection |
| unknown | 真实 start → accept → resultUnknown |
| unknown-nonfinite | TS unknown 不验证 sequence；NaN 被 JSON.stringify 写为 null，恢复保留 null |
| seed-order | 合法 Map 值键序 kind/mtimeMs 原样持久化，clone/restore 变为 mtimeMs/kind；真实 TS 第二次同输入 terminal 报冲突 |
| incomplete | 真实 start → accept，无终态 |
| bad-tail | normal 后添加无效 JSON 行及无效 envelope |
| partial | incomplete 后添加无换行的半写 terminal 字节；不能恢复为成功 |
| partial-continued | 真实 TS writer 对 partial 续写 unknown：保留碎屑并补换行 |
| bad-middle | normal 第一行后插入坏行；后续正常 terminal 仍可恢复 |
| legacy | normal 删除可选 entryId/parentEntryId，追加非 ledger、另一 session 的旧式 metadata 行（sequence=9）；用真实 TS reader/runtime/ledger 重新判定 |
| known-then-unknown | 拼接两次真实 TS ledger 生成的 terminal，修正第二条 envelope 序号/ID；验证恢复可降为 unknown，与在线 known 后 unknown no-op 不同 |

生成器和跨语言测试统一从 `PDRS_BASELINE` 读取基线路径。`generate.mts <rust-jsonl> --verify-rust` 用真实 TS reader、SessionRuntime.restore、ledger.recover 读取 Rust 文件，核对终态与链，并用真实 TS writer 继续写 sequence=5。由 Rust 忽略测试调用：

```sh
cd src
cargo test -p pilotdeck-gateway real_ts_reads_rust_and_continues_its_transcript -- --ignored --nocapture
```

默认 Cargo 测试不依赖 Node：除读取 TS 夹具恢复外，还将 Rust 写出的 normal/unknown 原始行屏蔽 `entryId`/`createdAt` 后与 TS 金样逐字节比较（保留键序、空白和转义）；`parentEntryId` 先核验链，再随 entry ID 映射。`oracle.json` 的 `readLimits` 来自真实 TS reader 读取 50 MiB、50 MiB + 1 字节的稀疏文件，默认测试对拍诊断 code/severity/line，并验证超限时 `open` 拒绝把空投影当新 session；大文件只在临时目录生成，不入库。跨语言测试是显式自证项，执行记录见任务报告。坏 JSON 的诊断 code/severity/line 与 TS 比较；具体 JSON 解析错误文本随 Rust parser，不假称 V8 文案相同。

本 PR 的恢复校验只覆盖事件日志层（`validateSessionEventLog` 部分）；领域校验（`SessionRuntime.ts:109` 的 `validateSessionDomainLog`）留给接入 host/runtime 的后续 PR，并作为该接入 PR 的前置事项。这里证明事件日志与 ledger 存储契约，不代表完整 session 领域恢复、gateway method 本地化或 host execute 路径已接入。

Rust 文件适配的锁和原子替换是实现细节：TS 默认 JSONL writer 没有锁协议；两种 writer 的切换必须先停止旧持有人，不支持 TS/Rust 同时写同一文件。TS transcript sequence 是提交序号，磁盘不新增 revision 字段。整个 session transcript 历史事务、RunRegistry 和投影 checkpoint 不在此 PR 范围。
