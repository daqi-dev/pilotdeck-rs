# pilotdeck-yaml

基线 npm `yaml` **2.9.0** 的 UTF-16 解析、组合和 `Document#toJSON()` 移植。生产代码不调用 Node，不使用其他 YAML loader，不预处理或删除 npm 接受的输入。逐文件对应、来源 SHA-256 和 ISC 原文见 [PROVENANCE](../PROVENANCE.md)（该文件的「pilotdeck-yaml」节）。

```rust
use pilotdeck_protocol::JsString;
use pilotdeck_yaml::{Options, parse_document};

let source = JsString::from("a: &x [1, 2]\nb: *x\n");
let document = parse_document(&source, Options {
    pretty_errors: false,
    ..Options::default()
});
assert!(document.errors.is_empty());
let graph = document.to_json().unwrap();
let value = graph.to_js_value().unwrap();
assert!(value.as_object().is_some());
```

`parse_document(&JsString, Options) -> Document` 总是返回文档。和 JS 调用点一样，调用方先检查 `errors`，再调用 `to_json()`；未解析 alias 和 alias 扩张限制等转换错误由 `Result` 返回。`parse_all_documents` 返回 `Documents`，可作为文档 slice 使用；空流的 comment、directives、errors、warnings 位于 `stream_info`。

- 所有动态字符串、源码、诊断和对象键使用 `JsString`，保留孤立 surrogate。`pos`、range 和 LineCounter 使用从 0 开始的 UTF-16 偏移；`line_pos` 的行列从 1 开始。
- 诊断保留 name、code、message、pos 和可选 linePos。默认 `pretty_errors=true`，包含原版代码摘录和 caret；`false` 时保留原始 message，且不附加 linePos。
- `ValueGraph` 用索引引用保存 alias 共用对象与环。Object 保留插入序，JS 数字属性的枚举顺序由协议编码器处理；Map/Set 保留 JS 插入序。number 为 binary64，保留 `-0`、NaN 和 Infinity。Symbol 有独立身份。
- `to_json()` 对 timestamp / binary 分别执行原版 Date / Buffer 的 toJSON 行为，得到 ISO 字符串 / `{type:"Buffer", data:[...]}`。`to_js_value()` 保留 UTF-16 和 binary64；共用的无环引用复制为树。JsValue 无法表达的环、Map、Set、Symbol 返回明确的 ConversionError。
- `keep_source_tokens=true` 保存 CST。`line_counter` 接受 `Rc<RefCell<LineCounter>>`，调用方可在解析后查询；单文档模式会像 JS generator 一样在第二个文档产出时停止累积行位置。

## 基线调用点与选项

