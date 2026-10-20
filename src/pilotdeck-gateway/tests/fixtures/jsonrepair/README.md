# jsonrepair（工具参数修复）TS 对拍

固定基线 `cc7c4e65`；被测是 `providers/anthropic/stream.ts` 实际 import 的 `node_modules/jsonrepair`（3.14.0，`packageSha256` 记 esm/cjs 主文件哈希）与真实 `normalizeAnthropicStreamEvent`。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
LANG=en_US.UTF-8 node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/jsonrepair/generate.mts "$(mktemp -d)"
cargo test --manifest-path src/Cargo.toml --locked --offline -p pilotdeck-gateway --lib jsonrepair
```

86 条坏/好 JSON（截断、尾逗号、缺逗号、单引号/弯引号、未加引号键值、注释、缺/多括号、控制字符、非法转义、拼接字符串、URL、前导零、markdown 围栏、JSONP、正则、省略号、NDJSON、各种修不好的输入）：每条记录 `repaired`（或抛错）、修复结果能否 `JSON.parse`，以及把它作为一次工具调用参数喂给真实流规范化得到的事件（`tool_call_end` + `wasRepaired`，或延迟的 `invalid_tool_arguments`）。Rust 回放逐码元比对修复文本、逐字节比对事件 JSON；条数精确断言（74 修好 / 12 失败）。生成器两次逐字节一致，溯源用共享 `bindSourceTree` + `sourceClosureSha256`。
