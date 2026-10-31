# Surrogate collation matrix（#132 第一步）

`oracle.json` 是 Node v22.22.0 / ICU 78.2 下真实 TS 网关（cc7c4e6，经 tsx 直跑源码）对孤立 UTF-16 代理项排序键的原件：`model_catalog_list`（modelCatalog.ts `items.sort((l,r)=>l.id.localeCompare(r.id))`）与同序 `session_model_get`（TranscriptReader.ts `createdAt.localeCompare`）。#132 明文禁止用 PUA 或替换字符猜映射，本夹具只录原件、不做任何解释性改写。

矩阵：10 个键类 × 2 个方法 × 5 个 locale（en_US / zh_CN / de_DE / sv_SE / ja_JP，resolved en-US / zh-CN / de-DE / sv-SE / ja-JP）。键类见 `generate.mts` 的 `cases` 表：孤立高代理、孤立低代理、高低带交界、U+FFFD 对合法 pair、合法 pair、嵌入式折叠探测（`a\ud800b` / `a\udc00b` / `a�b` —— ICU4X `Collator::compare_utf16` 会把孤立代理折成 U+FFFD 产生平局，Node 不会）、CJK 前缀同款、pair 解释 vs 单元解释、混合 Unicode 全谱、无代理的 locale 对照（其顺序随 locale 改变，证明子进程的 LANG 确实到达了排序）。

实测要点：孤立代理尾段（已赋值字符 < `\ud800` < `\ud801` < `\udc00` < `\udc01` < `�`）五个 locale 相同；已赋值字符的顺序随 locale 改变（en_US 下 `a < ä < Z < 中文`，sv_SE 下 `ä` 排到 `Z` 之后，zh_CN 下 `中文` 排最前，de_DE / ja_JP 下与 en_US 相同），这正是 `locale-control` 键类记录的内容——因此 de_DE 行的「locale 到达」只有 resolvedLocale 佐证，ja_JP 另在 mixed-uniform 上与 en_US 分歧（`中` 排到 `𠀀` 之前）。session 侧倒序输入 + `w<i>` 模型名编码胜者：若未来 native 比较器把所有孤立代理折成同一权重，稳定排序会保留倒序、胜者改变，夹具即失败——catalog 帧的稳定顺序本身检不出折叠，折叠由 session 胜者检出。

生成（ICU 门：生成器与守卫都断言 `process.versions.icu == 78.2`，即本机 homebrew node@22 基线；不符即以 `BLOCKED` 报错，不用别的 Node 静默重录）：

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/surrogate-collation-matrix/generate.mts \
  /absolute/new-output
```

父进程逐 locale spawn `--child` 子进程（剥离继承的 LANG/LC_*，注入单一 locale），子进程录制全部格后 stdout 单行 JSON，父进程校验 resolvedLocale 并汇总写 `oracle.json`（目录 recursive，wx 独占创建）。字节一致性：帧内 scratch 根替换为 `/fixture`，只记构造输入，不记目录树 / PID / 时钟。

Rust 正控：`generator_provenance.rs` 的 `surrogate_collation_matrix_regenerates_identically`（#[ignore]，需 PDRS_BASELINE/tsx，先验 Node v22.22.0 与 ICU 78.2）与结构正控 `surrogate_collation_matrix_covers_locale_and_key_classes`（对全部 10 个键类断言精确 catalog 顺序或尾部/locale 事实，含 de/ja 与 en 相同的钉死）。负控 `negative_controls.py`（本目录）：新文件提交前跑不了，提交后由协调者在宿主跑；宿主 node ICU ≠ 78.2 时整套判 BLOCKED（退出码 3），不计 INVALID。

第二步（icu_collator 补丁 vs 自写等价比较）不在本夹具实现。
