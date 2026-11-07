# transcript-collation：#132 同 sequence 的 createdAt 排序

`generate.mts` 用固定基线 `cc7c4e6` 的真实入口 `src/session/transcript/TranscriptReader.ts::readTranscript`：
每个 case 写一份 JSONL（`sequence` 全部相同，只有 `createdAt` 不同），调用原函数并把
`entries` / `diagnostics` 原样记录。比较语义来自基线本身
`entries.sort((l, r) => l.sequence !== r.sequence ? l.sequence - r.sequence : l.createdAt.localeCompare(r.createdAt))`，
本夹具不复制、不重写该函数体。

| case | 真实 TS 顺序（`createdAt` 码位） | 判别力 |
|---|---|---|
| lone-high-among-valid-code-points | `𐀀` → `\ud800` → `` → `�` | 若把孤立代理折叠成 U+FFFD，`\ud800` 与 `�` 变成平局，顺序改变 |
| distinct-lone-code-units | `\ud800` → `\ud801` → `\udc00` | 折叠后三者全部相等，稳定排序保留输入序，顺序改变 |

排序结果在 `native-collation/oracle.json` 的 14 个 locale 上逐一核对过，这四个/三个键的
相对顺序与进程 locale 无关，因此 Rust 断言不依赖宿主 `LANG`。

Rust 侧由 `storage::tests::equal_sequence_created_at_keeps_isolated_utf16_weights` 重放：
写同一份 JSONL，按 `stringify_json` 逐条对照 `entries`，并要求 `diagnostics` 为空。
测试同时走 `crate::test_baseline::oracle(...)`，因此改动 `sourceCommit` / `sourceSha256`
或写坏 JSON 都会在无 `PDRS_BASELINE` 的默认运行里变红。

重生成（本机 `node -v` 必须是 `v22.22.0`、`node -p 'process.versions.icu'` 必须是 `78.2`，
否则生成器自己 `assert` 失败）：

```sh
export PDRS_BASELINE=/absolute/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/transcript-collation/generate.mts /absolute/new-output
```

生成器只写新目录里的 `oracle.json`（`flag: 'wx'`）。ignored 守卫
`transcript_collation_regenerates_identically` 用共享 `regenerates_identically("transcript-collation")`
逐字节重生。负控见 `../native-collation/negative_controls.py` 的 `transcript-host-remains`：
把宿主边界装回去必须让上述测试红。
