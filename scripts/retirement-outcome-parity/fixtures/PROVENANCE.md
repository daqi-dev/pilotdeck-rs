# fixtures — retirement outcome-parity 参考轮的入库切片（#1809）

本目录是金样 `../ts-oracle.json` 的**可复现输入**：干净检出即可跑 `emit` / `check` /
`negative_controls.py` 的默认路径，不再依赖未跟踪的 `_tmp/`。

## 来源

| 项 | 值 |
|---|---|
| TS 基线 | `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`（Node v22.22.0，#1355 `--suite retirement`） |
| 原始记录（仓外，未跟踪） | `_tmp/20261106-issue-1477/r1/data/round-01..05/driver/{methods.json,frames.jsonl}` |
| 采集单 | #1477（判据登记件 `specs/gateway-parity/diffs/A8.6-RETIREMENT-OUTCOME-PARITY-001.md`） |
| k | 5 轮 |

## 切片规则（#1809，r7 修订）

- `driver/frames.jsonl`：**逐字节照搬**，未做任何重编码——线缆帧是判据的原始证据，
  `raw` 与 `frame` 保持自洽。因此 `rounds.files[].framesSha256` 与原始件**完全相同**。
- `driver/methods.json`：**逐字节照搬**原始件（r7，评审 P1）。r1–r6 曾只保留判据消费的
  5 个字段、去掉 `check_outcome_parity.py` 不读的四个容差字段（`elapsedMs` /
  `lastEventType` / `finalSeen` / `error`）——去掉容差字段后 5 轮内容相同、
  `methodsSha256` 五轮一致。**判据本身没有变松**：P7 仍逐轮比对解析后的
  `(outcome, errorCode)`、`p7Evaluated` 仍为 true（评估只要求轮数 >1，容差字段与文件
  哈希都不进判据），但 5 轮已不是 5 份独立采集、而是同一份数据的五个副本，跨轮稳定性变成
  「数据与自身比对」，`p7Evaluated=true` 所传递的「跨轮稳定已核」失去证据力（评审 r8 勘误
  措辞：此处曾写作「退化成空判」，不准确——P7 被评估了，只是评估对象塌缩）。r7 起改回
  照搬：5 轮 `methodsSha256` 互不相同（5 份独立采集的原始哈希），非退化由
  `../self_check.py` 的 `check` 用例**机检**（对入库夹具复算 5 轮 `methods.json` /
  `frames.jsonl` 的 sha256 并断言互异，r8——切回去掉容差字段的切片当场红）；判据消费的
  字段子集没变，容差字段只是留在文件里不被读。体积增量在预算内（见下）。

## 体积（`du -sk` 原始值，2026-11-07 r7 实测）

```
1568    scripts/retirement-outcome-parity/fixtures
 296    .../round-01/driver/frames.jsonl     (300624 B)
  15    .../round-01/driver/methods.json     (14905 B)
（round-02/03 同；round-04 14913 B、round-05 14904 B）
```

单文件最大 300624 B（约 294 KiB，上限 1 MiB），总量 1568 KiB（上限 5 MiB）。
预算由 `../self_check.py`（`--only budget`，不存在 `--budget` 参数）机检，超限即红。

## 原始 ↔ 入库 sha256（审计链）

`ts-oracle.json` 的 `rounds.files[]` 登记的始终是**当前入库文件**的 sha256。r7 起入库
件逐字节等于原始件，因此下表两列即入库值。r10（评审 P1）起两个文件都逐轮登记
**原始采集**哈希：`../self_check.py` 的 `check` 用例对入库件复算并与本表交叉核对
（methods 与 frames 各自同口径；表解析不出 / 行数对不上 / 任一字节改动都当场红），
`frames.jsonl` 不再依赖金样 `rounds.files[].framesSha256` 的自指锚（金样本身是从
入库夹具 emit 出来的，挡不住入库件被改）：

| 轮 | 原始 `methods.json` sha256 | 原始 `frames.jsonl` sha256 |
|---|---|---|
| round-01 | `c46f74315569d5009592a42e05b01d523c1710b7587263d64f4c55cdb43bca0e` | `0945e78eca71dce65135d0eb8a4bbcb4a828fc4f2f35ae16f3a566fa5dded034` |
| round-02 | `5b8398a91e3cd08c8697c81fa7f87076d0c8054be8b6f061b5a5731aca6acee0` | `902366ef5363009121c6e72633c35aace41282b2533b7638c46bc6c93f83b2a4` |
| round-03 | `da8f21606c098f931530d7bc12de54a9819753e0471a45da53902bc89eb98ced` | `0afa3a14ab2f552e9df0a3760f913755f68085db7f2f6acad7171af4392a7888` |
| round-04 | `820deb64fefdf44be769c5439fcc3382295b6b942a59fa61e1f1afd8849ebd03` | `311657e03529f970b84ad133d576860cef368df434f97e8c66860761b59c9446` |
| round-05 | `b6ef2848952fdc4e736959597eb007d2c1a18275779046f101c3ddc1d2623b6d` | `641d95cb2712bce5f68640a828049ca9f0550fb5afaa9c9fbba68c4cd814397b` |

历史对照（r1–r6 的切片期值，去掉容差字段后五轮同为
`d7e5e3e897cf3aab9849aa7c29a5a198028b20ac4d52aeb4457fe3aefcbed49d`）留档于 r6 及之前的
提交历史，不再在此维护。

**金样的判据部分（`rounds` 之外的 101 行 `methods`）切片前后逐字节相同**——切片没有
改动任何一条判据，只改了输入指纹（r7 把指纹改回原始件）。

## 备注：绝对路径残留

原始件与入库件的 `frames`/`methods` 数据里保留**采集宿主的绝对路径**（错误文本
`error.message` 等）：判据不比较这些文本（只要求线缆 `final: true` 与 message 非空），
重编码抹掉反而破坏「逐字节照搬」。判据（`specs/gateway-parity/diffs/
A8.6-RETIREMENT-OUTCOME-PARITY-001.md`）不消费这些路径。

## 重新生成（必须与已提交金样逐字节相同）

```sh
F=scripts/retirement-outcome-parity/fixtures
python3 -I scripts/retirement-outcome-parity/check_outcome_parity.py emit \
  --oracle /tmp/oracle.json \
  --round $F/round-01 --round $F/round-02 --round $F/round-03 \
  --round $F/round-04 --round $F/round-05
cmp /tmp/oracle.json scripts/retirement-outcome-parity/ts-oracle.json
```

CI 的 `retirement-outcome-parity` job 逐步执行这条命令；不一致即红。
