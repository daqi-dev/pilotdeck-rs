# retirement-outcome-parity — Rust retirement 轮逐方法 outcome / 错误码对照装置（#1477）

判据（P1–P8、阈值 0 处不一致、容差）登记在
`specs/gateway-parity/diffs/A8.6-RETIREMENT-OUTCOME-PARITY-001.md`，**先于任何 Rust
retirement 轮登记**（S1 判据提交，body 含 `criteria-commit: yes`；登记时未打开任何
Rust 侧 retirement 产物，Rust 列一律 `NOT_RUN`）。本目录是这些判据的独立装置：

| 文件 | 作用 |
|---|---|
| `check_outcome_parity.py` | 唯一实现。子命令 `emit`（从 TS 参考轮重生成 / 复核金样）与 `check`（一轮或多轮对照金样）。退出码 0 PASS / 1 FAIL / 2 输入不可用（fail-closed）。 |
| `ts-oracle.json` | 101 行金样：`method` / `shape` / `outcome` / `errorCode` / `wireCode` + `baseline`（revision、Node 版本）+ `rounds`（k、目录名、逐文件 sha256）。 |
| `negative_controls.py` | N1–N17 负控汇总式执行器（见下）。 |
| `fixtures/` | 金样的**入库输入**（5 轮 `methods.json` + `frames.jsonl` 切片，`du -sk` 1568 KiB）。来源、切片规则与新旧 sha256 对照见 `fixtures/PROVENANCE.md`。 |
| `self_check.py` | 装置守卫闸门（体积预算 / `emit` 复现 / `check` 5 轮 / 负控全 KILLED / 输出目录锁 / 敌意写目标 / CI 接线——ci.yml 与 pr-gate.yml 两份 workflow 都巡走），CI 与本机共用；`--self-check-controls` 是它自己的变异负控。 |

## 金样来源

- TS 基线：`cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，Node v22.22.0，#1355
  `--suite retirement` TS 参考轮 **k=5**。
- 原始数据（仓外，未跟踪）：`_tmp/20261106-issue-1477/r1/data/round-01..05/driver/{methods.json,frames.jsonl}`。
- **入库夹具（判据与 CI 的实际输入）**：`fixtures/round-01..05/driver/{methods.json,frames.jsonl}`
  ——干净检出即可跑。`frames.jsonl` 逐字节照搬原始件；`methods.json` 同样**逐字节照搬**
  （r7 前曾把判据不消费的容差字段去掉、5 轮因此字节相同——P7 判据本身没变松：仍逐轮
  比对解析后的 `(outcome, errorCode)`、`p7Evaluated` 仍为 true，但「5 份独立采集」塌缩成
  同一份数据的五个副本，跨轮稳定性变成「数据与自身比对」，评审 r7 P1 已改回照搬）。
  「原始 ↔ 入库 sha256」对照见
  `fixtures/PROVENANCE.md`。逐文件 sha256 记在
  `ts-oracle.json` 的 `rounds.files` 里——金样来源可核，不是手搓。5 轮的
  `methodsSha256` 与 `framesSha256` **各自互不相同**（5 份独立采集），非退化由
  `self_check.py` 的 `check` 用例**机检**（对入库夹具复算 5 轮 `methods.json` /
  `frames.jsonl` 的 sha256 并断言互异，r8；切回去掉容差字段的切片当场红）。
  互异只排除「同一份数据的多个副本」这一退化，排不掉「五份互异但都被换过」——r9 起
  `methods.json` 的复算值须**逐轮等于** `fixtures/PROVENANCE.md` 登记的原始采集
  sha256；r10 P1 起 `frames.jsonl` **同口径**（此前 frames 只有金样
  `rounds.files[].framesSha256` 的自指锚——金样本身从入库件 emit 出，改一个判据
  不消费的字节全绿；现 PROVENANCE 逐轮登记原始哈希）。表解析不出 / 行数对不上 /
  任一字节改动都当场红，负控 `control_check` 三个方向各验证一次（methods 塞字节 /
  frames 塞字节 / 删表行）。
  解析后的 `(outcome, errorCode)` 值集合断言在 P7 判据下则是恒真（5 轮解析值必须相同，
  同片与互异数据给出同一集合）——非退化的机器证据是**字节级同片 + 原始 sha256 交叉核对**，
  不是解析值集合。
- 分布（对本数据独立复算）：101 方法 / 轮 = 77 ok + 24 error（15 种错误码）；
  `submit_turn` 唯一 `stream`；6 个 `CAPABILITY_UNAVAILABLE`（`manager_browsers`、
  `memory_list`、`memory_wipe`、`snapshot_get`、`snapshot_list`、`snapshot_restore`）；
  5 轮 `(outcome, errorCode)` 零漂移；24/24 error 行线缆 `error.code` 与
  `outcome.errorCode` 一致。

## 重生成（必须与已提交金样逐字节相同）

```sh
F=scripts/retirement-outcome-parity/fixtures
python3 -I scripts/retirement-outcome-parity/check_outcome_parity.py emit \
  --oracle /tmp/oracle.json \
  --round $F/round-01 --round $F/round-02 --round $F/round-03 \
  --round $F/round-04 --round $F/round-05
