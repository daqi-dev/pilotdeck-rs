# specs/parity/device-patches — 对拍装置补丁登记（append-only）

> 2026-11-08 #1841 首建（AI 代批，待人复核）。本目录与 `diffs/`、`frozen/` 同形态：只追加、
> 子目录内文件不进 `FROZEN.md` 的哈希清单（`check_frozen.py` 只扫 `specs/parity` 顶层文件；
> `--base` 模式只对 `diffs/` 与 `accepted-diffs.md` 做 git 历史 append-only 核，**不覆盖本目录**）。
> 本目录内容**会**进 git 历史：目录秩序分两层保障——登记件 `Patch:`/`Verify:`/`Corpus:` 行
> 钉了 sha256 的文件（patch、verify 脚本、corpus manifest）由 verify 脚本机械校验，不符即
> FAIL-CLOSED；未钉 sha 的文件（登记件、本 README）没有机械 gate，靠 git 历史与人工复核
> （改动走新登记件，不原地改写）。

## 一、device patch 是什么

对上游基线 `resources/PilotDeck`（冻结基点 `cc7c4e65`）内**装置文件**的**已批准偏离**。
它不是产品代码、不是 `scripts/parity` 判分装置，也不是 `accepted-diffs.md` 的差异账本条目：

- `scripts/parity/**` 动的是**判分装置**（比较器、计分、阈值），走 `FROZEN.md` 重登记程序；
- `accepted-diffs.md` / `diffs/<ID>.md` 登记的是**两侧行为差异**（Rust 产品 / 原版缺陷 / 原版不稳定）；
- device patch 动的是**录制与比对装置本体**（`resources/PilotDeck/tools/agent-loop-parity/`）——
  改它等于改「尺子的制造工艺」，同样必须走冻结程序，但不改 G03 判分语义。

## 二、登记程序（五件套，缺一不可）

沿用 `CLAUDE.md` §五「冻结尺子的变更」的批准程序，每条 device patch 必须：

1. **独立 issue**：理由、现场、数据都在 issue 正文，不在 PR 里补理由。
2. **理由**：为什么这是采集口径 / 装置缺陷，而不是产品缺陷或判分语义（须给两侧都踩的证据）。
3. **两家非作者评审**（不同谱系），收敛到无 P0/P1；评审路径写进登记件。
4. **真实数据复算**：不只跑注入负控——必须对在盘真实证据全量复算（对照只证明能抓注入错误，
   真实数据复算才证明不误伤，memory「ruler-real-data」）。
5. **登记**：在本目录新增登记件 + patch + verify 脚本（同名前缀 `YYYYMMDD-<issue>`；证据语料
   固定的单再加一份 corpus manifest，登记件 `Corpus:` 行钉其 sha256，由 verify 逐文件锚定）。

**改判分语义须明确授权**：device patch 不得改变 `scripts/parity/g03_compare.py`、`score.py`、
S 集、分母、k 值的任何判定结果；登记件必须显式声明「G03 判分语义不变」。没有授权的改判
登记阻塞转独立工作。

## 三、登记件字段约定

文件名 `YYYYMMDD-<issue>.md`，字段按序：

```markdown
# YYYYMMDD-<issue> <一句话标题>

- Issue: #<N>（链接）
- 理由: <为什么是采集口径而非产品缺陷；数据支撑>
- 范围: <只动哪个函数 / 哪个 kind；明确不放宽什么（值、条数、同 kind 内顺序、其他 kind）>
- Change: <基线内路径> old=<改动前全量 sha256> new=<改动后全量 sha256>（基线 `cc7c4e65`）
- Patch: <本目录内 patch 文件名>（git diff 格式，只含被改文件）
  sha256=<patch 文件 sha256>
- Verify: <本目录内 verify 脚本名> 与复现命令
  sha256=<verify 脚本 sha256>
- Corpus: <本目录内 corpus manifest 文件名>（证据语料固定的单；整文件 sha256 即证据树整树摘要）
  sha256=<manifest sha256>
- 评审与批准: PR 号 + 两家非作者评审路径（收货后由协调者补终值）
- 复算证据: <证据目录路径>（原始输出，不重录）
- G03 判分语义: 不变（显式声明）
- 后续: <不在本单范围的事项>
- 放置判断: <新文件为什么放本目录、为什么不是 scripts/parity 或 specs/parity 顶层>
```

