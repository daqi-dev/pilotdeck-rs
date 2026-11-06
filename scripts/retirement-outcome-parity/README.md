# retirement-outcome-parity — Rust retirement 轮逐方法 outcome / 错误码对照装置（#1477）

判据（P1–P8、阈值 0 处不一致、容差）登记在
`specs/gateway-parity/diffs/A8.6-RETIREMENT-OUTCOME-PARITY-001.md`，**先于任何 Rust
retirement 轮登记**（S1 判据提交，body 含 `criteria-commit: yes`；登记时未打开任何
Rust 侧 retirement 产物，Rust 列一律 `NOT_RUN`）。本目录是这些判据的独立装置：

| 文件 | 作用 |
|---|---|
| `check_outcome_parity.py` | 唯一实现。子命令 `emit`（从 TS 参考轮重生成 / 复核金样）与 `check`（一轮或多轮对照金样）。退出码 0 PASS / 1 FAIL / 2 输入不可用（fail-closed）。 |
| `ts-oracle.json` | 101 行金样：`method` / `shape` / `outcome` / `errorCode` / `wireCode` + `baseline`（revision、Node 版本）+ `rounds`（k、目录名、逐文件 sha256）。 |
| `negative_controls.py` | N1–N11 汇总式控件执行器（见下）。 |

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
- rc=2：输入不可用（缺文件、解析失败、缺字段、金样形状不对）——fail-closed，绝不当 PASS。
- **Rust 轮必须显式跑本装置**：判据尚未接进冻结装置 `session_domain.py`（见「容差与残余」③）。

## 容差与残余（登记在差异条目里，此处只摘要）

- 阈值 = **0 处不一致**；无百分比、无退化名额、无 per-method 白名单。
- 不比较（理由在条目「范围」）：`elapsedMs`（纯计时，95/101 跨轮变动）、
  `error.message` 文本（含轮次绝对路径与 provider 措辞，只要求非空）、
  `events` 计数 / `lastEventType`（只要求 stream ok 有 `final: true` 帧且 `events >= 1`）。
- 残余：① 文案与帧数不比较；② 只覆盖单轮内方法级行为，hostAudit / entryInventory /
  网络面归 #1478；③ 未接进冻结装置，Rust 轮必须显式跑本装置，否则不得宣称覆盖。

## 负控（N1–N11）

```sh
python3 -I scripts/retirement-outcome-parity/negative_controls.py <输出目录> \
  [--data _tmp/20261106-issue-1477/r1/data] \
  [--oracle scripts/retirement-outcome-parity/ts-oracle.json]
```

只做数据变异（内存改 + 写临时目录，**不改任何源码文本**）；逐条记
`KILLED` / `SURVIVED` / `INVALID` + 原因，全部跑完再汇总，任一非 KILLED 即非零退出；
`summary.json` 原子发布（先写 `*.tmp` 再 `rename`）。判据分两步：**注入是否落地**（重读
落盘文件核对变异事实）+ **归因是否落到被注入对象**（期望规则 fire 且点名被注入方法）。
N10（99/101 unavailable，线缆同步）是 F5 / DS-P0-1 的闭环证据：旧 `UNIFORM_UNAVAILABLE`
语义（全 101 才 FAIL）在该形状下一条规则都不触发。

## 为什么不在 `scripts/gateway-parity/`

`scripts/gateway-parity/` 是 hash 冻结的装置目录：往里**新增文件**即被
`check_frozen.py` 的 `unknown_apparatus_files` 判为未登记装置，白名单登记（`A2_SCRIPTS`
/ `DATA_FILES`）与 `FROZEN.md` 哈希行重登记（`--write`）都只归协调者收货步骤——本单
明确不动冻结装置。因此本切片用独立目录 `scripts/retirement-outcome-parity/`，判据走
差异账本条目（`diffs/A8.6-RETIREMENT-OUTCOME-PARITY-001.md`）登记；若要走冻结程序接进
`session_domain.py`（Rust 轮判分自动带上 P1–P8），另立条目由协调者决定。