cmp /tmp/oracle.json scripts/retirement-outcome-parity/ts-oracle.json   # 必须无差异
```

不一致说明夹具或金样有一个被改过而另一个没跟上（或有人手改金样）——停下来写清楚哪一轮
哪一行不同，不得继续。重采全量 TS 数据后用同一命令重生成，并把新旧 sha256 对照补进
`fixtures/PROVENANCE.md`。

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
- 三个入口（`check_outcome_parity.py` / `negative_controls.py` / `self_check.py`）都要求
  `python3 -I`（断言 `sys.flags.isolated`，同装置族其它入口）；缺 `-I` 与
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
  [--data scripts/retirement-outcome-parity/fixtures] \
  [--oracle scripts/retirement-outcome-parity/ts-oracle.json]
```

`--data` 默认指向**入库夹具**（相对脚本位置，不依赖 cwd，干净检出即可跑，CI 同此）；
`--data` 仍可覆盖，用于重采后的新参考轮。`--oracle` 默认同目录金样。

**输出目录互斥（#1809 条目 4）**：整轮运行（权威失效 → 控件构造 → 逐条写 `N*/driver/*`
→ 发布 `summary.json`）都在一把 `fcntl.flock` 独占锁下，锁文件是
`<输出目录>/.retirement-outcome-parity.lock`。第二个进程**阻塞等待**（不是失败、不是
静默跳过），等锁期间在 stderr 打一行 `waiting for output lock …`。理由：并发跑两次是
合法用法（CI 与本机各一次、审计者边跑边看），明确失败会被误读成数据问题；阻塞既能保证
「一次只有一个进程在写」，又不会把合法的并发调用打红。缺 `fcntl` / `O_NOFOLLOW` 的平台
按 rc2 拒跑（与 `require_safe_writes` 同口径），不静默退化成「没锁」。锁文件必须打开成
**普通文件**（`fstat` + `S_ISREG`）且用 `O_NONBLOCK` 打开：预置同名 FIFO 时不带
`O_NONBLOCK` 的只读 `open` 会一直等写端（`O_NOFOLLOW` 拦不住 FIFO），整个装置连同 CI job
一起挂死；预置目录则会被当成「锁」静默通过。三者（FIFO / 目录 / 符号链接）都在
`self_check.py` 的 lock 用例里各造一次，必须快速 rc2（样本登记在 `self_check.py` 的
`LOCK_HOSTILE_PLANTS`，条数 3 由 `EXPECTED_LOCK_HOSTILE_PLANTS` 断言——删一条敌意形态
必须当场红，而不是少测一种还全绿）。`emit` 写金样时用
**同一把** `output_lock`（锁金样所在目录），两个入口不会在同一个目录里交错写。负控执行器里
**失效旧 summary 的守卫 fd**（`negative_controls.py` 的 `_output_lock_guard_fd`）用与真锁
**同一口径**打开同一把锁文件（`O_RDONLY|O_CREAT|O_NOFOLLOW|O_NONBLOCK` + `fchmod 0644` +
`S_ISREG`，r8 对齐——此前它 `O_RDWR`+0600 自建，跨账号共享目录里会挡住后来者的只读打开、
不带 `O_NONBLOCK` 的打开在预置同名 FIFO 上挂死）。「同一口径」不只是文字声明（r9）：
`check_lock` 对三种敌意形态各跑一次**守卫 fd 探针**（子进程预置敌意锁文件 + 一份已发布的
`summary.json`，断言探针不挂死、已发布 summary 不被误删）；`control_lock` 再对守卫 fd
本身做三条变异——去 `O_NONBLOCK`（预置 FIFO 上挂死必红）、去 `S_ISREG` 判定（预置目录
锁下已发布 summary 被删必红）、去 `O_NOFOLLOW`（预置符号链接锁下同红）。三条变异登记在
`self_check.py` 的 `GUARD_FD_MUTATIONS`，条数 3 由 `EXPECTED_GUARD_FD_MUTATIONS` 断言
（#2436）——外层预算的锁趟数算式 `LOCK_CHECK_PASSES − len(GUARD_FD_MUTATIONS)` 从登记簿
推导，删一条变异两边一起降、不等式仍绿；条数钉死让登记簿缩水当场红，而不是少测一种
还全绿。锁文件名的字面副本共三份（checker /
负控守卫 / self_check 控件），`self_check` 的 lock 用例断言三份都在源码里——改常量只改两处
当场红（r8）。

