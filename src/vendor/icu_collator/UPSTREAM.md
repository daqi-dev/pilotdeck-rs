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

## 发布范围裁剪（2026-11-01，#132 fix-r2）

按 `state/decisions-20261101-132-vendor-scope.md` 的方案 A，本目录只发布 lib：

- `Cargo.toml` 删除 `[[test]] name = "tests"` 与 `[[bench]] name = "bench"` 两个 target，并删除 include 中 `tests/**/*`、`benches/**/*`。保留 `autotests = false`、`autobenches = false` 等自动 target 开关。
- 删除 `tests/` 全部 5 个文件与 `benches/` 全部 11 个文件；具体相对路径与原始 SHA-256 仍完整保存在 `UPSTREAM.sha256`，没有删减或重算该清单。
- `VENDORED_PACKAGES` 登记本目录，沿用所有 vendor 仅可含 lib target 的守卫。主 workspace 与 smoke probe 继续使用同一个 path patch；数据 crate 不改。
- 本轮未修改任何算法源码；上述 `comparison.rs` / `elements.rs` 差量属于既有 #132 实现。

原规范化 `Cargo.toml` 的 SHA-256 为 `a706e2ceec461b38e46110fbb786baefb021edcec340ef717f36756f2640efd2`。`UPSTREAM.sha256` 是 25 个导入原件的校验清单，不是裁剪后本目录全量校验清单；在本目录直接运行全表校验会因已裁剪文件缺失及既有补丁而失败。

## 恢复与独立上游验证

从本机已有 Cargo 源码缓存取得 `icu_collator-2.1.1.crate`，先核对发布包 SHA-256（见文首），再解压到独占临时目录。进入解压后的 `icu_collator-2.1.1/`，用本仓保留的 `UPSTREAM.sha256` 对全部原件运行 `shasum -a 256 -c <本仓绝对路径>/src/vendor/icu_collator/UPSTREAM.sha256`。本轮已对缓存源包内的 25 项逐一核验，均与该清单一致。

要恢复裁剪内容，从已验证的临时目录取回原 `Cargo.toml`、`tests/`、`benches/`，先审阅差异；不得将整个源包覆盖本目录，否则会丢失已有算法补丁。恢复非 lib target 必须先重新决定 vendor 范围；当前方案下，上游测试源码可以在临时目录里独立保存，但要运行还须另补发布包 manifest 未声明的 dev-deps（见下段）；不重新暴露为产品 vendor targets。

2026-11-01 fix-r1 曾尝试直接运行上游 tests，但因缓存缺少 `databake` 在依赖解析阶段失败，未执行测试，不能记作通过；历史原始输出见 `_tmp/20261026-issue-132/fix-r1/vendor-tests.log`。发布包规范化 manifest 也未完整声明上游 tests 所需 `atoi`、`arraystring`、`icu_normalizer_data` 等开发依赖，因此还原文件不等于具备可运行的上游验证环境。产品回归与 Node 原件/变异验证另见 gateway `tests/fixtures/native-collation/`，它们不冒充上游完整测试。

核算法差量：从相同版本源包读取原文件，对照 `UPSTREAM.sha256` 后比较 `src/comparison.rs` 与 `src/elements.rs`。内部比较主循环除左右迭代器改为闭包调用外没有更改判序规则；本文件说明不能替代产品回归。
