# D6：ICU4X 2.1.1 Unihan 排序数据生成器

在仓根执行一条命令，下载官方数据、校验大小与 SHA-256、从锁定的 crate
源码构建工具、生成数据并与入仓版本逐字节比较：

```sh
python3 tools/collation-datagen/regenerate.py --check
```

生成到一个不存在的新目录：

```sh
python3 tools/collation-datagen/regenerate.py --output _tmp/d6/new-collation-data
```

已有数据包可用 `--sources-dir _tmp/d6/downloads`，仍逐一检查大小和哈希。
`CARGO_HOME` 可显式指向已有源码缓存；默认使用本次独立临时缓存。
脚本先 `cargo fetch --locked`，再 `cargo run --locked --offline`，没有下载或调用
预编译 datagen。完成后删除本次下载目录、源码缓存、target 和临时生成目录，
留下下载与生成哈希记录。显式提供的缓存、输入目录和 `--output` 目录不删除。
失败的 hash check 会返回非零，不替换入仓数据。直接 Cargo 调用也拒绝已有输出目录。

本目录有独立 `[workspace]` 和 Cargo.lock，不属于 `src/` workspace。
主 Cargo.lock 的 package 名称、版本和依赖列表均不增加、不升级；只把已有的
`icu_collator_data 2.1.1` 从 crates.io 数据包改为 `src/data/icu_collator_data`。
工具的 source/export/collator 固定 2.1.1；provider 2.3.1 已是主工程锁定的版本。
额外固定 `icu_provider_registry=2.1.1`，因为 2.1.2 registry 宣告了 source 2.1.1
未实现的 IndicConjunctBreak 属性 marker，会导致整个 source crate 编译失败。
这是生成器的版本配套限制，不涉及主工程依赖升级。

## 源数据与兼容性

只使用 `sources.json` 记录的 unicode-org 官方 release 数据：

- ICU `release-78.2`：`icu4x-icuexportdata-78.2.zip`，包含 `collation/unihan/`。
- CLDR JSON `48.0.0`：`cldr-48.0.0-json-full.zip`，提供 locale fallback 数据。
- ICU 同一 release 的 LICENSE；CLDR 的 LICENSE 原文取自上述 JSON archive。

Node v22.22.0 实测报告 ICU 78.2 / CLDR 48.0 / Unicode 17.0。
ICU 78.2 官方发行使用 CLDR 48.1，维护版没有结构改变；不把 48.1 JSON 当作
Node 的 48.0 JSON。这里分别固定 Node 的 ICU 和 CLDR JSON 版本。
`icu_provider_source 2.1.1` 的上游测试常量是 CLDR `48.0.0` /
ICU `release-78.1rc`；没有声称上游已认证 78.2。其 `with_icuexport` / `with_cldr`
接口接受本地 ZIP，collator 读取上述 TOML 数据结构和 Unihan 目录。本仓已经
实际用 2.1.1 解析全部所需数据、导出、在 icu_collator 2.1.1 加载并与 Node
对拍，因此这组数据受当前版本支持，无需升级算法或修改源数据。

参见 [ICU 78.2 release](https://github.com/unicode-org/icu/releases/tag/release-78.2)、
[CLDR JSON 48.0.0 release](https://github.com/unicode-org/cldr-json/releases/tag/48.0.0)、
[SourceDataProvider 2.1.1 源码](https://docs.rs/crate/icu_provider_source/2.1.1/source/src/lib.rs)。

## 输出范围与加载

输出是标准 `BakedExporter` Rust 数据宏，使用
`CollationRootHan::Unihan`、`DataLocaleFamily::FULL` 和 maximal deduplication。
只导出 icu_collator 2.1.1 构造器实际使用的七种 marker：root、tailoring、
diacritics、jamo、metadata、reordering、special primaries。gateway 从环境读取
任意 locale，不能用测量中的 en/zh/ja 白名单缩小语义；因此保留全部排序
locale 的 tailoring 及其 fallback。usage=sort 不选择 search tailorings，沿用
ExportDriver 默认排除 search 的规则。没有生成日历、数字、时区、分词等数据。
normalizer 和其他组件继续使用原来的锁定数据，不混用 implicit Han tailorings。

root 与 tailoring 权重配套生成；只换 root 会把引用 root 的 tailoring 权重留在
另一套 Han 数据中。选择 baked 使 `Collator::try_new` 直接加载 static 数据，
无需 blob 解码器、运行时 IO、新主依赖或自写排序算法。Cargo 的 data crate
patch 生效于已有 compiled provider；gateway 的构造与 fallback 行为保持原样。
生成数据 crate 排除在主 workspace 之外，项目的 `unsafe_code=forbid` 未改。

所有生成文件列在 `src/data/icu_collator_data/SHA256SUMS`；该清单自身的哈希
及源数据登记见 `src/PROVENANCE.md`。`pretty=false` 保持 exporter 的确定格式，
生成结果不依赖 rustfmt 版本。重现使用仓根锁定 Rust 工具链和本工具 Cargo.lock。
完整 Unicode License v3、ICU 附带许可证和 CLDR 原版权声明随生成物保存。