正控机制（数据变异、断言「应当 PASS 的性质」，判 HELD / BROKEN / INVALID；rc 映射
与负控同口径：checker rc=0 → 核断言，rc=1 → BROKEN，rc∉{0,1} → INVALID）保留在
执行器与 `summary.json` 的 `positives` / `positiveCounts` 里，**当前注册数为 0**——
A8.6-R1 的延期正控 PC1–PC2 已随 #2294 always-on 实现落地按反转条款撤回
（`diffs/A8.6-RETIREMENT-OUTCOME-PARITY-001-R2.md`，见汇总表「负控 N1–N17」行）；
将来再登记正控时改 `EXPECTED_POSITIVE_IDS` 即可，任一条非 HELD 即整轮非零退出。

**名册 / 分母断言（#2378）**：控件的分母是 `check_outcome_parity.py` 里登记的**字面
ID 全集**（`EXPECTED_CONTROL_IDS` = N1–N17、`EXPECTED_POSITIVE_IDS` =
空集），不是 `len(controls)`。条数拦不住改名（一条改名成已有 ID 后一重复一
缺失、条数不变）；执行器在**构造后**与**汇总前**各核一次
`sorted(实际 ids) == sorted(名册)`（排序后逐元素比对，重复条目同样不等），不符
即 rc=2 拒跑并拒绝发布 summary——分母缩水是「拒绝」，不是「剩下的全绿」。判决
词表外的值（含 None）在汇总前一律**折叠记 INVALID**（改在 record 本身上，原
判决与原因保留在 reason 里可见），不静默不计数——否则 `SURVIVED == 0 and
INVALID == 0` 会对词表外判决空真判 PASS。`status` 由 counts **整字典等于
期望值**决定（`{KILLED: 17, SURVIVED: 0, INVALID: 0}`），不再是「两个零」的空真式
判定；`self_check` 的 `check_negctl` 对 summary 做同一断言（整字典 + ID 名册），两端
同源读 `check_outcome_parity` 的登记值。

