# D6 方案 A：Unihan 数据与 Node 排序对齐

2026-10-15：人定方案 A 已实现。ICU4X source/export/collator 2.1.1 从官方
ICU 78.2 icuexportdata 和 CLDR JSON 48.0.0 生成标准 baked 数据，使用
`CollationRootHan::Unihan`，配套生成七种排序 marker。主工程只替换原有
`icu_collator_data 2.1.1` 数据来源，没有增加 crate 或升级算法。
独立工具、版本兼容说明、来源哈希和 Unicode License v3 登记分别见
`tools/collation-datagen/README.md`、`tools/collation-datagen/sources.json` 和
`src/PROVENANCE.md`。

`needs_ts_collation` 与 `TS_COLLATION_REQUIRED` 已撤掉。下方第 3 轮的转发
测试恢复为本地响应逐字节断言；既有 JSON 原件未改，没有登记豁免。
108 条 CJK 请求全部本地逐字节一致；内存配对通道也重放全部 108 条原始请求，
客户端收到原件字节、上游保持静默，包含非最小 126/127 帧长度。
config 的2,728条load诊断继续核对，wire中2,711条本地、17条孤立surrogate排序走精确宿主边界；project-files原件继续通过。
合法Unicode/CJK的108条全部本地；非良构UTF16不属于scalar扫描覆盖，不宣称全输入原生化。

全量扫描扩为 71,712 个码位（含未分配位置）和 19 个混合字符串，覆盖
基本区、Ext-A、Ext-B、两个兼容区、部首补充、康熙部首，以及 ASCII、重音、
组合字符和 emoji。九种环境组合和十个显式 locale 共 19 轮，都检查完整排序
输出、对“中”的比较符号、每对相邻排序项的比较符号（包括相等分组），零差异。
完整结果与 A2 六个本地 method 回归命令见本 worktree `_tmp/RESULT.md`。
在线 A2 回归需要 listen，由协调者执行。

```sh
python3 tools/collation-datagen/regenerate.py --check
python3 src/pilotdeck-gateway/tests/scan_cjk_collation.py _tmp/d6-fresh-scan
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway \
  cjk_requests_match_node_byte_for_byte --locked --offline
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway \
  cjk_local_responses_match_node_without_upstream_requests --locked --offline
```

## 历史：PR #96 排序根因与方案 C


PR #96 第 3 轮的历史状态：D6 方案 C 已实现，含非 ASCII 排序键的请求原样转发给 TS。
以下根因、扫描统计与 74 PASS / 34 FAIL 是第 2 轮的历史实测，原件保持不变；
它们不是第 3 轮的本地应答结果。A / B 仍等人定，未登记已接受差异。

基线：PilotDeck `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`；Rust 起点 `85e21c3`。
Node `v22.22.0` / ICU `78.2` / CLDR `48.0` / Unicode `17.0`。
Rust `icu_collator=2.1.1` + `icu_collator_data=2.1.1` compiled provider：
包内 `src/lib.rs` 标注 CLDR `48.0.0`、ICU `release-78.1rc`。

## 结论

根因是 **root Han 排序数据变体不同**，不能仅归因于 ICU 版本号不同：
Node 的 root 行为是 Unihan 部首笔画序；ICU4X 2.1 compiled data 的 root
行为是 implicit Han Unicode 分配序。语言定制未覆盖的汉字继承这套根数据。

