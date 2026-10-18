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

- 102 条解析：全部 25 种事件；根值、事件、matcher、hooks、type 与四种必需字段的错误分支；callback 禁止持久化；有效/无效项交错；严格类型过滤；非有限 timeout；继承字段；整数键顺序、孤立代理和 headers 的 `__proto__`。
- 9,719 条 matcher：默认值、大小写、仅单独 `*` 通配、先于正则的 `|` 拆分、空分支与 trim、无 flags 的 UTF-16 正则、字符类、旧式转义、捕获/反引、量词、原子 lookaround、反向 lookbehind、命名捕获、千层嵌套、百万次可空重复，以及固定种子的组合矩阵。
- 182 条 condition：名称只保留 ASCII 字母数字再转小写；工具名空/缺省；条件语法；删除所有 `*` 后做 JSON 文本子串匹配；键顺序、非有限数、数字格式、换行及孤立代理。
- 6 条 input + 32 条 event：payload 覆盖、event 最后赋值、仅顶层 ASCII 大写转 snake、键冲突、null/undefined 可选字段与受支持/legacy 事件判定。

输入用带类型标签的树保留 undefined、NaN/Infinity/-0、UTF-16 与显式原型。期望值来自真实 TS 的 JSON.stringify；Rust 用既有 protocol codec 对比同一 JSON 字节顺序，不经 serde_json 改写字符串或数字。生成器对每个解析诊断分支和关键怪癖另有命中断言。

## 当前未闭合边界

上述矩阵不证明任意表达式的 V8 资源耗尽行为一致。Rust 使用显式栈并压缩冗余空重复；V8 对极大动态可空重复可能抛 `RangeError`，被 TS matcher 捕获为 false，而 Rust 仍返回数学匹配结果 true。已确认 `/^(a?){10000000}b$/` 与 `/^(a*?){1000000000}b\1$/` 对 `b` 存在此差异。这是**未获批准的 P1 缺口**，不能把本实现标为完整 TS 等价或可合并；复现原件与其余自证见本单 RESULT。没有修改冻结件或登记为已接受差异。
