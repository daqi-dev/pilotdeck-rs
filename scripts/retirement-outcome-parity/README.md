# retirement-outcome-parity — Rust retirement 轮逐方法 outcome / 错误码对照装置（#1477）

判据（P1–P8、阈值 0 处不一致、容差）登记在
`specs/gateway-parity/diffs/A8.6-RETIREMENT-OUTCOME-PARITY-001.md`，**先于任何 Rust
retirement 轮登记**（S1 判据提交，body 含 `criteria-commit: yes`；登记时未打开任何
Rust 侧 retirement 产物，Rust 列一律 `NOT_RUN`）。本目录是这些判据的独立装置：

| 文件 | 作用 |
|---|---|
| `check_outcome_parity.py` | 唯一实现。子命令 `emit`（从 TS 参考轮重生成 / 复核金样）与 `check`（一轮或多轮对照金样）。退出码 0 PASS / 1 FAIL / 2 输入不可用（fail-closed）。 |
| `ts-oracle.json` | 101 行金样：`method` / `shape` / `outcome` / `errorCode` / `wireCode` + `baseline`（revision、Node 版本）+ `rounds`（k、目录名、逐文件 sha256）。 |
| `negative_controls.py` | N1–N17 汇总式控件执行器（见下）。 |

## 金样来源

- TS 基线：`cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，Node v22.22.0，#1355
  `--suite retirement` TS 参考轮 **k=5**。
- 原始数据：`_tmp/20261106-issue-1477/r1/data/round-01..05/driver/{methods.json,frames.jsonl}`
  （目录名与逐文件 sha256 记在 `ts-oracle.json` 的 `rounds` 里——金样来源可核，不是手搓）。
- 分布（对本数据独立复算）：101 方法 / 轮 = 77 ok + 24 error（15 种错误码）；
  `submit_turn` 唯一 `stream`；6 个 `CAPABILITY_UNAVAILABLE`（`manager_browsers`、
  `memory_list`、`memory_wipe`、`snapshot_get`、`snapshot_list`、`snapshot_restore`）；
  5 轮 `(outcome, errorCode)` 零漂移；24/24 error 行线缆 `error.code` 与
  `outcome.errorCode` 一致。

## 重生成（必须与已提交金样逐字节相同）

```sh
python3 -I scripts/retirement-outcome-parity/check_outcome_parity.py emit \
  --oracle /tmp/oracle.json \
  --round _tmp/20261106-issue-1477/r1/data/round-01 \
  --round _tmp/20261106-issue-1477/r1/data/round-02 \
  --round _tmp/20261106-issue-1477/r1/data/round-03 \
  --round _tmp/20261106-issue-1477/r1/data/round-04 \
  --round _tmp/20261106-issue-1477/r1/data/round-05
cmp /tmp/oracle.json scripts/retirement-outcome-parity/ts-oracle.json   # 必须无差异
```

不一致说明金样来源不明或含未登记变异——停下来写清楚哪一轮哪一行不同，不得继续。

## Rust retirement 轮怎么调用

```sh
python3 -I scripts/retirement-outcome-parity/check_outcome_parity.py check \
  --oracle scripts/retirement-outcome-parity/ts-oracle.json \
  --round <rust-round-01> --round <rust-round-02> ...   # 一轮或多轮