[ICU Data Build Tool](https://unicode-org.github.io/icu/userguide/icu_data/buildtool.html#collation-ucadata)
明确区分 `ucadata-unihan` 与 `ucadata-implicithan`，ICU 默认采用前者。
[ICU4X 2.1 SourceDataProvider](https://github.com/unicode-org/icu4x/blob/icu%402.1.0/provider/source/src/lib.rs#L452)
的 `CollationRootHan` 默认是 `Implicit`，另有 `Unihan` 数据生成选项；
[该版本数据读取源码](https://github.com/unicode-org/icu4x/blob/icu%402.1.0/provider/source/src/collator/mod.rs#L91)
按这个选项选择不同的导出目录。本地 baked 数据的实测与 implicit 分配序一致。
这些资料与实测共同支持根因判断；没有下载 ICU/CLDR/Unihan 数据包。

`node_locale` 在本轮九个环境下均与 Node resolved locale 一致。
`LC_ALL > LC_MESSAGES > LANG` 的优先级也一致；缺省值为 `en-US`。
中文默认分别与显式 pinyin / stroke 的输出一致，日文默认选择也一致。
显式 `ja-u-co-unihan` / `zh-u-co-unihan` **仍有差异**；
`en-US-u-co-unihan` 在 Node 中被剔除，resolved collation 为 `default`，
因此给英语硬塞 `co=unihan` 不能修复。

ICU4X 2.1 能在生成数据时选择 Unihan，但当前依赖只内置一份
`CollationRootV1`，不是 `CollatorPreferences.collation_type` 的运行时开关。
本地没有与 Node 对应的 root Unihan baked 数据。按任务限制，本轮不加 crate、
不下载或生成替换数据、不改 locale 映射。ICU 78.1rc 与 78.2 的来源版本差仍应
记录，但无需靠这个差异来解释已证实的 implicit / Unihan 根数据差异。

## 最小实测

每行都是全新进程；开始前删除所有继承的 `LC_*` / `LANG`，再设置表中值。
执行 `['中','㐀'].sort((a,b)=>a.localeCompare(b))`，并记录
`Intl.Collator().resolvedOptions()`。五行 resolved collation 均为 `default`，
usage=`sort`、sensitivity=`variant`、numeric=false、caseFirst=`false`。

| LANG | Node resolved locale | Node | Rust node_collator |
|---|---|---|---|
| C | en-US | 㐀、中 | 中、㐀 |
| en_US.UTF-8 | en-US | 㐀、中 | 中、㐀 |
| zh_CN.UTF-8 | zh-CN | 中、㐀 | 中、㐀 |
| zh_TW | zh-TW | 中、㐀 | 中、㐀 |
| ja_JP | ja-JP | 中、㐀 | 中、㐀 |

另录缺省环境、LC_MESSAGES 覆盖 LANG、LC_ALL 覆盖两者、LC_ALL 指向中文。
环境决定**是否复现这个特定两字案例**；不能据此推断某个 locale 全部 CJK 排序一致。
其他评审的 162 例未见差异不与这里矛盾；缺少其原始输入，无法认定它们覆盖
了根数据未定制的扩展汉字或基本区例外。

## 差异范围

`cjk-node22.json` 含九个环境 × 12 个真实 WS response（catalog 3 个、files 9 个），
共 108 个原件。每个环境有基本区、Ext-A、Ext-B、BMP 兼容区、补充兼容区、
ASCII、重音字母、emoji；同名字符同时作为模型键和真实文件名。
保留完整响应，文件 stat 使用实测值，未用排序归一化抹掉差异。

本轮结果为 **74 wire PASS / 34 wire FAIL**。五个指定 LANG：
英文两行各 6 FAIL；中文与日文三行各 2 FAIL。
`catalog-mixed`、`files-mixed` 在所有九行失败。
英文环境中最小文件列表及第一页、第二页均失败，证明影响分页边界。
36 个基本区控制 response 均 PASS；这个控制集不是基本区完全一致的声明。

`cjk-scope.json` 对以下五个完整码位区段做扫描，共 71,360 个 scalar：
Ext-A U+3400–U+4DBF、基本区 U+4E00–U+9FFF、Ext-B U+20000–U+2A6DF、
兼容区 U+F900–U+FAFF、补充兼容区 U+2F800–U+2FA1F。
包含区段中的未分配位置。没有扫描 Ext-C 及其后扩展，也没有穷举所有字符串或码位对。

| LANG | Rust 排序相邻项在 Node 输出中的索引逆序数 | 与「中」比较的符号不同：Ext-A / 基本区 / Ext-B / 兼容区 / 补充兼容区 |
|---|---:|---|
| C | 940 | 6 / 6 / 92 / 0 / 1 |
| en_US.UTF-8 | 940 | 6 / 6 / 92 / 0 / 1 |
| zh_CN.UTF-8 | 603 | 0 / 0 / 0 / 0 / 0 |
| zh_TW | 562 | 0 / 0 / 0 / 0 / 0 |
| ja_JP | 831 | 0 / 0 / 0 / 0 / 0 |

“索引逆序数”是输出序列的统计，不是全体不一致码位数，也不是全体不一致码位对数。
与「中」比较的符号相同不能推出两个字符互相比也相同。
英文下这 6 个 Ext-A 是 U+3400–U+3404、U+4DB6；6 个基本区是
U+4E2C、U+4E3D、U+4E90、U+5344、U+9FD6、U+9FD7；补充兼容区是 U+2F800。
92 个 Ext-B 的精确区间及各区段相邻逆序分类在 `cjk-scope.json` 中。

范围不限于 Ext-A 和扩展/基本区混排：英语 root 的基本区内部也有
`世(U+4E16)` / `丗(U+4E17)` 等差异；日文有
`丬(U+4E2C)` / `丮(U+4E2E)`；简体中文有 U+9FA6 / U+9FA7；
繁体中文有 U+9FB5 / U+9FB6。兼容字符有规范化映射，可能继承被映射汉字的
排序差异；BMP 兼容区相对于「中」的差异数为零，仍出现与其他基本区/Ext-A 字符
的相对顺序差异。具体示例保留在 scope fixture 的 `examplesByBlock`。

## 已知差异候选登记文本

- 候选 ID：`A4-CJK-ROOT-HAN-001`。
- 状态：**待人批**。这是登记草案，不是已批准豁免；CJK parity 仍为 FAIL。
- 方法：`model_catalog_list`、`project_files_list`。
- 触发：id / relativePath 的比较到达采用 root Han 权重的字符；默认 locale
  为英文、简体中文、繁体中文、日文均已证实有案例。显式 Unihan 也受影响。
- 差异：Node ICU 78.2 root Unihan 与 ICU4X 2.1.1 compiled implicit Han
  的排序顺序不同。已证实基本区、Ext-A、Ext-B、兼容区及补充兼容区有相关差异，
  不宣称上述整个区段每个字符都不一致，不宣称其他 locale / 扩展区已排除。
- 影响：catalog items 次序；files items 次序和分页成员/页边界；扫描遇到
  symlink/visited 或资源上限时顺序也可能影响结果，但本轮未实测该组合。
- 边界：不改 payload 内容、配置验证、模型可用性或路径安全判断；本轮确认
  的错误是排序及分页，不把未测扫描组合描述为已复现。
- 处置候选：单独评估改用对应 Unihan 根数据的来源、大小、版本、维护成本，
  再重新跑所有 locale 和两个 RPC；未经人批不登记 accepted-diffs、不修改冻结尺子。
- 当前约束：不增加 crate、不下载数据；本轮只保留证据、诊断和控制测试。

影响概率是工程判断，不是生产统计。本仓固定内置 catalog 有 12 providers /
57 models，全部 id 为 ASCII；displayName 不参与排序，因此内置模型本身不触发。
用户自定义中文 provider/model id 可以触发，预计较少，但无法给出比例。
只读基线的 3,621 个 tracked 路径全为 ASCII；这不能代表用户项目。
中文文档/源文件/数据集中的中文文件名更可能遇到 CJK 排序差异，且无需 Ext-A
才能触发。不能因“罕见汉字”将这个文件列表问题评为零影响。

## 复现与验证口径

仓根运行，输出目录必须为未使用的新目录。录制器自己隔离 HOME/XDG/TMPDIR、
设置 `SKIP_BROWSER_OPEN=1` 和 `PILOTDECK_SKIP_BROWSER_OPEN=1`，
所有 locale 均启动全新 Node。source hashes 与 baseline commit 在原件内。

```sh
python3 src/pilotdeck-gateway/tests/probe_cjk_collation.py \
  /Users/da/ws/pilotdeck-rs/resources/PilotDeck _tmp/cjk-fresh-oracle
python3 src/pilotdeck-gateway/tests/scan_cjk_collation.py _tmp/cjk-fresh-scope
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway \
  cjk_requests_forward_to_ts --locked --offline
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway \
  cjk_fallback_reaches_upstream --locked --offline
```

第 3 轮将旧 `cjk_locale_and_basic_controls` / ignored 的
`cjk_full_node_parity_diagnostic` 替换为正常运行的转发断言。
九个环境 × 12 请求共 108 条都必须 Forward；locale 解析断言仍保留。
配对通道测试重放这 108 个原件，上游必须收到完整原始 masked WS 帧，
包括非最小 126/127 长度形式；客户端在 TS 回包前无本地响应，随后收到完整
Node 原件 response。通道使用内存 duplex 与模拟 TS 上游，不启动网络服务。

D6 判定只用一个 `needs_ts_collation` 函数，任何 UTF-16 单元 > 0x7f 都触发，
不划“安全非 ASCII”子集。catalog 在 provider/query 过滤后检查完整 id，
Auto 插入前排序；displayName 不是排序键。文件遍历在每次 children 排序前
检查名字（即使 query/includeDirs 最后不返回该项），最终 relativePath 排序
也检查，分页在检查之后。校验已提前报错的请求保持原有本地错误优先级。
`project_file_read` 不参与排序，仍本地应答。

ASCII、本地/转发混合同 method、汉字/重音/emoji、过滤排除非 ASCII id、
非 ASCII displayName、非 ASCII query 但 ASCII 文件名、孤立 surrogate、
非 ASCII 文件名读取均有正常运行的测试。这里的 PASS 表示回退路径正确，
不表示 ICU4X Han 根数据已经对齐；A8 退役 TS 前仍须解决 D6 A / B。

冻结 A2 `compare.py` 的 `--local-method` 是 method 级别：只要上游出现任何
该 method 就报 `G7.no-forward`，不支持按请求标记本地归属。因此本地化回归
须用装置现有 ASCII fixture（`run_round.py` 创建 `note.txt` 与
`parity/deterministic`）。含非 ASCII 的混合录制不能声明这两个 method 全本地；
若不声明它们，A2 又要求每个未声明的只读 method 至少出现一条上游请求。
不能仅删除所有 local flags 来验证仍有其他本地 methods 的录制。
完整协调者回归命令见本 worktree `_tmp/RESULT.md`「第 3 轮」。尺子未修改。

原生非良构UTF16排序跟进 #132，Linux真实非法UTF8 dirent端到端跟进 #133，均前置A8/#127收尾。22组Node Buffer解码原件仅证明共用CStr lossy解码结果；macOS实际建名返回EILSEQ(92)。#133 的 Linux 门控测试已入库（`../project-files-linux-dirent/`，需 cb8 主机运行，证据由协调者产出后收口）；在其实跑证据落盘前，单元解码核验仍不能视为真实文件系统验收。
