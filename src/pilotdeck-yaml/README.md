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

深度保护的常量来自 Node **v22.22.0** + yaml **2.9.0**、默认栈（无 `--stack-size`）的实测。每种形态做 5 次二分，每个观测使用新进程，取首次出现 `RESOURCE_EXHAUSTION` 的最低深度。原始观测、错误位置和 catch 节点在 [`tests/fixtures/depth.json`](tests/fixtures/depth.json)，常量在 `src/stack.rs`。形态按 CST 集合类型判断，同种集合使用对应限额，含不同集合类型的文档使用 mixed 限额；混合 oracle 用固定 `- ? ` 前缀加交替 flow seq/map，确保增长深度时 catch 位置附近的形态不变。

| 形态 | 5 次新进程最低/最高首次报错层数 | Node 热身后二分观测上界 | Rust 首次报错层数 |
|---|---:|---:|---:|
| flow seq | 664 / 664 | 1717 | 664 |
| flow map | 664 / 664 | 1527 | 664 |
| block seq（紧凑 `- `） | 782 / 782 | 1718 | 782 |
| block map（紧凑显式键 `? `） | 738 / 738 | 1527 | 738 |
| mixed | 664 / 664 | 1526 | 664 |
| 扁平 alias 链 | 搜到 1024 链，无 compose RESOURCE_EXHAUSTION | 不适用 | 保留 Node 的默认 alias 扩张限制 |

compose 显式计数，在集合 tag/props 处理后触发保护，以保留 catch 前产生的诊断。错误仍是 `YAMLParseError` / `RESOURCE_EXHAUSTION` / `Maximum call stack size exceeded`，`pos` 走原版 token offset/source-length 规则。随后像 `compose-node.js` 的 catch 后分支一样调用空 scalar composer，保留 anchor/tag/comment/space/source token，并继续处理兄弟节点。阈值前一层和第一报错层额外比较诊断位置、空节点范围及源 token。

lexer 用状态循环；parser 的 step/pop 用工作列表推进；CST stringify/visitor/相等比较、新行检查、AST 前序遍历、alias 计数和数组转字符串均不使用原生递归。CST 子节点改为 `Arc<Token>`，克隆共享后代；`Token::drop` 显式拆除所有边（start/props/end/items/key/value/flow_start），包括未结束的 parser 和 keepSourceTokens 留下的 CST。AST 和 ValueGraph 本来就用 arena 索引，释放无需递归；协议 `JsValue` 已有迭代 Drop。保留递归的 toJSON、merge、复杂键 stringify 共享 782 层转换预算（实测 compose 上限中的最大下界），ValueGraph→JsValue 也显式限深，超限返回 `RangeError: Maximum call stack size exceeded`。组合超过 64 层和较大图的转换仍在 32 MiB scoped worker 上运行；它只承载**有界**递归，外部 LineCounter 留在调用线程。

从 **2 MiB** 调用栈验证 6 种形态 × 10⁴/10⁵/10⁶，共 **18** 个场景（全部 keepSourceTokens），逐条比 Node 的错误 code/message。扁平 alias 链的 Node 行为是 parse 无错误，toJSON 抛 `ReferenceError: Excessive alias count indicates a resource exhaustion attack`，不伪造 RESOURCE_EXHAUSTION。另外验证百万层裸 CST 的 clone/相等/stringify/visitor/Drop、未结束 parser 的深 CST 释放、单次 dedent 关闭十万层块序列、百万层 arena 值图和十万层手动 AST 转换，保留原先 512 层正常转换回归；另有十万层单次 dedent 后兄弟节点保留、10,000 层扁平 merge alias 链的 toJSON RangeError 回归（Node 原始观测见 `merge-depth.json`）。