只做数据变异（内存改 + 写临时目录，**不改任何源码文本**）；逐条记
`KILLED` / `SURVIVED` / `INVALID` + 原因，全部跑完再汇总，任一非 KILLED 即非零退出；
`summary.json` 原子发布（先写 `*.tmp` 再 `rename`）。判据分三步：**先核各原始轮对金样
rc=0**（原始数据本就有违规时，注入后的违规不能算作注入被杀）→ **注入是否落地**（重读
落盘文件核对变异事实）→ **归因是否落到被注入对象**（**每条期望规则都** fire 且点名被注入
方法；期望两条规则时任一命中不算 KILLED）。前置自检覆盖 `--data` 下**全部**轮次（逐轮 +
一次跨轮合并；#1809 条目 5——此前写死 `round-01`/`round-02`，换 `--data` 后
`round-03..05` 的 PASS 不在自检范围）。
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
`summary.json`。**分类不可用**时（argparse 连导入都失败，或孪生解析器自己对某个形状抛 `ArgumentError` / `SystemExit`）
改走保守的**字面扫描**：只认 `--output 值` / `--output=值`；内联形式 `=` 之后整段都收
（哪怕 `-old`）；分隔形式只在下一个 token 是**值**时才收。值是四类之一：不以 `-`
开头 · `-` 本身 · 含 ASCII 空格（`if ' ' in arg_string: return None`，两版同源，`--bogus x`
也是值；但**没先命中选项分支**才算——`-h x` / `--output=x y` 已被下面的选项分支截获）·
argparse 的负数字面量（`-1` / `-1.5` / `-.5` / `-١` / `-１` / `-1.٢`，`\d` 取
**本解释器**的 Unicode 十进制数字 Nd = `str.isdecimal()`；含 3.12 `$` 允许的**一个末尾
换行**，`'-1\n'` 两版都是值）——
而 `-h` / `--help` / `-h x`（短选项内联）/ `--output=x y`（`=` 之前是选项）/ 裸 `--`
在任何一版里都是选项（已注册选项名与短选项都从 `PARSER_OPTIONS` / `HELP_OPTIONS` 推出，
不另抄一份）。这几类逐条对着 `_parse_optional` 返回 `None` 的条件写：**除负数匹配那一步
外**，分类分支在当前注册选项下**逐条等价**（「负数」与「含空格」两步相对源码对调，两步都
返回「值」故不影响结论；`allow_abbrev=False` 让长选项那步的缩写查表退化为空，短选项一侧
由已注册短选项前缀覆盖）；只有**负数那一类**是保守子集——按**两版正则形状的交集**取值
（3.12 整串、3.14 只锚开头），不是等价（残余见下）。
实测见 `_tmp/20261106-issue-1477/verify-fix-r15/probe-312.txt` 与
`probe-314.txt`，逐 token 对照见 `verify-fix-r16/sweep.txt` 与 `compare.txt`。遇裸 `--`
停止；下一个 token 是选项
时不消费它（`--output --output=X` 仍收 X，`--output -- --output=X` 不收）。
所以**只漏不多删**：漏的只剩**两版负数正则不同**的形状——3.12 的负数正则是整串
`^-\d+$|^-\d*\.\d+$`，3.14 换成了只锚定开头的 `-\.?\d`，于是 `-1x` / `-1e3` / `-1_000` /
`-0x1f` / `-1.5x` / `-1.` / `-1-2` / `-1..` / `-١x` / `-٣.٤.٥` / `'-1\n\n'` 在 3.14 都是值、
在 3.12 都是选项。字面扫描的**负数那一步**按「两版**正则形状**的交集」取值（数字表取本
解释器），因此这批一律**不收**——残余（**只在 3.14 存在**，3.12 上残余为空）可以精确写成**三条同时成立**：「`-\.?\d` 在开头匹配」·
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
执行器的原子发布打掉）。并发互斥由 `<输出目录>/.retirement-outcome-parity.lock` 的独占
`flock` 保证（见上节）；锁在**权威失效之前**拿到，所以后到者不会在先到者发布 `summary.json`
的窗口里读到半成品。前置自检逐轮覆盖 `--data` 下的**全部**轮次，另跑一次**跨轮合并**
（`check --round <全部轮次>`，`p7Evaluated=true`）：单轮 P7 是空判，不先排除
「参考轮对本身已漂移」，N11 的跨轮归因就不成立。checker 子进程带 600 s 超时（`TimeoutExpired`
收口为 rc2）；注入用常量取自判据实现（`CHECK.CAPABILITY_UNAVAILABLE`），不另抄一份。三个入口
（`check_outcome_parity.py` / `negative_controls.py` / `self_check.py`，含 `self_check` 的
`check_lock` 在子进程里再加载 checker 的那一处）都成对设
`sys.pycache_prefix` + `sys.dont_write_bytecode`（同族 `scripts/gateway-parity/*`、
`scripts/parity/*` 口径）：只挡写不挡读时，`spec_from_file_location` 仍可执行陈旧 `.pyc`
（PEP 552 unchecked-hash），负控的 KILLED 归因就不再是「跑当前源码」；没有可写临时目录时回退到
一个**核实不存在**、名字带 16 位随机字节的目录（固定的 `.no-pycache-<pid>` 可预测，等于给
unchecked-hash pyc 留了可投毒的位置）——**启动依赖不得先于旧结果失效**。

入口级失败一律 **rc=2**（输入或环境不可用，不是判据 FAIL）：缺 `-I`、checker 缺失 / 不可读 /
**加载期任何异常**（含 `SyntaxError`：坏合并、半写入）、pycache 前缀候选耗尽、**argv 连宽松
孪生解析器都分不了类**、`emit` 写金样失败（`--oracle=`、不可写路径、目标是目录）、**`summary.json`
发布失败**（只读输出目录 / `summary.json` 已经是目录 / 平台给不出安全打开）。**负控写盘失败按层
分档（#2573）**：控件循环里逐控件的输出写盘失败（`N1` 已经是文件 / 父件不是目录 / 输出目录下
预置的符号链接 / 建完目录又被移走的竞态 / 平台给不出安全打开）只把**该控件**记 INVALID、其余
控件照跑、总 rc=1；只有最后的 `summary.json` 发布失败才整体 rc2。**写盘不跟随符号
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

## CI（#1809）

`.github/workflows/ci.yml` 的 `retirement-outcome-parity` job 每次 PR / 合入 main / merge
queue 都跑（只受 `scope` 的 docs-only 分类控制，不做 paths 过滤——纯 Python，本机实测
七项全跑 ~3 s、变异自检 ~1 s）：

| 步骤 | 判据 |
|---|---|
| 安装钉死 PyYAML（#2017-A） | 装出 `pyyaml==6.0.3` venv 并前置 PATH，供下方 `python3 -I` 闸门使用 |
| `emit` 复算 + `cmp` 金样 | 夹具能逐字节复现已提交金样 |
| `check` 5 轮 | P1–P8 全 PASS，`p7Evaluated=true` |
| 负控 N1–N17 | 负控全 KILLED（任一 SURVIVED / INVALID / BROKEN 即红）。A8.6-R1 的延期正控 PC1–PC2 已随 #2294 实现落地按反转条款撤回（A8.6-…-R2），正控注册数现归 0 |
| `self_check.py`（七项全跑） | 体积预算 · `emit` 复现 · `check` 5 轮 · 负控 · 输出目录互斥 · 敌意写目标 · CI 接线，七条守卫本身都还在 |
| `self_check.py --self-check-controls` | 给上面这些守卫造违反样本，它们必须**变红**（接线守卫自己的变异自检） |

