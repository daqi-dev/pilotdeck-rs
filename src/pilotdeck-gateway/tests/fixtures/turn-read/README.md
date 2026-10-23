# turn-read

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的真实 TS 读面金样。生成器执行未修改的 `InProcessGateway.getActiveTurnSnapshot` / `runGet` / `runEvents` / `runReattach` / `readSessionMessages`、真实 `FileRunRegistry`、coordinator 与 replay store。源码逐份校验提交内容与工作文件哈希，运行前后均校验基线；Rust 经 `test_baseline::oracle` 检查溯源。

宿主注入仅提供可控的持久 history reader（在 epoch 的 `before` 快照之后同步变异 coordinator 再返回 history 或抛错）与一次注入的 registry 失败；不实例化真实 Gateway 宿主。`sameTurnEpoch` 重试循环就是产品实现本身，不是夹具重写。epoch 的完整矩阵（active/terminal/替换/回收）已由 `tests/fixtures/turn-replay` 金样覆盖；本夹具专注读面本身：history 与 stream 的合并形状、错误传播、registry 投影与游标分页。每个成功结果同时记录顶层键序，键序错乱同样判负。

20 场景分组记录：`readCases` 8 条（稳定 active 合并、无 turn、读中 terminal 化重试、run 被替换重试、epoch 三次不收敛的逐字最终错误、history 错误不重试直接传播、未配置、宿主已带 `stream` 键时原位替换）；`snapshotCases` 1 条（includeEvents 真/假与缺失 session 的纯委托）；`runCases` 11 条（含 runReattach 与已完成 run 的 runEvents 的投影、跨 session/project/缺失、分页含 afterSeq 0/2/4/5/6 与 limit 1/500 与 gap、游标/上限校验与 RUN_NOT_FOUND 的 8 个非法输入、CAPABILITY_UNAVAILABLE、registry 失败传播；修复轮 1 增补三条：非数字 `lastSeq` 的 JS 关系比较与原值 `nextSeq`（"5"/null/true/[2]/"10"）、损坏快照的 `SyntaxError`/`TypeError`/`Error` 错误传播（`"{"`、`"null"`、schemaVersion 不符、records 含 null）、含孤立代理 runId 的 RUN_NOT_FOUND 逐字消息；修复轮 2 增补两条：ToPrimitive 抛错的 `lastSeq`（`{toString:null}`、`[{toString:null}]`、`{toString:5}` 抛 V8 TypeError；`{}`、`{valueOf:5}`、`[{}]` 照常 NaN 无 gap）、深层 `lastSeq` 数组经读面端口 seam 的 2000/2001/2400 层转换与原值 `nextSeq`）。每条 runCase 调用支持逐调快照手术：`snapshot` 原样写入文件字节，`recordPatch` 原位改写解析后记录的字段；两侧 registry 每次读取都重载磁盘。`setup.inMemory` 时改用内存记录端口（`get` 按调用序供应当调 `record`）——文件 registry 自身的 V8 structuredClone 递归活不到待测深度，深层场景因此走端口 seam。注册表记录带本进程时间戳，生成与回放两侧均先掩码 `<date>` 再比较。

## 重生成

从仓库根执行，使用基线已有 Node 22 / tsx，所有输出写工作区或 /tmp：

```sh
export PDRS_BASELINE=/path/to/PilotDeck
export TSX_DISABLE_CACHE=1
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/turn-read/generate.mts /tmp/turn-read-first
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/turn-read/generate.mts /tmp/turn-read-second
cmp /tmp/turn-read-first/oracle.json /tmp/turn-read-second/oracle.json
shasum -a 256 /tmp/turn-read-first/oracle.json /tmp/turn-read-second/oracle.json
```

当前 SHA-256：`1cea0e94772b2e1311c50bb4b4a4d0680e8857310db5a2af44a90cd632378c65`。

## 对拍与负控

```sh
# 先调用项目 cargo-slots.sh；使用本树独立 CARGO_TARGET_DIR。
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib runtime::turn::read
PDRS_BASELINE=/path/to/PilotDeck TSX_DISABLE_CACHE=1 \
  cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib \
  real_ts_turn_read_regenerates_identically -- --ignored
python3 -I src/pilotdeck-gateway/tests/fixtures/turn-read/negative.py /tmp/turn-read-negative
```

`negative.py` 必须独占本树运行。它临时变异产品源文件（`runtime/turn/read.rs` 与 `storage/run_registry.rs`，两者哈希都写入 `negative-original-sha256.json`），测试与金样不变；先跑原版，再对每个变异要求编译成功且目标断言失败。原始 SHA、变异前后文本、完整输出、退出码写输出目录；finally 恢复源文件原字节并核验。不要与编辑器、cargo 或另一负控并发写本树。

金样回放全程留在 `JsValue` 域并以 `stringify_json` 逐字节比较，键序与字段缺失都判负；并发行为另有两条例内测试：history 挂起时并发 publish 不被读阻塞，以及发布中 settle 的 epoch 在三限内重读到一致快照。

Gateway WS 方法接线归 pr5；本单不改动 `session/handle.rs` 与连接层。