**残余风险（第 4 轮，待协调者复核）**：V8 阈值依赖 JIT、调用帧和运行时栈；[`depth-warm.json`](tests/fixtures/depth-warm.json) 记录每形态热身 32 次后再做 5 次二分。同形态 Rust 下界 L 至热身观测上界 U 的 `[L,U)` 区间，Node 可能仍成功而 Rust 已报错。保守选择下界是为了宁可报告资源耗尽也不让进程崩溃；这些运行时观测不能当作所有机器的绝对 V8 阈值。不同集合混合比例、tag/props、调用环境也可能改变 catch 的位置或诊断数量。附加 [`depth-recovery.json`](tests/fixtures/depth-recovery.json) 实测在 cutoff 处加 `&cut !!str` 时，Node 的空 scalar 回退再次耗尽栈，记录两个 RESOURCE_EXHAUSTION；Rust 的有界回退记录一个，末次位置、空字符串节点、range/anchor/tag/warning 与 Node 一致。该差异显式保留，测试只对这个已登记的附加运行时场景按 code/message 和最终回退节点断言；常规 3099 条差分、五种阈值边界和 18 条压力输入均没有诊断数量容差。

## 差分与复现

- 原始 **71** 条 `tests/fixtures/yaml.json` 保留原字节，迁自 gateway；录制脚本 `tests/record_config_yaml.mts` 一并迁移。
- 固定 xorshift32 seed **0x294a4a6b**，Node v22.22.0 + 基线 yaml 2.9.0 录制 **2,986** 条新文本，加原始 71 条加第 4 轮 42 条独立分片，共 **3,099** 条。
- 2,304 条组合生成 + 21 条分支探针 + 120 条复杂键组合 + 29 条边角探针 + 512 条确定性变异；第 4 轮另加 24 条独立转义/Unicode 探针及 18 条隐式键长度/UTF-16 边界探针。含 scalar 样式/转义、块/流集合、anchor/alias、tag、directive、多文档、注释、错误输入、深嵌套和长行，包含原始/转义孤立 surrogate。
- 逐例比较 lexer token、完整 CST、lineStarts、CST.stringify、原始与 pretty 诊断、toJSON 值图或异常；parseAllDocuments 比较每个文档的值/诊断、版本继承、空流元数据；CST visitor/resolveAsScalar 比较路径和解析结果。
- 71 条旧例 + 21 条分支探针额外按 UTF-16 的 1 / 2 / 7 / 31 单元分块，第 4 轮新增语料也逐个分块，共 **536** 个增量输入场景。包括 surrogate pair 中间断块。
- manifest 记录 Node/yaml/基线版本、seed、每片条数/字节数/SHA-256。Rust 测试检查哈希和单文件 ≤ 2,000,000 字节；不删除或跳过失败输入。

从 worktree 根运行（Node 仅用于离线录制；Rust 测试不需要 Node）：

```sh
node src/pilotdeck-yaml/tests/record_generated.mjs \
  /Users/da/ws/pilotdeck-rs/resources/PilotDeck \
  src/pilotdeck-yaml/tests/fixtures/differential
cargo test --manifest-path src/Cargo.toml -p pilotdeck-yaml
```

重录原始 71 例时，使用 `node --import <baseline>/node_modules/tsx/dist/loader.mjs tests/record_config_yaml.mts <baseline> <output.json>`。catalog 的 32 条 fixture 和 recorder 仍位于 gateway tests，供下一个配置/catalog PR 使用；本轮没有接入 gateway dispatch。

第 4 轮深度、热身与压力 oracle 的重录命令（均用默认 Node v22 栈）：

```sh
node src/pilotdeck-yaml/tests/record_depth.mjs <baseline> src/pilotdeck-yaml/tests/fixtures/depth.json
node src/pilotdeck-yaml/tests/record_depth_warm.mjs <baseline> src/pilotdeck-yaml/tests/fixtures/depth-warm.json
node src/pilotdeck-yaml/tests/record_stress.mjs <baseline> src/pilotdeck-yaml/tests/fixtures/depth-stress.json
node src/pilotdeck-yaml/tests/record_recovery.mjs <baseline> src/pilotdeck-yaml/tests/fixtures/depth-recovery.json
node src/pilotdeck-yaml/tests/record_traversal.mjs <baseline> src/pilotdeck-yaml/tests/fixtures/traversal.json
node src/pilotdeck-yaml/tests/record_depth.mjs <baseline> - merge-chain 10000 > src/pilotdeck-yaml/tests/fixtures/merge-depth.json
python3 src/pilotdeck-yaml/tests/check_mutations.py
```

