# ICU4X collation source patch（#132）

来源：crates.io `icu_collator` **2.1.1**，使用本机已有源码缓存；未下载预编译产物。
发布包 SHA-256：`32eed11a5572f1088b63fa21dc2e70d4a865e5739fc2d10abc05be93bae97019`。
上游 https://github.com/unicode-org/icu4x ，许可证见 `LICENSE`（Unicode-3.0）。
`UPSTREAM.sha256` 保存所有导入文件的原版哈希；版本不升级。

仅 `src/comparison.rs`、`src/elements.rs` 有算法差量：

- 从已有 compare_impl 提取主级至四级比较内核，原合法输入和新 UTF-16 输入共同调用。
- 新 `compare_utf16_preserving_surrogates` 按孤立项切分合法 run，run 使用原 CollationElements；孤立码元直接进入原 implicit-weight 公式。迭代惰性推进，不用 PUA/FFFD 占位。
- Identical 强度保留每个合法 run 的 NFD、原孤立码位以及 U+FFFE 特例。
- `with_backward_second_level` 暴露已有选项；产品 node_collator 为仓内 78.2 数据遗漏的 fr-CA 默认补倒序 secondary，显式 collation 不覆盖。原件/回归见 gateway `tests/fixtures/native-collation/`。

原 Cargo.toml、其余源码及上游测试/benches 原样保留。主 workspace 与 smoke probe 使用同一 path patch；数据 crate 不改。

核差量：从相同版本 crates.io 源包读取文件，对照 `UPSTREAM.sha256`，再比较上述两文件。内部比较主循环除左右迭代器改为闭包调用外没有更改判序规则。Node 原件和变异验证是产品测试，不能用本文件说明替代。