同日同单再登记用 `YYYYMMDD-<issue>-r2.md`。已登记文件的字节保障以登记件钉的 sha256 为准：
patch / verify / corpus manifest 三类被钉文件由 verify 机械校验（改一个字节即 FAIL-CLOSED），
登记件与其余文件靠 git 历史与人工复核——修改或撤销一律另开新文件，不原地改写。

## 四、应用时机约束

改 `tools/agent-loop-parity/trace.py` 会换 ledger 的 `inputs_sha256`；`score.py::require_ts_source`
要求同一次判分所有 capture 同源。因此 patch 的**实际应用**必须与「一次全量
（TS 参考池 + K=5 两侧同批重录）」对齐、由协调者持对拍锁执行，**不得夹在旧证据中间**
（否则旧证据整批不可用）。worker 只交付 patch + 登记 + 复算，不 apply。

## 五、apply / verify 的确定性命令序列（fail-closed）

apply（协调者，在 §四 的窗口内；`<PD>` = `resources/PilotDeck` 的根）：

```bash
# 1. apply 前核 clean sha：与登记件 Change 行 old 值不符即拒绝（不得套用）
shasum -a 256 <PD>/tools/agent-loop-parity/trace.py
# 2. 应用（patch 用仓库相对路径 a/tools/agent-loop-parity/... 生成，git apply 直接可用）
git -C <PD> apply specs/parity/device-patches/<patch 文件>
# 3. apply 后核 patched sha：与登记件 Change 行 new 值不符即回滚并拒绝
shasum -a 256 <PD>/tools/agent-loop-parity/trace.py
# 回滚：git -C <PD> checkout -- tools/agent-loop-parity/trace.py，然后重核 clean sha
```

verify（任何人可复算；`--evidence` 指向 #1841 证据目录，不重录；登记件与 corpus manifest
默认取 verify 脚本同目录的 `20261108-1841.md` / `20261108-1841-corpus-manifest.txt`，
也可用 `--manifest` 显式指定；**`--out` 与 `--manifest` 都必须落在 `--evidence` 之外**
——判据 0 是精确文件集，结果 JSON 写进证据目录会让下一次复算报「unexpected file」假
FAIL；fix-r3 起这是入口机械断言，落内即 FAIL-CLOSED；fix-r4 起符号链接与父目录口径：
`--out`/`--manifest` 本身是符号链接、或其所在目录落在 `--evidence` 内，均 FAIL-CLOSED
——实际写入位置是未解析路径的父目录，`open("x")` 遇符号链接必 EEXIST、attempt 文件落
在链接旁边）：

```bash
python3 -I specs/parity/device-patches/20261108-1841-verify.py \
  --baseline <PD>/tools/agent-loop-parity/trace.py \
  --patch specs/parity/device-patches/20261108-1841-trace-context-budget-position.patch \
  --evidence <证据目录> \
  --out <结果 json 路径>   # 证据目录之外
```

verify 内部同样 fail-closed：登记件三处 sha 钉（patch / verify 自身 / manifest）与 `Change:`
old/new 不符拒绝；基线 sha 不符拒绝；`git apply --check` / `git apply` 失败拒绝；apply 后
sha 不符拒绝；证据语料对照 corpus manifest **缺失、多余、逐件 sha256 不符均 FAIL-CLOSED**
（不是只核哈希：文件集必须精确相等；fix-r3 起路径集合比对先于任何字节读取——多余文件在
被读进内存之前即拒绝，语料读取按 500 MB 累积上限机械断言）、trace 为空、kind 条数直方图
偏离写死值、census "n/a"
均拒绝。锚定语义为 **validate-once 全链**（fix-r2）：manifest、patch、每个证据文件各只读
一次，哈希校验后判分一律消费已核字节（manifest 条目从已核字节解析、`git apply -` 从 stdin
吃已核 patch 字节、证据语料整树缓存在内存、`load_trace` 从缓存的 scratch 物化副本加载），
校验之后磁盘上的任何变动都改不了比对内容。`--out` 独占创建：已存在则写
`<名>.attempt-N.<扩展名>`，永不覆盖旧结果。退出码 0 才算过。

corpus manifest（证据语料固定的单；在证据目录内生成，输出写到本目录，其整文件 sha256 即
`find . -type f | sort | xargs shasum -a 256 | shasum -a 256` 的整树摘要）：

```bash
(cd <证据目录> && find . -type f | sort | xargs shasum -a 256) \
  > specs/parity/device-patches/<前缀>-corpus-manifest.txt
```
