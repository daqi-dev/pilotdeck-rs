# Session operation ledger TS fixtures

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。`oracle.json` 保存实际读取源码的 SHA-256、输入以及 TS reader/runtime/ledger 的恢复输出；JSONL 由真实 `SessionAgentLoopOperationLedger` → `JsonlTranscriptWriter` → `JsonlSessionPersistence` 生成，无实现源码复制。

在仓库根目录，用已有的 Node **v22.22.0**、基线 lockfile 中的 **tsx 4.22.3** 执行。获取固定源码可用下列命令（已有基线时不重复 clone/checkout，不改它）：

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
git clone --no-checkout https://github.com/OpenBMB/PilotDeck.git "$PDRS_BASELINE"
git -C "$PDRS_BASELINE" checkout --detach cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85
```

基线依赖由其 `pnpm-lock.yaml` 锁定，准备环境时使用 `pnpm install --frozen-lockfile`；不使用 `npx` 临时拉最新版。生成前核对实际运行器：

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
test "$(node --version)" = v22.22.0
node -e 'const assert = require("node:assert/strict"); assert.equal(require(process.env.PDRS_BASELINE + "/node_modules/tsx/package.json").version, "4.22.3")'
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/operation-ledger/generate.mts src/pilotdeck-gateway/tests/fixtures/operation-ledger
```

生成器在创建输出前强制断言基线 HEAD 等于上面的完整 commit；漂移时直接失败，不能静默刷新 oracle 的来源。

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
| identity-{accepted,terminal}-* | 修改真实 TS 行的 request/run/turn/key/binding 共八种字段或有无状态；真实 TS ledger 全部拒绝，错误原件在 oracle.rejectedCases |
| seed-{invalid-kind,string-mtime,invalid-write} | 真实 TS terminal 先写盘后 clone/parse 抛错，重开也拒绝；Rust 的写前拒绝时机差异仍归 #201 |
| seed-{null-fields,falsy-fields,optional-fields} | 真实 TS 可写可读；顶层 null/false/0/空串被序列化省略，Map 值原样写盘、恢复时丢可选字段的错误类型及额外键 |
| invalid-utf8 | 真实 TS reader 将 0xff 解码为 U+FFFD，createdAt 非日期字符串仍可恢复；Rust 续写保留原始非法字节 |

生成器和跨语言测试统一从 `PDRS_BASELINE` 读取基线路径。`generate.mts <rust-jsonl> --verify-rust` 用真实 TS reader、SessionRuntime.restore、ledger.recover 读取 Rust 文件，核对终态与链，并用真实 TS writer 继续写 sequence=5。由 Rust 忽略测试调用：

```sh
cd src
cargo test -p pilotdeck-gateway real_ts_reads_rust_and_continues_its_transcript -- --ignored --nocapture
```

完整 storage 自证（显式含上述实时 TS 交叉测试）在 `src/` 执行 `PDRS_BASELINE=/absolute/path/to/PilotDeck cargo test -p pilotdeck-gateway storage:: -- --include-ignored`。缺少变量或基线时明确失败，不能作为“跳过后的通过”；本轮可复现命令及原始输出索引在 `_tmp/20261017-a62/FIX2.md`。默认 CI 仍运行静态 TS 金样及拒绝夹具，自动拉起实时基线交叉测试转跟进，前置于 A6.2 整包关闭。

默认 Cargo 测试不依赖 Node：除读取 TS 夹具恢复外，还将 Rust 写出的 normal/unknown 原始行屏蔽 `entryId`/`createdAt` 后与 TS 金样逐字节比较（保留键序、空白和转义）；`parentEntryId` 先核验链，再随 entry ID 映射。`oracle.json` 的 `readLimits` 来自真实 TS reader 读取 50 MiB、50 MiB + 1 字节的稀疏文件，默认测试对拍诊断 code/severity/line，并验证超限时 `open` 拒绝把空投影当新 session；大文件只在临时目录生成，不入库。跨语言测试是显式自证项，执行记录见任务报告。坏 JSON 的诊断 code/severity/line 与 TS 比较；具体 JSON 解析错误文本随 Rust parser，不假称 V8 文案相同。

本 PR 的恢复校验只覆盖事件日志层（`validateSessionEventLog` 部分）；领域校验（`SessionRuntime.ts:109` 的 `validateSessionDomainLog`）留给接入 host/runtime 的后续 PR，并作为该接入 PR 的前置事项。这里证明事件日志与 ledger 存储契约，不代表完整 session 领域恢复、gateway method 本地化或 host execute 路径已接入。

Rust 文件适配的锁和原子替换是实现细节：TS 默认 JSONL writer 没有锁协议；两种 writer 的切换必须先停止旧持有人，不支持 TS/Rust 同时写同一文件。硬链接别名不受支持：锁按 canonical 路径区分，rename 会使两个名字变成分叉历史，不能把它们当作同一 session。TS transcript sequence 是提交序号，磁盘不新增 revision 字段。

原子替换只保证运行期 rename 的可见性，不承诺掉电后的目录项持久性。TS `JsonlSessionPersistence.ts:24` 只给 compact boundary 设置 flush，普通 ledger 写入没有目录 fsync；Rust 临时文件 sync 后 rename，同样不 fsync 父目录。写入 IO 失败、外部字节变化或提交结果不确定后，返回 `LedgerError::ReopenRequired`；调用方必须 drop ledger 再 open，从磁盘重建状态。旧实例持续拒绝读回、对账和幂等 no-op，不能据旧内存自行重试。整个 session transcript 历史事务、RunRegistry 和投影 checkpoint 不在此 PR 范围。
