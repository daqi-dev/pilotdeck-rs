# #502 自述差异补登的真实 TS 证据

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，Node v22.22.0。生成器先用
`tests/baseline_source_guard.mjs` 绑定整棵 src 的 Git blob，再登记实际入口的相对 import 闭包哈希；
jsonrepair 从 Anthropic stream 的真实 import 位置解析，包实现字节须匹配既有 jsonrepair 金样。
所有文件写入输出 parent 下独占 mkdtemp 目录，基线和 node_modules 只读；无网络监听或外部请求。

```sh
PDRS_BASELINE=/abs/fixed/PilotDeck LANG=en_US.UTF-8 \
  node --import /abs/fixed/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/diff-backfill/generate.mts /abs/output-parent
```

连续生成两次须 `cmp` 一致再更新 oracle。当前 SHA256：
`91f07c904dc11a94f06cba0b3caa9a4171289639b5ea786382a66793205fb5d5`。
不手填期望值；fetch 端口仅记录真实 TokenAccountingRuntime 发出的 URL 并返回固定输入响应，
所有 URL 构建和错误均由真实 TS 产生。InstructionDiscovery 对同一个实例实际更改环境变量再读；
Rust 为避免多线程测试中不安全的环境突变，注入每次读取环境的返回值，另外钉住显式目录不读环境。

| 集合 | 数量 | 用途 |
|---|---:|---|
| urlCases | 48 | 三协议 × 八种 URL × 正常/错误 tools，错误消息与优先级、发出的 URL |
| managed | 5 | 同实例 managed directory 从 a→b→空→缺失→a，整个层列表 |
| syntaxCases | 8 | 真实 readTranscript 诊断与真实 FileRunRegistry 语法错误 |
| dates | 9 | 合法扩展年份/小数截断与非法 TimeClip 边界 |
| validation | 6 | 未对齐的默认原型属性查找，仅证据，非相等回放 |
| arrays / repairs | 4 / 5 | String / jsonrepair 的独立深度实测，非固定 V8 阈值声明 |
| collation | 3 | 孤代理与真实 PUA 的 Node 比较符号 |

产品相等回放：count_body 的 `count_urls_match_real_ts_including_error_precedence`、instructions 的
`managed_environment_is_read_on_each_discovery_like_real_ts`、prompt 的 `extended_year_timestamps_match_real_ts`，
以及 `tests/diff_backfill.rs` 两条 storage 语法测试。dates 的非法值只用于披露剩余未对齐项，不跳过后声称全域相等。
其余集合是账本现状证据，不是差异豁免或已实现声明。

变异负控（工作树须无并发写入；会按字节保存并 finally 恢复产品源码）：

```sh
python3 src/pilotdeck-gateway/tests/fixtures/diff-backfill/negative.py /abs/log-directory
```

五条分别恢复 URL coercion、构造时缓存环境、Rust JSON parser 文案（两个入口）与旧的无扩展年份 formatter；
只认实际测试断言失败，不把编译失败当杀死变异。每次 cargo 前检查 cargo-slots，退出 1 时脚本不修改文件、不运行 cargo。