```

- rc=0：全部规则 PASS（stdout 是 JSON 报告：逐轮 executed/ok/error/timeout 计数 +
  `violations[]`，每条含 `rule`（P1-COVERAGE / P2-SHAPE / P3-OUTCOME / P4-ERRORCODE /
  P5-UNAVAILABLE-SET / P6-WIRE-CODE / P7-ROUND-STABILITY / P8-FAIL-CLOSED）、`round`、
  `method`、`detail`）。
- rc=1：有 violation，逐条点名方法（+轮次 / 双方码）。
- rc=2：输入不可用（缺文件、解析失败/**非法 UTF-8**、缺字段、`errorCode` 非字符串、`events`
  非整数、**两个轮次目录同名**（P7 以目录名为键）、金样形状不对、**金样 `(method, shape)` 集
  与冻结 fixture `scripts/gateway-parity/fixtures/retirement-methods.json` 不一致**）——
  fail-closed，绝不当 PASS。
- 报告含 `p7Rounds` / `p7Evaluated`：只跑 1 轮时 P7 是空判，`status: PASS` 不得当作
  「跨轮稳定已核」的证据。
- 两个入口都要求 `python3 -I`（断言 `sys.flags.isolated`，同装置族其它入口）；缺 `-I` 与
  其它入口级失败一律 rc=2（输入不可用），不是 rc=1。
- **Rust 轮必须显式跑本装置**：判据尚未接进冻结装置 `session_domain.py`（见「容差与残余」③）。

## 容差与残余（登记在差异条目里，此处只摘要）

- 阈值 = **0 处不一致**；无百分比、无退化名额、无 per-method 白名单。
- 不比较（理由在条目「范围」）：`elapsedMs`（纯计时，95/101 跨轮变动）、
  `error.message` 文本（含轮次绝对路径与 provider 措辞，只要求非空）、
  `events` 计数 / `lastEventType`（只要求 stream ok 有 `final: true` 帧且 `events >= 1`）。
- 成功 response 的 `"error": null`（或字段缺省）等同「无 error」（不判 FAIL）；**除 null /
  缺省之外的任何值都算携带 error**——`error: String` 的 `"boom"`、`error: Vec` 的 `[]`、`0`、
  `true` 一律判 P6 FAIL（「不是 dict 就当没有」会放成假 PASS）。
- 残余：① 文案与帧数不比较；② 只覆盖单轮内方法级行为，hostAudit / entryInventory /
  网络面归 #1478；③ 未接进冻结装置，Rust 轮必须显式跑本装置，否则不得宣称覆盖；
  ④ P6 要求 error 行带同 id `response` 帧，比冻结装置 `EVIDENCE_BACKING` 对 stream
  error 的「任意 receive 帧」更严——只落 event 帧的 stream error 会判 FAIL（响亮失败，
  非假 PASS）；放宽需另立更正件。

## 负控（N1–N17）

```sh
python3 -I scripts/retirement-outcome-parity/negative_controls.py --output <输出目录> \
  [--data _tmp/20261106-issue-1477/r1/data] \
  [--oracle scripts/retirement-outcome-parity/ts-oracle.json]
