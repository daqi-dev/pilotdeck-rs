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
前缀设置（这三者都可能让进程起不来），且只依赖 `os` / `sys`；它只在 `__main__` 下执行——被
import 时不得拿宿主的 `sys.argv` 去删任何东西。

输出目录只从**显式** `--output` 取：解析器 `allow_abbrev=False`，选项名精确匹配，解析前即可
无歧义地取出。不按「哪些 token 可能是位置参数」猜候选——未知选项带不带值在解析前不可判定，
猜错的代价是删掉别人的证据（误删同样算丢数据）。**没有 `--output` 就一个都不删**：`--` 之后的
`--output` 按 argparse 语义只是位置参数，而本执行器没有位置参数（命令直接 rc=2），此时不做任何
猜测。**帮助路径同样失效**：命令里显式点了输出目录，之后无论是跑、是 `-h` / `-hh`、还是
argparse 报错，该目录的旧 PASS 都已报废——「跳过帮助」等于把「哪些形状算帮助」再猜一遍，猜错的
方向永远是留旧 PASS 冒充本轮。重复写多次时每个非空值都失效（argparse 只有最后一个生效，但这条
命令终究没跑完）。值接受规则照 argparse：非选项样式的下一个 token 即值，`-1` / `-1.5` 与单字符
`-` 也算值。空值（`--output=`）不失效且判参数错误 rc=2——`Path('')` 就是当前工作目录，否则会删掉
调用者 CWD 里的 `summary.json`。

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

入口级失败一律 **rc=2**（输入不可用，不是判据 FAIL）：缺 `-I`、checker 缺失 / 不可读、pycache
前缀候选耗尽。`raise SystemExit('消息')` 返回的是 **1**，会被下游当成「判据 FAIL」。

## 为什么不在 `scripts/gateway-parity/`

`scripts/gateway-parity/` 是 hash 冻结的装置目录：往里**新增文件**即被
`check_frozen.py` 的 `unknown_apparatus_files` 判为未登记装置，白名单登记（`A2_SCRIPTS`
/ `DATA_FILES`）与 `FROZEN.md` 哈希行重登记（`--write`）都只归协调者收货步骤——本单
明确不动冻结装置。因此本切片用独立目录 `scripts/retirement-outcome-parity/`，判据走
差异账本条目（`diffs/A8.6-RETIREMENT-OUTCOME-PARITY-001.md`）登记；若要走冻结程序接进
`session_domain.py`（Rust 轮判分自动带上 P1–P8），另立条目由协调者决定。
