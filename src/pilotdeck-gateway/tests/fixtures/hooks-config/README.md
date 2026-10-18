# Hooks 配置与匹配 TS 夹具

固定源码为 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。生成器直接导入 `extension/hooks/config/*` 和 `protocol/{events,input}.ts` 的真实公共函数，不复制实现、不写基线。七份源码的 SHA-256、Node/Unicode 版本均记录于 `oracle.json`，生成前检查 HEAD 与对应文件的干净状态。

运行环境固定为 Node v22.22.0（Unicode 17.0）及该基线安装的 tsx：

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/hooks-config/generate.mts src/pilotdeck-gateway/tests/fixtures/hooks-config
node src/pilotdeck-gateway/tests/fixtures/hooks-config/generate-identifiers.mjs src/pilotdeck-gateway/src/hooks/identifier_ranges.rs
```

第二次生成写入另一目录，用 `cmp` 比较 `oracle.json` 和 `identifier_ranges.rs`。后一份是从同一 Node 运行时录制的 ECMAScript IdentifierStart/Continue 范围，供正则命名捕获使用；Rust regex 自带 Unicode 16 表不能代替 Node 的 Unicode 17。名称范围生成器只读取运行时属性集合，不读取用户配置。

`hooks_config.rs` 在默认 `cargo test -p pilotdeck-gateway` 中回放：

- 104 条解析：全部 25 种事件；根值、事件、matcher、hooks、type 与四种必需字段的错误分支；callback 禁止持久化；有效/无效项交错；严格类型过滤；非有限 timeout；继承字段；整数键顺序、4294967294/4294967295 下标边界、孤立代理和 headers 的 `__proto__`。
- 9,800 条 matcher：默认值、大小写、仅单独 `*` 通配、先于正则的 `|` 拆分、空分支与 trim、无 flags 的 UTF-16 正则、字符类、旧式转义、捕获/反引、量词、原子 lookaround、反向 lookbehind、命名捕获、千层嵌套、百万次可空重复的末轮捕获、资源预算内/远超预算、负向断言错误传播，以及固定种子的组合矩阵。
- 194 条 condition：名称只保留 ASCII 字母数字再转小写；工具名空/缺省；条件语法；删除所有 `*` 后做 JSON 文本子串匹配；键顺序、非有限数、数字格式、换行及孤立代理；4000/10000 层数组、对象及混合嵌套；短路及不序列化原型。
- 8 条 input + 32 条 event：payload 覆盖、event 最后赋值、仅顶层 ASCII 大写转 snake、键冲突、null/undefined 可选字段与受支持/legacy 事件判定。

输入用带类型标签的树保留 undefined、NaN/Infinity/-0、UTF-16 与显式原型。期望值来自真实 TS 的 JSON.stringify；Rust 用既有 protocol codec 对比同一 JSON 字节顺序，不经 serde_json 改写字符串或数字。生成器对每个解析诊断分支和关键怪癖另有命中断言。

## 资源边界（第二轮，协调者选项 B）

按本轮协调者决策，在 Node v22.22.0 的确定成功区设置保守预算；**[上界, V8 实际阈值) 这段属于已接受的不可判定带**。上界包含在成功区，实际拒绝的是严格超限输入；阈值依表达式、调用深度和 V8 运行时而变，不声称任意资源耗尽行为精确等价。授权卡：主仓 `_tmp/20261018-a67-pr3/card.md`（AI 代批，待人复核）。没有修改冻结尺子。

| 资源 | Rust 上界 | 度量与超限结果 |
|---|---:|---|
| 动态可空重复 | 2,000,000 工作单位 | 每次空体展开的捕获及量词成本；必需的内层重复相乘；空捕获及断言内捕获也计入。分支回溯恢复预算，原子断言退出丢弃内部工作。超限终止整个正则为 false，不能被负向断言反转为 true。普通文字扫描不计入该预算 |
| 条件 JSON 序列化 | 4000 层 | 根数组/对象计 1，混合容器逐层累加，仅访问 own properties。超限返回 `Err(HookConditionError::StackOverflow)`，名称 `RangeError`、文本 `Maximum call stack size exceeded`，由调用方传播 |

静态零宽重复和没有消费来源的自引用仍可压缩十亿次；它们的捕获若位于动态可空体内仍产生保存成本。量词次数不是统一阈值：`(a?)` / `(a*)` 体各需 2 单位，100 万次在预算内；`(?:a?)` 每次 1 单位，200 万次在预算内；8 个空捕获/断言捕获加 `a?` 的体为 10 单位。嵌套 `((a?){1000})` 为 2002 单位，999 次仍在预算内。只在实际执行到空迭代时收费，未进入的断言或早期不匹配不提前拒绝。

复现实测（与生成器使用相同的 `PDRS_BASELINE` / Node）：

```sh
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/hooks-config/probe-resources.mts
```

探针直接运行真实 `HookRuntime.run → runActive → matchHooks`，执行器均为无 IO stub；同时运行真实 matcher/condition 和裸 RegExp。外部同步栈额外加 0/32 帧。11 类正则体的 22 个预算内样本全部成功；约 4000 万单位的 22 个远端样本全部抛 RangeError，TS matcher/runtime 均为 false。JSON 三类形状在两种栈深度的 6500 层样本都成功，7000/10000 层都以同文 RangeError 拒绝真实 run。4000 的选择留有余量。原始命令与输出见工作树 `_tmp/20261018-a67-pr3/RESULT.md` 的「第二轮」。

回放严格断言五类数量为 **104 / 9800 / 194 / 8 / 32**。设置 `PDRS_BASELINE` 时，另逐一核对 7 个固定路径的 `sourceSha256` 与当前文件内容；路径集合、commit、Node 和 Unicode 版本始终检查。深值以 `["json", rawJson]` 标签传输，两端各自解析，避免 oracle 的编码器先触发栈溢出。异常期望直接由真实 TS catch 录得，没有手填为布尔值。另一个独立测试锁定已接受策略的 2M/2M+1 与 4000/4001 边界，不把 Rust 策略结果写成 TS oracle。