`check_mutations.py` 只改 `_tmp/review-yaml/mutations` 的副本，15 个 mutant 必须能编译并由新增 42 条 Node oracle 报 FAIL，恢复后必须 PASS。覆盖 `\a\b\e\f\v\r\t`、反斜杠接真实 TAB、`\/`、`\U0010FFFF` 的接受上界及 `\U00110000` 超界、block/flow-seq 的 1024/1025 键长判定；不修改生产源码或 oracle。17 个差分分片共 26,078,817 字节，最大 1,799,945 字节；原有 16 个分片逐字节不变。新增 visitor 控制 oracle 还覆盖 Skip/Remove/Index/Break/Next（含 after-key 返回符号后仍访问 value 的顺序）。

## 资源缩放回归（第 8 轮）

常规 `cargo test --all` 不运行耗时/峰值内存测试。单独运行 release 测试：

```sh
export CARGO_HOME="$PWD/_build/cargo-home"
export PILOTDECK_YAML_BASELINE=/path/to/baseline
cargo test --manifest-path src/Cargo.toml -p pilotdeck-yaml --release \
  --test resource_scaling resource_scaling -- --ignored --exact --nocapture
```

也可以直接运行脚本并指定机器结果路径：

```sh
python3 src/pilotdeck-yaml/tests/run_resource_scaling.py \
  --baseline /path/to/baseline --output _tmp/review-yaml/resources-round8.json
```

脚本检查 Node v22 和基线 yaml 2.9.0，使用同一个生成文件测试双方。12 个族覆盖多文档、注释、plain、双引号与转义、宽 flow seq、宽 block map、固定 500 层混合嵌套、anchor+alias、块标量、复杂 `? ` 键、大量 `%TAG` 指令与逐 handle 使用、大量锚与重复 `<<` merge。新增 merge 族每个锚固定 2,000 键、各合并 90 次；N 只增加锚数（原复现为 20 个锚），4N 不增加键宽或深度。N 仅由 Node 耗时校准到约 0.2–1 秒，4N 增长长度/宽度/重复数量；固定深度族不增加深度。复杂键固定 200 层，通过增加文档数校准；单个 700 层键的 Node 转换已约 25 秒，不能作为 0.2–1 秒校准的最小单位。

每个规模、每个实现分别启动三个新进程，交错执行顺序；耗时取中位数，RSS 取三次最大值。计时覆盖默认 `parseAllDocuments` 和所有文档的 `toJSON`，保留文档与转换结果；输入读取、进程启动和释放不计入耗时，均计入进程峰值 RSS。Node 的 collection-key 提示不计终端 IO。独立 Python wrapper 只启动一个子进程，使用内核 `getrusage(RUSAGE_CHILDREN).ru_maxrss`；macOS 单位为 bytes，Linux 为 KiB。没有在同一进程连续测多个输入，没有用父进程 RSS、退出后 current RSS 或上一样本的高水位替代峰值。超时/非零退出会保留失败行并让脚本退出非零。

判定同时检查耗时和 RSS：两个规模的 Rust/Node 耗时与绝对 RSS 均不得超过 3；耗时的 (Rust 4N/N) / (Node 4N/N) 不得超过 1.5；增量 RSS 满足 (Rust RSS@4N − Rust RSS@N) ≤ 1.5 × (Node RSS@4N − Node RSS@N)。RSS 不用原始增长比判定，以避免 Node 固定运行时内存影响缩放结论。`--sizes <previous.json>` 可以复用已记录的 Node 校准值，`--families` 可定位单个族，`--rust-bin` 供 ignore 测试复用已编译二进制；默认所有族都运行。JSON 保存每次原始测量、输入字节数/SHA、N、所有比值和通过/失败结果；同时核对两个规模全部样本的文档数、error/warning 数和转换错误数，双方不一致也判失败。

内存表示的调整保持 UTF-16 值与位置：每个流的 `Document::line_counter` 是同一个 `Arc<LineCounter>`；schema 与 directive 标签表共享，实际扩展时采用 copy-on-write。CST 的常见字段保留在 Token，其他字段通过 `TokenData` 的 Deref/DerefMut 按需分配；构造 CST 使用 `Token::new`。Node 的可选装饰通过 `NodeMetadata` 按需分配，`src_token` 为 `Arc<Token>`。读字段和查询位置的方法保持原语义；外部源代码若用 Token struct literal，应改用构造函数。