外层 `timeout-minutes` 必须**大于**全 job 有效内部最坏和 + 开销余量（r11 P0）。
判据是 `RETIREMENT_JOB_TIMEOUT_MINUTES` × 60 > `INTERNAL_WORST_CASE_SECONDS` +
`JOB_OVERHEAD_MARGIN_SECONDS`，由 `self_check.py` 的 `retirement_budget_rows`
遍历 ci.yml 实际声明的全部 steps 求值：各步贡献记录与 `RETIREMENT_STEP_ORDER`
双向互证后，累加导出 `INTERNAL_WORST_CASE_SECONDS`。未知形态没有预算规则即红；
遗漏一项贡献或多算一个幽灵 step 也红，不再手列全 job 加数。下表与正文的量级数字
是**给人看的近似值**（#2454）——判据以 `retirement_budget_rows` 求值为准，本
README 与 ci.yml 注释都不维护第二份精确和（改推导常量后手抄数字只会静默烂掉）。

| step | 上限来源 | 内部贡献 |
|---|---|---|
| checkout / 安装 PyYAML / upload | `JOB_OVERHEAD_STEPS` 显式共享 overhead 桶，桶集合双向核覆盖 | 共享 `JOB_OVERHEAD_MARGIN_SECONDS`（≈ 5 min） |
| 直跑 emit | step 声明 `timeout-minutes`，值钉死为登记值且求和读取声明 | = 声明值（≈ 5 min） |
| 直跑 check | 同上 | ≈ 5 min |
| 直跑负控 | 读 `negative_controls` 的参考轮集合、控件登记与 `CHECK_TIMEOUT_SECONDS`；再应用 step 声明 `timeout-minutes`（登记与自检 NEGCTL 同源） | 原始 ≈ (5+1+17+0)×600 s ≈ 3.8 h；有效 = 声明限（≈ 30 min） |
| 七项自检 | EMIT + CHECK + NEGCTL + 一趟 LOCK + WRITES | ≈ 0.9 h |
| 变异自检 | (LOCK_CHECK_PASSES−1)×LOCK + CONTROL_CHECK_SUBPROCESSES×CHECK | ≈ 1.2 h |