```

只做数据变异（内存改 + 写临时目录，**不改任何源码文本**）；逐条记
`KILLED` / `SURVIVED` / `INVALID` + 原因，全部跑完再汇总，任一非 KILLED 即非零退出；
`summary.json` 原子发布（先写 `*.tmp` 再 `rename`）。判据分三步：**先核各原始轮对金样
rc=0**（原始数据本就有违规时，注入后的违规不能算作注入被杀）→ **注入是否落地**（重读
落盘文件核对变异事实）→ **归因是否落到被注入对象**（**每条期望规则都** fire 且点名被注入
方法；期望两条规则时任一命中不算 KILLED）。
N10（99/101 unavailable，线缆同步）是 F5 / DS-P0-1 的闭环证据：旧 `UNIFORM_UNAVAILABLE`
语义（全 101 才 FAIL）在该形状下一条规则都不触发。N12（`shape` request↔`stream` 互换）与
N13（`outcome` 改成未登记值）分别给 P2 / P8 两条此前无控件的规则配了具名控件。N14
（stream ok 方法加一条同 id 错误 response，事件侧 `final: true` 仍在；该 stream 若本就有合法
ok response 则改写成错误帧）与 N15（两个 request ok 方法共用同一请求 id、只留一条 response）
封住 P6 的两条假 PASS：矛盾 response 被忽略、一个方法的成功 response 被另一个方法借用；
N16（同 id 两条 response）与 N17（一个方法两条 send）给重复响应/重复请求两条守卫配控件。

执行器**第一件事**是失效同输出目录的旧 `summary.json`（含残留 `*.tmp`）——下游以「文件存在」
为跑完证据，所以必须在读数据 / preflight / 控件构造**之前**失效，任何早退路径都不得留下上一轮
的 PASS。因此这段写在**模块体最前面**，早于 `-I` 断言、早于 `load_checker()`、早于 pycache
前缀设置（这三者都可能让进程起不来），且只依赖 `os` / `sys` / `argparse`（标准库，无副作用）；
它只在 `__main__` 下执行——被 import 时不得拿宿主的 `sys.argv` 去删任何东西。

输出目录只从**显式** `--output` 取，取值**不手抄 argparse 的规则**：用**同一张选项表**
（`PARSER_OPTIONS`）建一个宽松孪生解析器（`build_classifier`），所有选项 `nargs='?'`、
`--output` 改 `action='append'`、未知选项交给 `parse_known_args`——它永不因「缺值」报错，
但**每个 token 的分类与真实解析器一致**，因为分类本身就是运行中那版 argparse 做的
（负数正则、含空格当值、短选项内联、跨版本差异全部自动跟随）。手抄规则是前几轮 P0 的根因：
每加一层镜像（候选集 / 帮助判定 / 负数正则 / 「两版都判成选项的形状」），就在另一版 Python
或另一种拼写上漏收——漏收留旧 PASS 冒充本轮，猜错删掉别人的证据（误删同样算丢数据）。
孪生解析器还顺带消掉「扫描表与解析器选项表两处硬编码」这一类漂移：加选项/加别名只改
`PARSER_OPTIONS` 一处。

`-h` / `--help` 必须在孪生解析器里按真实解析器注册，否则 `'-h x'`（短选项 + 内联值）会被当
普通位置参数、进而被当成目录名去删（Codex r11 P0）。**没有 `--output` 就一个都不删**：`--`
之后的 `--output` 按 argparse 语义只是位置参数，而本执行器没有位置参数（命令直接 rc=2），
孪生解析器同样不收集。**帮助路径同样失效**：命令里显式点了输出目录，之后无论是跑、是 `-h` /
`-hh`、还是 argparse 报错，该目录的旧 PASS 都已报废。重复写多次时每个非空值都失效，且
**逐个都试一遍再退出**：前一个目录不可删（如 `/dev/null`）不让后面显式点名的目录跳过失效
（argparse 只有最后一个生效，但这条命令终究没跑完）。空值（`--output=` / `--output ''`）不
失效且判参数错误 rc=2——`Path('')` 就是当前工作目录，否则会删掉调用者 CWD 里的
`summary.json`。**分类不可用**时（argparse 连导入都失败，或孪生解析器自己都起不来）
改走保守的**字面扫描**：只认 `--output 值` / `--output=值`；内联形式 `=` 之后整段都收
（哪怕 `-old`）；分隔形式只在下一个 token 是**值**时才收。这个判定逐条对着
`_parse_optional` 返回 `None` 的条件写：**除负数匹配那一步外**，分类分支在当前注册选项下
**逐条等价**（「负数」与「含空格」两步相对源码对调，两步都返回「值」故不影响结论；
`allow_abbrev=False` 让长选项那步的缩写查表退化为空，短选项一侧由已注册短选项前缀覆盖）；
负数匹配则按**两版正则形状的交集**取保守子集（残余见下，不是等价）：不以 `-`
开头 · `-` 本身 · 含 ASCII 空格（`if ' ' in arg_string: return None`，两版同源，`--bogus x`
也是值）· argparse 的负数字面量（`-1` / `-1.5` / `-.5` / `-١` / `-１` / `-1.٢`，`\d` 取
**本解释器**的 Unicode 十进制数字 Nd = `str.isdecimal()`；含 3.12 `$` 允许的**一个末尾
换行**，`'-1\n'` 两版都是值）——
而 `-h` / `--help` / `-h x`（短选项内联）/ `--output=x y`（`=` 之前是选项）/ 裸 `--`
在任何一版里都是选项（已注册选项名与短选项都从 `PARSER_OPTIONS` / `HELP_OPTIONS` 推出，
不另抄一份）。实测见 `_tmp/20261106-issue-1477/verify-fix-r15/probe-312.txt` 与
`probe-314.txt`，逐 token 对照见 `verify-fix-r16/sweep.txt` 与 `compare.txt`。遇裸 `--`
停止；下一个 token 是选项
时不消费它（`--output --output=X` 仍收 X，`--output -- --output=X` 不收）。
所以**只漏不多删**：漏的只剩**两版负数正则不同**的形状——3.12 的负数正则是整串
`^-\d+$|^-\d*\.\d+$`，3.14 换成了只锚定开头的 `-\.?\d`，于是 `-1x` / `-1e3` / `-1_000` /
`-0x1f` / `-1.5x` / `-1.` / `-1-2` / `-1..` / `-١x` / `-٣.٤.٥` / `'-1\n\n'` 在 3.14 都是值、
在 3.12 都是选项。字面扫描按「两版**正则形状**的交集」取值（数字表取本解释器），因此这批
一律**不收**——残余（**只在 3.14 存在**，3.12 上残余为空）可以精确写成**三条同时成立**：「`-\.?\d` 在开头匹配」·
「整串十进制正则（含单个末尾换行的 `$` 语义）不匹配」·「token 里**没有 ASCII 空格**」。第三条不能省：`-1 x` 满足前两条，却因
`if ' ' in arg_string` 在两版都是值、扫描也照收，它不是残余（Codex r16 P1）。`\d` 与
`isdecimal()` 一律取**跑着的那个解释器**的 Unicode 表，所以它天然与同版本 argparse 一致：
3.14（Unicode 16）把 `-𐵀`（U+10D40）当值、扫描也收，3.12（Unicode 15）把它当选项、扫描
也不收——这不是残余，只是「两版都收」的交集随 Unicode 版本移动，方向仍是「只漏不多删」。
残余不需要一张会过期的样本表；**两个方向都实测过**（137 615 个 token：3.14 上 41 188 个
残余里「不属于该规则」的 0 个、「满足该规则却不是残余」的 0 个，两版误收都是 0；6 788 个
token 的旧扫描同结论；Codex r16 用独立的 54 278 个 token 复算也同结论；见
`verify-fix-r17/residual-exact.txt` 与 `verify-fix-r16/compare.txt`）。那只在
**3.14 且分类不可用**（argparse 连导入都失败，或孪生解析器自己对某个形状抛
`ArgumentError` / `SystemExit`）时漏掉（漏掉等于不删），方向是「不删」而不是误删
（Codex r17 P1：旧文只写了「导入失败」，比代码里的回退条件窄）。扫完仍按
输入不可用 rc2 退出（旧 PASS 不因启动依赖缺失而留盘，Codex r12 P0）。`--out` 这类缩写
**不是漏**：真实解析器 `allow_abbrev=False`，命令行根本不会跑到写盘那一步。

实测（`_tmp/20261106-issue-1477/verify-fix-r12/classifier_matrix.py`，3.12.13 与 3.14.3 各
一遍）：37 种单 token 形状 + 19 种组合形状，孪生解析器与真实解析器的「收 / 不收」逐个一致，
包括两版**结论相反**的 `-1x` / `-1e3` / `-1_000` / `-0x1f` / `-1.5x` / `-1.`（3.12 选项、
3.14 值）与 `'-h x'` / `-²` / `-.²`（两版都是选项）。

失效只碰本执行器自己的产物名（`summary.json`、`summary.json.tmp`、`summary.json.<pid>.tmp`）：
`summary.json.backup` 之类的既有文件不动，**存活 pid** 的 tmp 也不动（否则会把另一个仍在跑的
执行器的原子发布打掉；并发互斥残余见跟进单）。前置自检除逐轮外还跑一次**跨轮合并**
（`check --round round-01 --round round-02`，`p7Evaluated=true`）：单轮 P7 是空判，不先排除
「参考轮对本身已漂移」，N11 的跨轮归因就不成立。checker 子进程带 600 s 超时（`TimeoutExpired`
收口为 rc2）；注入用常量取自判据实现（`CHECK.CAPABILITY_UNAVAILABLE`），不另抄一份。两个入口
成对设 `sys.pycache_prefix` + `sys.dont_write_bytecode`（同族 `scripts/gateway-parity/*`、
`scripts/parity/*` 口径）：只挡写不挡读时，`spec_from_file_location` 仍可执行陈旧 `.pyc`
（PEP 552 unchecked-hash），负控的 KILLED 归因就不再是「跑当前源码」；没有可写临时目录时回退到
一个**核实不存在**、名字带 16 位随机字节的目录（固定的 `.no-pycache-<pid>` 可预测，等于给
unchecked-hash pyc 留了可投毒的位置）——**启动依赖不得先于旧结果失效**。

入口级失败一律 **rc=2**（输入或环境不可用，不是判据 FAIL）：缺 `-I`、checker 缺失 / 不可读 /
**加载期任何异常**（含 `SyntaxError`：坏合并、半写入）、pycache 前缀候选耗尽、**argv 连宽松
孪生解析器都分不了类**、`emit` 写金样失败（`--oracle=`、不可写路径、目标是目录）、**负控自己的
写盘失败**（只读输出目录 / `N1` 已经是文件 / `summary.json` 已经是目录 / 父件不是目录 /
输出目录下预置的符号链接 / 平台给不出安全打开 / 建完目录又被移走的竞态）。**写盘不跟随符号
链接（范围是 `out_root` 之下）**：从输出根之下**逐段 `O_NOFOLLOW|O_DIRECTORY` 打开**并**持有
目录 fd**，此后所有创建、写入、`unlink`、`replace` 都相对这个 fd（`dir_fd`）做，不再按路径
重新解析——先 `is_symlink()` 再按路径 `open` 只能挡住静态预置的链接，父目录在检查后被换成
链接照样写穿（两家评审 r13 各报一次）。**范围例外如实登记（Codex r14 P1）**：`out_root`
本身按其路径打开，**允许是链接**（macOS 的 `/tmp` 就是指向 `/private/tmp` 的链接），它的
**祖先按路径解析、不在威胁模型内**——调用方点名的目录视作可信输入；**启动失效**同样按路径
`stat` + `unlink`（只删本执行器自己的产物名，`unlink` 不跟随链接），那一段早于 fd 层。
单段部件带分隔符 / `.` / `..` 一律拒绝（`dir_fd` 只对单个名字生效，`'N1/child'` 会按路径
重新解析、中间链接被跟随）。文件层同样 `O_NOFOLLOW`，原子发布的 tmp 再叠 `O_EXCL`。平台缺
`O_NOFOLLOW`/`O_DIRECTORY`/`dir_fd` 时**拒跑**（`require_safe_writes`），不静默退回跟随链接。
`emit` 的 tmp 用 `tempfile.mkstemp` 取不可预测的名字（`<name>.<pid>.tmp` 在世界可写目录里可被
预置符号链接），失败路径先删 tmp 再收口。
`raise SystemExit('消息')` 返回的是 **1**，会被下游当成「判据 FAIL」。

## 为什么不在 `scripts/gateway-parity/`

`scripts/gateway-parity/` 是 hash 冻结的装置目录：往里**新增文件**即被
`check_frozen.py` 的 `unknown_apparatus_files` 判为未登记装置，白名单登记（`A2_SCRIPTS`
/ `DATA_FILES`）与 `FROZEN.md` 哈希行重登记（`--write`）都只归协调者收货步骤——本单
明确不动冻结装置。因此本切片用独立目录 `scripts/retirement-outcome-parity/`，判据走
差异账本条目（`diffs/A8.6-RETIREMENT-OUTCOME-PARITY-001.md`）登记；若要走冻结程序接进
`session_domain.py`（Rust 轮判分自动带上 P1–P8），另立条目由协调者决定。