完整输入的 lexer 借用输入 UTF-16 并立即发送 lexeme，composer 立即消费 CST；增量 Lexer 仍拥有未完成的尾部。没有 keepSourceTokens 时，文档根的宽 map/flow CST 项随组合释放。注释循环累积 UTF-16，集合键保留片段至最外层一次展开。转换结束删除从根不可达的临时容器，并重映射全部引用，保留别名共享、环、Map/Set 顺序和 Symbol 身份。pretty-error 先截取显示范围，再复制字符，避免每条诊断重拷整条长行。

协议层 `JsString` 保持原有 `Vec<u16>` 存储。YAML crate 内的 `Token::source`、`Node::source`、`ScalarValue::String` 和 `Value::String` 使用 `Arc<JsString>`；同一 plain scalar 的 CST、AST source/value 和转换图共享本地字符串实例，scalar alias 也共享转换值。没有全局去重表；不同文档或不同 scalar 不自动合并内容相等的字符串。plain scalar 无需换行折叠时复用 CST source，其他样式解析后共享自己的解析值。UTF-16、相等/排序、行位置保持原语义，孤立 surrogate 回归覆盖共享与输出边界。`ValueGraph::to_js_value` 在协议边界复制为原有 `JsString`，默认 alias 数量上限未改。

这些 YAML 字段/变体的 Rust 构造调用方需使用 `Arc::new(js_string)`，读取可通过 Deref 或 `.as_ref()`，修改共享字符串可用 `Arc::make_mut` 保留 copy-on-write。`resolve_as_scalar` 的公开返回值仍为 `JsString`。80,000 码元和 99 个 alias 的回归验证内容、长度和 YAML 内共享；plain scalar 回归还验证 CST/AST/图的同一实例及释放文档后的输出。

多文档入口对仅含注释/空白且没有外部 LineCounter 的空流不分配不可观察的行表；其他输入继续使用原 parser 逐 token 登记位置，外部 counter 保留原行为。全部 3099 例的 parseAllDocuments 行表另与原 Node parser lineStarts 逐项比较，所有文档仍共享同一 Arc。

`Directives::tags` 是 `Arc<TagTable>`：内部 Vec 保存声明顺序，HashMap 按原始 UTF-16 handle 定位。重复声明覆盖原槽位的 prefix，不移动顺序；1.1 继承/重置、1.2 文档重置时整个表与索引一起共享或切换。读取仍支持 `.iter()`、`.len()` 与下标；修改使用 `Arc::make_mut(&mut directives.tags).insert(handle, prefix)`，不直接修改内部 Vec，以保证索引同步。

转换 Object/Map/Set 的 Vec 保留顺序，达到 16 项后建立私有索引；小表扫描最多 16 项。数值键使用 SameValueZero（NaN 等价、±0 合并），Ref/Symbol 按身份区分；merge 的 only-missing 与覆盖顺序不变。`!!omap` 标量去重也使用索引。alias 按 arena 前序遍历预先记录最近的前置同名 anchor，转换时按 NodeId 直接查找；集合键中已注册/局部 anchor 使用名称集合。Schema 只含有限的内置 tags，按默认匹配优先级扫描；未知 `%TAG` 不扩展 schema。tag 输出的 prefix 匹配仍按声明顺序遍历，属于有序前缀匹配而非按 key 查表。

第 8 轮另有 44 条 `tests/fixtures/set-merge.json` 差分，不改原 3,099 条分片。YAML 1.1/1.2 下核对 `!!set`、bare/显式/quoted merge、alias、顺序、集合键、非 null 值诊断及转换异常；即使存在解析诊断也比较显式 `to_json()` 的结果。npm 以 YAMLSet nodeClass 组合 set 标签，resolve 失败时仍保留 Set；Rust 同样报告错误并保留类型。重录：`node tests/record_set_merge.mjs <baseline> tests/fixtures/set-merge.json`（从 crate 目录运行）。

转换期的宽 Object/Map/Set 索引仅保存随机哈希到槽位，碰撞通过容器内的原键及 SameValueZero 校验，不拥有键副本。完成对象构建及 merge 临时 Map 构建后删除索引；merge 临时 Map 的 entries 移入目标，立即释放累加器存储。原键插入序、显式覆盖与 merge 仅补缺失键的规则不变。