两项自检沿用 r10 内部推导：LOCK 单趟 = 4×60 s 等锁 + 2×60 s wait + 敌意锁
3×50 s + 守卫 fd 3×50 s，共 `LOCK_CHECK_PASSES` 趟；WRITES 4×50 s；
control_check 三趟各 `CHECK_TIMEOUT_SECONDS`。公式与 `_assert_pass_counting`
的调用点互证不变。完整原始和 ≈ 6 h；直跑负控加声明上限后，有效内部和 ≈ 2.7 h，
外层登记值 `RETIREMENT_JOB_TIMEOUT_MINUTES`（现 170 min，低于托管 job 6 h
平台上限）保持严格大于有效和 + 开销。
本轮采用任务书修项 3 的 step 上限路线：仅把外层抬到 400 min 无法满足
`ubuntu-latest` 的托管 job [6 小时平台上限](https://docs.github.com/en/actions/reference/limits)；
超过平台上限的 timeout 会[先按平台上限取消](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#jobsjob_idtimeout-minutes)。
保留完整原始推导，再用实际声明的 step 上限约束有效贡献，不改变 PASS/KILLED 判分。
r9 的 85 min、r10 的 130 min 均源自不完整和；本轮补全覆盖范围，不改严格大于判据。
`retirement-job-timeout-lowered` 降到 55 仍须红，其失败锚串从外层预算常量派生。
`raw_seconds > cap_seconds` 的降级语义由 `NEGCTL_STEP_CAP_ABSORBED_CHECKER_TIMEOUTS`
机器化（#2453）：被声明限压到 cap 的必须恰是直跑负控行（emit/check 的声明值与入算值
相等），cap 必须吸收恰 3 个整 checker 超时（`cap % limit == 0`、
`cap // limit == NEGCTL_STEP_CAP_ABSORBED_CHECKER_TIMEOUTS`），
raw = passes × limit 一并断言——名义上第 4 次 checker 超时落进 step 硬杀（summary.json 只在
控件循环结束后写，硬杀下没有 summary、哪一控挂了不可见）；机器钉的是声明配置的整除
关系，真实硬杀边界受 step 时钟先启动与进程启动开销影响**略早**（评审 r1 登记）；
整除关系被破坏当场红。
外层不足会退化为平台硬杀（cancelled，summary 不发布）。

`self_check.py` 是这些守卫的单一实现，本地可整跑：

```sh
python3 -I scripts/retirement-outcome-parity/self_check.py            # 七项全跑
python3 -I scripts/retirement-outcome-parity/self_check.py --self-check-controls
```

`--self-check-controls` 给每个能造违反样本的守卫造样本，对应检查必须**变红**：体积预算用
超大夹具**两种**（单文件超 1 MiB、若干个不超单文件上限的文件合计超 5 MiB 总量）；CI 接线用
**四十九种**变异 workflow（删掉 retirement job / 整段注释掉 / 给 job 内闸门
step 加 `if: false` · 引号键 `"if": false` · `continue-on-error` / 给 **job 级**（4 空格缩进）
加 `continue-on-error` / 把闸门命令整行挪进同 step 的 `env.UNUSED: |` / 用嵌套 `env:` 下的
`run: exit 1` 冒充聚合 Require 的执行体 / 用 `if false; then … fi` 包住整条命令 / `|| true` /
`! ` 取反退出码 / `echo` 前缀 / 删掉一个闸门 step / 给 Require step 换掉 `if:` ·
加 `continue-on-error` · 末尾追加 `&& false` · 把 `run: exit 1` 改成 `exit 0` / 把聚合 `check` job 的 `if:` 改成 `false` / 追加一份 `---` 之后的 YAML 文档 / r5 标量形态四种——`run` 行内续行、
聚合 job `if` 续行 `&& false`、Require `if` 续行 `&& false`、块标量 `|`→`>`——加把两个 job
整块挪到 `on:` 下冒充（行首正则会认这种假块）与 **r6 根级** `defaults.run.shell:
/bin/true {0}`（workflow 级 shell 模板让每个 run step 无条件 rc=0，只看 `jobs:` 的守卫逐字
全等仍假 PASS）与 **r7 执行层五条**——闸门 step 删掉 `shell: bash` 行、闸门 step 把
`shell: bash` 换成 `/bin/true {0}`、聚合 Require step 换 shell 值、**非闸门 job**（rust 的
`cargo fmt`）删 `shell: bash` 行（专测全 workflow 巡走）、checkout 加 `with: {ref: …}` 让
闸门跑另一棵树的脚本——与 **r8 五条**：另外三个 job（metadata-observer /
negative-controls / gateway-schema-baseline）的 checkout 各加 `with: {ref: …}`（uses 逐键
钉死挪进全 workflow 巡走后，同一手法换 job 必须红）、pr-gate.yml 的 check-pr-gate step 删
`shell: bash` 行、pr-gate.yml 根级 `defaults.run.shell`（该文件此前没有任何守卫读）——
与 **r9 八条**：retirement job 的 `timeout-minutes` 降到 55（钉值 + 派生不等式的落点）、
pr-gate job `if: false`（被跳过的必需检查被 GitHub 报成 success）、闸门 step 加
`continue-on-error: true`（步全红而 job 仍 success）、闸门 step `run` 换壳 `exit 0`、
fetch 基线步 `continue-on-error` 键删 / 登记值 `true`→`false`（键集恰等与值钉死各有违反
路径）、闸门 step env 加未登记键、闸门 step 整步删；另修正 r8 的「pr-gate check step 删
`shell: bash`」样本——pr-gate.yml 的 shell 行带行尾注释，旧字面量匹配不到它、实际删的是
fetch 步的 shell（r9 起按「行首值 + 可选注释」锚定，step 键集恰等先行拦下）——
与 **#2451 五条**：三个直跑 gate 的 step `timeout-minutes` 降 / 升（emit 5→4 / 5→6、
负控 30→29 / 30→31——降值让挂住的检查打不满内部超时、失败退化成 step 硬杀；钉值判定
红在 `retirement_budget_rows`，预算接线块被删时这四条 SURVIVED）与负控 step 的
`timeout-minutes` 整行删（红在 step 键集判定，第一道拦截）。
每条样本的失败文案必须落在**点名判定**上（锚串全量登记在
`SAMPLE_FAILURE_ANCHORS`，`control_wiring` 另断言样本集合与登记簿逐名全等——漏一条 /
多一条 / 红错地方都算控件失败；条数 49 由 `EXPECTED_WIRING_SAMPLES` 钉死，样本与锚串
**一起**删时逐名全等照样成立、登记簿静默缩水——对齐 `EXPECTED_LOCK_HOSTILE_PLANTS`
先例，r9）；r11 预算接线的另外五个判定（step cap / overhead / budget rule /
budget coverage / budget total）只有预算面消费，workflow 样本够不到（改顺序 / 键集 /
脚本的变异先红在各自判定上）——`control_wiring` 另对**登记簿本身**做进程内变异逐条直攻
（幽灵闸门超时登记 → step cap coverage、幽灵 overhead 桶 → overhead coverage、摘掉
upload 步的桶登记 → budget rule missing、重排 step 登记顺序 → budget coverage、
`EMIT_TIMEOUT_SECONDS` 常量漂移 → budget total；登记在 `REGISTRY_MUTATION_ANCHORS`，
条数 5 由 `EXPECTED_REGISTRY_MUTATIONS` 钉死，finally 必还原）；输出目录锁用三种**预置敌意锁文件**（同名 FIFO / 目录 /
符号链接，去掉 `O_NONBLOCK` 或 `S_ISREG` 判定即挂起或假持锁，登记在 `LOCK_HOSTILE_PLANTS`，
条数 3 由 `EXPECTED_LOCK_HOSTILE_PLANTS` 断言）加**时间戳序互斥**（两个持锁子进程各打
`LOCKED ACQUIRED`/`RELEASED`（`time.monotonic()`，取锁信号与时戳合并为单次 print——分两次
flush 时父进程的块边界读会把 `ACQUIRED` 残留进 RELEASED 读取），父进程用**逐名锚正则**取
值——RELEASED 锚取 RELEASED、ACQUIRED 锚取 ACQUIRED（r8 P0：合并正则取首匹配在管道分块
交错下锚错方向、断言恒真不红），第二个的 ACQUIRED 不得早于第一个的
RELEASED——吞掉 `BlockingIOError` 的弱化照样能打出「等锁」字符串，骗不过时戳序；lock 控件
另对「阻塞 flock 换成 pass」的弱化样本跑时戳序断言、并在 ACQUIRED+RELEASED 同流的样本上
复现旧合并正则的确定性错锚）；负控的
`write_at` 用四种**预置敌意写目标**（无读者端的 FIFO / **带读者端的 FIFO** / 目录 / 符号
链接——前者需要 `O_NONBLOCK` 才不挂死，次者只有 `fstat` 后的 `S_ISREG` 能拒，去掉任一
判定都有各自的违反路径；登记在 `WRITE_HOSTILE_TARGETS`，条数 4 由
`EXPECTED_WRITE_HOSTILE_TARGETS` 断言——删一条敌意形态当场红）。`emit` / `check` / 负控没有独立违反样本：它们本身就是行为测试，
控件即同一正向检查的重跑。守卫被拆掉时它自己变红——这就是「拆掉上限 / 锁 / CI 步骤 →
相应用例变红」的机器证据。

**每条变异样本都实测过「真的造成假绿」**：从变异后的 workflow 里取出该 step 真正会执行的
脚本，用 `bash -e` + 一个必失败的 `python3` 替身跑——命令没执行 / 失败被吞的样本必须 rc=0，
未变异基线必须 rc=1。样本本身不成立就不是样本（例如把 `|| true` 加在以续行反斜杠结尾的行上
会变成 rc=127；`bash -e` 下末尾补 `exit 0` 根本走不到，同样不算假绿）。

接线守卫认的是 **PyYAML 解析后的完整标量**，不是源码行文本（#2017 r5py 重写）：守卫不再做任何
行式 / 缩进式切块，也没有结构比对的兜底正则——`yaml.compose_all` 取唯一文档（多文档 / 解析失败
即红），**根文档直属键白名单**（`name` / `on` / `jobs` / `permissions` / `concurrency` /
`run-name`；根级 `defaults` / `env` 即红——workflow 级 `defaults.run.shell` 会替换每个 run
step 的 shell，与 job 级 `defaults` 同一判法，r6 P0），job 只在 `jobs:` 映射里找（挂在
`on:` 等别处的同名块不是 job）；两个 job 的直属键白名单
（job 级 `continue-on-error` 即红，重复键也红——两个解析器可以对「哪个键活着」各执一词）；
retirement job 的 `if:` 与聚合 `check` job 的 `if:` 归一化后逐字等于登记值（GitHub 把**被跳过的
job 报成 success**，而 `check` 自己是必需检查，跳掉它就再没有东西挡合并）；retirement job 的
step 顺序与名字按登记比对，`timeout-minutes` 值钉死为登记值并断言**大于**内部最坏和
`INTERNAL_WORST_CASE_SECONDS` + job 开销余量（r9；外层预算先到时挂住的检查退化为平台硬杀）；
闸门 step 只许有 `name` + `run` + `shell` 且 `shell` 值恰为
`bash`，**加**三个直跑 gate（emit / check / 负控）各自登记的 `timeout-minutes`
（`GATE_STEP_TIMEOUT_MINUTES` 成员才许带这一键，键值钉死为登记值，#2451；
`env` / `with` / `if` / `continue-on-error`、嵌套的 `env: {run: …}` 全红）；每个闸门
step 的 `run` 必须是
**literal 块标量（`|`）**且完整标量值与 `GATE_SCRIPTS` **逐字相等**——续行拼接（plain 多行
标量，换行折叠成空格）、`>` 折叠块、引号标量**显式判红并写明是哪一种形态**；聚合 Require
只许 `name` / `if` / `run` / `shell`，`run` 恰为 `exit 1`，`if:` 归一化空白后逐字等于登记值；
**全 workflow 每个 run step 都必须显式 `shell: bash`**（r7 P0 执行层解：step 级 `shell:` 优先于
任何 workflow / job 级 `defaults.run.shell`，GitHub workflow syntax 明文「You can override
the default shell settings … using the `shell` keyword」——就算 `defaults.run.shell:
/bin/true {0}` 出现在任何层级，钉了 shell 的检测步照样执行；根键 / job 键白名单挡
`defaults` 的出现，这条挡某个 run step 裸奔继承 defaults）；uses step 的 `with` 键集合按
action 逐键钉死（给 checkout 加 `with: {ref: …}` 会让闸门跑另一棵树的脚本而登记的 `run`
标量逐字不变，r7 P1；r8 起是**全 workflow 巡走**——同一手法换任何 job 的 checkout 都必须
红，checkout 的合法键集合族「默认检出 / `fetch-depth: 0` 深克隆」登记在
`WORKFLOW_USE_WITH_KEYS`，跑未登记的 action 本身即红）；**pr-gate.yml（合并闸门第 2、3 步
——git fetch 基线步与 check-pr-gate.py 步——的宿主，r8 P1）同一巡走**：唯一文档、根键白名单、
job 直属键白名单（现只有 `if`/`runs-on`/`steps`）、每个 run step 钉 `shell: bash`、uses `with`
逐键钉死——该文件此前没有任何守卫读，对它加根级 `defaults.run.shell: /bin/true {0}` 即让
合并闸门本体无条件 rc=0 而两侧 ci.yml 守卫逐字全等；r9 P0 起 step 级三层再钉——job `if`
逐字（`if: false` 必红）、step 条数恰为 3 且按序、每个 run step 的直属键集合**恰等**（fetch 基线步
的 `continue-on-error: true` 是登记值、闸门步多出该键即红）、env 键集恰等、`run` 标量逐字
（`exit 0` 换壳必红）——三个静默废闸门形态（step `continue-on-error` / job `if: false` /
`run` 换壳）在装置侧与兄弟守卫（`test_metadata_io_driver.py` 的
`test_pr_gate_wiring_is_pinned_by_parsed_yaml` + 变异负控）双侧都必须红。逐字相等
同样意味着**等价写法会被误拒**（`run` 写成 plain / 引号标量、`if` 折叠续行、重排 `--round`
参数），这是有意的
fail-closed：本仓这段 YAML 只有一个写方，宁可误拒也不放过「文本活着但闸门失效」；改闸门脚本
必须同步改 `GATE_SCRIPTS`。PyYAML 不可用**或版本不符**时同样 fail-closed 判红，不降级回行式
解析；解析器版本钉死 **`pyyaml==6.0.3`**（#2017-A，本机实测 `yaml.__version__` 6.0.3），
CI 里由两个 job 的「安装钉死 PyYAML（#2017-A）」步骤装进 venv 并前置到 PATH
（`python3 -I` 不读 user site，但 venv 的 site-packages 属解释器自身配置、`-I` 下仍可见），
`self_check.py` 的 `PYYAML_PINNED_VERSION` 与 `src/pilotdeck-gateway/tests/
test_metadata_io_driver.py` 的 `pinned_pyyaml_version`（兄弟守卫同一登记）各有一份
fail-closed 断言；换版本必须四处同步改（两个 job 的安装步骤各一处 + 两个常量各一处）。

## 为什么不在 `scripts/gateway-parity/`

`scripts/gateway-parity/` 是 hash 冻结的装置目录：往里**新增文件**即被
`check_frozen.py` 的 `unknown_apparatus_files` 判为未登记装置，白名单登记（`A2_SCRIPTS`
/ `DATA_FILES`）与 `FROZEN.md` 哈希行重登记（`--write`）都只归协调者收货步骤——本单
明确不动冻结装置。因此本切片用独立目录 `scripts/retirement-outcome-parity/`，判据走
差异账本条目（`diffs/A8.6-RETIREMENT-OUTCOME-PARITY-001.md`）登记；若要走冻结程序接进
`session_domain.py`（Rust 轮判分自动带上 P1–P8），另立条目由协调者决定。
