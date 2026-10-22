# EdgeClaw repository accessors (A6.6-pr10c)

真相源为 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的
`src/context/memory/edgeclaw-memory-core/src/core/storage/sqlite.ts:592-909`；
模式及外部标识规则见 `core/general-projects.ts:15-34`，仓库初始化见
`core/storage/sqlite.ts:502-540`。`generate.mts` 通过 `bindSourceTree` 和
`sourceClosureSha256` 校验固定基线及其完整源码依赖，随后调用真实 TS。
没有用 Rust 产生期望值。

金样包含六个 TS 创建的 SQLite 数据库（base64）、TS 写出的初始文件树、
可在 Rust 侧重放的写入操作和 81 个真实 TS 观测值。`seedFiles` 只记录文件，
空目录由仓库构造器建立；损坏外部库的输入另行记录。时钟固定为
`2026-10-22T00:00:00.000Z`。外部 workspace 标识用稳定的 `/fixture/*` 字符串；
临时 HOME 在导入 TS 前注入，绝不使用真实 `~/.pilotdeck`。
初始数据库不包含临时目录字符串；JSON 中的临时根路径映射为 `$ROOT`。

覆盖：close/重复 close、关闭后 SQL 与文件 accessor、accessor 对象身份；
默认目录、single/general 模式及重开切换；空目录、普通文件、损坏库、
同 workspace 排除、general workspace 排除、外部 workspace 路径变化、目录移除；
本地/外部/镜像目录、孤立镜像、摘要合并、最新时间及 ICU 名称排序；
project/feedback/deprecated/query/includeExternal 过滤和外部 ID；
project kinds 的 `new Set(...)` 语义（数组过滤、字符串按字符迭代得空集、
`null`/`undefined` 取默认、数字/布尔/对象抛 TypeError；external-only 项目在
`includeExternal:false` 时不触达 store）；includeExternal 的 `0`/`""`/`1`/`null` 真值语义；
project 条目忽略调用方 limit/offset（固定 5000/0）；global 入参白名单
（多余 projectId/includeTmp 不透传）、非数组 kinds 的 TypeError、空/缺省 kinds；
global user 过滤、分页、搜索、summary 路径前缀；损坏 manifest 的 repair 与重复 repair。

生成两次并核字节（从仓库根目录运行）：

```sh
export PDRS_BASELINE=/absolute/path/to/fixed/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-accessors/generate.mts /tmp/accessors-run1
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-accessors/generate.mts /tmp/accessors-run2
cmp /tmp/accessors-run1/oracle.json /tmp/accessors-run2/oracle.json
```

`edgeclaw_accessors.rs` 同时验证 TS `.db`/文件树 → Rust、Rust 重放写入 →
金样，以及 `--include-ignored` 下真实 TS 重开 Rust 数据库并读取文件的反向验证。
Node verifier 的 stdout/stderr 写入文件后再读取，45 秒超时会终止整个进程组并
reap，panic 时仍能带出两侧输出。`home.mts` 在 HOME 未设/空串时直接询问真实 TS
的 `os.homedir()` 与 `GENERAL_WORKSPACE_DIR`，再与 Rust 子进程逐值比对（不落
盘为金样）；现有 `edgeclaw_storage.rs` 的事务迁移、回滚、双实例并发打开和
L0 repair 回归同时适用。

```sh
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --test edgeclaw_accessors
PDRS_BASELINE="$PDRS_BASELINE" cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-gateway --test edgeclaw_accessors -- --include-ignored
python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-accessors/negative.py /tmp/accessors-negative
```

`negative.py` 逐条临时破坏产品实现；每条必须编译成功且在真实 TS 金样断言处转红。
脚本用 `finally` 恢复原始字节，不修改 oracle。证据及双次生成 SHA256 见本单 RESULT。