逐个检索了裁定列出的 10 个文件。路径相对 `resources/PilotDeck/`，行号对应 commit `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。

| 调用点 | 行 | 解析调用 / 实际选项 |
|---|---|---|
| `src/pilot/config/loadPilotConfig.ts` | 251 | parseDocument，`prettyErrors:false`，检查 errors 后 toJSON |
| `src/pilot/config/updateLocalSettings.ts` | 73 | parseDocument，`prettyErrors:false`；后续 AST 修改/toString 留到 stringify PR |
| `src/pilot/config/sdkSessionSettings.ts` | 164 | parseDocument，`prettyErrors:false` |
| `src/pilot/staffdeck/projectBinding.ts` | 135、208 | parseYaml，默认选项 |
| `src/extension/skills/SkillManager.ts` | 578 | parseYaml，默认选项 |
| `src/sop/staffdeck/StaffDeckSopDefinitions.ts` | 19 | parseDocument，`prettyErrors:false` |
| `src/gateway/dialog/commands.ts` | 138 | parseYaml，默认选项 |
| `src/tool/builtin/filesystem/syntaxDiagnostics.ts` | 221、306 | parseDocument，`lineCounter` + `prettyErrors:false` |
| `src/cli/commands/gatewaySetup.ts` | 619 | parseYaml，默认选项；stringify 另做 |
| `ui/server/staffdeck-publish-route.js` | 45 | 注入 `createAtomicBundleStore({parse:parseYaml})`；`staffdeck-publish-runtime.mjs:74` 只传源码 |

扩展 grep 还发现 3 个非测试文件：`ui/server/services/pilotdeckConfig.js:786,1035,1317` 使用默认 parseYaml，`:1023` 使用 `parseDocument(...,{keepSourceTokens:true})`；`ui/server/routes/config.js:843` 和 `ui/server/routes/modules.js:55,547` 使用默认 parseYaml。因此本 crate 支持 prettyErrors、keepSourceTokens、lineCounter，其他解析行为采用原版默认值，且保留源码 `%YAML 1.1` / `%YAML 1.2` 的 schema 切换。

`Options::from_js` 只接受 prettyErrors / keepSourceTokens 的 boolean 值；其余键和错误类型返回带 option name/message 的 `UnsupportedOption`。lineCounter 是有状态回调对象，通过 Rust 的类型化字段提供。version、schema、customTags、strict、compat、stringKeys、uniqueKeys、intAsBigInt、merge、maxAliasCount、mapAsMap、reviver、logLevel 等非基线选项不接受，不会悄悄忽略。json/failsafe 标签表保留在移植源码中，但不额外开放 schema 选项。

## 对应关系和边界

lexer/parser/composer 保留原函数和状态分支；yield 变为输出队列，节点身份变为 arena 索引，处理顺序不变。compose 的 resolve/map/seq/scalar/props/end 文件逐个对应。schema 的 core、yaml-1.1、json、failsafe 和 tags 保留原标签顺序、regex 和 resolver。nodes / doc 保留解析和 toJSON 所需的数据与方法；详细文件表在 PROVENANCE。

公开入口集中在 `parse_document`、`parse_all_documents`、`Document::to_json`；parse 模块还暴露差分所需的 Lexer/Parser/CST/LineCounter。CST 的读取 stringify、resolveAsScalar（默认 strict=true）和 item visitor 可用；创建/替换 scalar 的两个 CST mutator、完整 AST 编辑、公开 YAML stringify、JS `parse` 的抛错/日志包装、原生 Date/Buffer 的 keep=true 转换、reviver 不在本轮实现。内部有仅用于复杂映射键转字符串的 flow stringifier，这是 toJSON 的必要依赖。

超过 64 层的组合和较大值图转换在临时 32 MiB 栈的 scoped worker 上执行，保持原递归函数；外部 LineCounter 仍在调用线程更新。另有测试从 2 MiB 调用栈解析/转换 512 层，包括 keepSourceTokens。

V8 的 `Maximum call stack size exceeded` 取决于 JIT 和运行时栈大小，Rust 极深递归的栈耗尽行为尚未证明等价。固定差分包含深度 24–128，并单独录制 256 / 512 层；不要把该结果解释为任意深度或 npm 全部 API 的等价证明。

## 差分与复现

- 原始 **71** 条 `tests/fixtures/yaml.json` 保留原字节，迁自 gateway；录制脚本 `tests/record_config_yaml.mts` 一并迁移。
- 固定 xorshift32 seed **0x294a4a6b**，Node v22.22.0 + 基线 yaml 2.9.0 录制 **2,986** 条新文本，加原始 71 条共 **3,057** 条。
- 2,304 条组合生成 + 21 条分支探针 + 120 条复杂键组合 + 29 条边角探针 + 512 条确定性变异。含 scalar 样式/转义、块/流集合、anchor/alias、tag、directive、多文档、注释、错误输入、深嵌套和长行，包含原始/转义孤立 surrogate。
- 逐例比较 lexer token、完整 CST、lineStarts、CST.stringify、原始与 pretty 诊断、toJSON 值图或异常；parseAllDocuments 比较每个文档的值/诊断、版本继承、空流元数据；CST visitor/resolveAsScalar 比较路径和解析结果。
- 71 条旧例 + 21 条分支探针额外按 UTF-16 的 1 / 2 / 7 / 31 单元分块，**368** 个增量输入场景。包括 surrogate pair 中间断块。
- manifest 记录 Node/yaml/基线版本、seed、每片条数/字节数/SHA-256。Rust 测试检查哈希和单文件 ≤ 2,000,000 字节；不删除或跳过失败输入。

从 worktree 根运行（Node 仅用于离线录制；Rust 测试不需要 Node）：

```sh
node src/pilotdeck-yaml/tests/record_generated.mjs \
  /Users/da/ws/pilotdeck-rs/resources/PilotDeck \
  src/pilotdeck-yaml/tests/fixtures/differential
cargo test --manifest-path src/Cargo.toml -p pilotdeck-yaml
```

重录原始 71 例时，使用 `node --import <baseline>/node_modules/tsx/dist/loader.mjs tests/record_config_yaml.mts <baseline> <output.json>`。catalog 的 32 条 fixture 和 recorder 仍位于 gateway tests，供下一个配置/catalog PR 使用；本轮没有接入 gateway dispatch。
