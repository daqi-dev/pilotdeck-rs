# #132 原生孤立 UTF-16 排序选型

状态：自决 · 待复核；AI 代批，待人复核。

## 卡在哪

ICU4X 2.1.1 的 `compare_utf16` 将孤立代理项解码为 U+FFFD；Node v22.22.0 / ICU 78.2 保留其隐式权重。现有 catalog/session 精确 host 边界因此不能退役。既有共享比较器的 PUA 映射也不能作为本单方案。

## 谁受影响

`model_catalog_list` 的 provider/model 排序、`session_model_get` 的同 sequence 时间戳排序，以及调用共享比较器的产品代码。archive/storage 自身的 host 边界仍按各自任务接续。

## 能否撤回

可撤回：本单源码、Cargo path patch 和调用点可整体回退；不迁移持久数据，不改变输入域。

## 动不动尺子 / 意向

不改冻结装置、数据或原件。验收正本是 #1193 已入库 `src/pilotdeck-gateway/tests/fixtures/surrogate-collation-matrix/oracle.json` 的 100 格原始响应；逐字节一致并由 pairing 独立 observer 证明零业务上游请求。选型不等于 A2/A8 收尾通过。

## 选项与后果

- A：复用依赖树已有的 `icu_collator` 2.1.1，增加原始 UTF-16 比较入口。合法片段仍由原 CollationElements 处理，孤立码元直接使用 ICU 已有隐式权重公式；共用主级至四级比较。公开 API 仅接受 `char`，所以需将 crates.io 同版本源码放入 `src/vendor/icu_collator`，保留 Unicode-3.0 许可证和上游差量。无新版本、无二进制下载；来源 https://crates.io/crates/icu_collator/2.1.1 。Cargo 与 smoke probe 同步 path patch/lock。
- B：自写与 V8 / ICU 78.2 `localeCompare` 等价的比较器。必须自行承担 normalization、contraction、expansion、backwards secondary、case、numeric、locale tailoring 等语义，重复已有算法且增加验证面。
- 保留 host 边界：行为保真，但不完成本单原生化目标；仅作为验证失败时的回退，不用猜测映射冒充完成。

## 建议

选 A。孤立代理项是 CCC=0、无分解/收缩规则的码位，截断合法片段的上下文；直接插入对应隐式 collation element，再由同一比较内核处理所有级别。合法 UTF-16 保持原快路。不得用 PUA/FFFD 占位或码元序近似。

可复用原则：扩大输入表示时复用底层语义算法，直接保留原值，而非映射到可能碰撞的合法字符。断言：100 格原件逐字节一致；PUA/U+FFFD/合法 pair 与孤立项区别、跨片段主次级优先顺序；码元排序、折叠与残留 host 边界负控。

依据：ICU 对孤立代理项按未分配码位处理的说明 https://icu.unicode.org/design/collation/icu-4-6-changes ；具体公式复用锁定版本 `elements.rs::new_implicit_from_char`。本单 Node 实测、实现差量与结果见 `_tmp/20261026-issue-132/RESULT.md`。

## 实测后的必要补充

补充比较矩阵发现仓内 78.2 baked metadata 缺 fr-CA 的 backward-secondary 位（原版 ICU4X 2.1.1 数据有 bits=129，本仓所有 bits 均无 1<<7）。纯合法 `coté/côte` 与带孤立项的重音相对位置都可见，不是 CE 分段造成。为保证移除 host 边界后该 locale 仍保真，选 A 同时暴露原有方向选项，在 `node_collator` 恢复 fr-CA 默认；数据与冻结装置不改。显式 script 的 fr-Latn-CA 经 Node lookup 回退普通法语，emoji/eor 显式 collation 也不套法加默认；补充原件逐对钉住这些行为。此修正可撤回、按真实 Node 判据执行，仍为 AI 代批待人复核。
