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
  `lastEventType` / `finalSeen` / `error`）——去掉容差字段后 5 轮内容相同，
  `methodsSha256` 五轮一致，P7 跨轮稳定性在入库夹具上**退化成空判**而
  `p7Evaluated` 仍报 true。r7 起改回照搬：5 轮 `methodsSha256` 互不相同（5 份独立
  采集的原始哈希），P7 在入库夹具上非退化；判据消费的字段子集没变，容差字段只是
  留在文件里不被读。体积增量在预算内（见下）。

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
件逐字节等于原始件，两列相同：

| 轮 | 原始 `methods.json` sha256 | 入库 sha256（当前金样） |
|---|---|---|
| round-01 | `c46f74315569d5009592a42e05b01d523c1710b7587263d64f4c55cdb43bca0e` | 同左 |
| round-02 | `5b8398a91e3cd08c8697c81fa7f87076d0c8054be8b6f061b5a5731aca6acee0` | 同左 |
| round-03 | `da8f21606c098f931530d7bc12de54a9819753e0471a45da53902bc89eb98ced` | 同左 |
| round-04 | `820deb64fefdf44be769c5439fcc3382295b6b942a59fa61e1f1afd8849ebd03` | 同左 |
| round-05 | `b6ef2848952fdc4e736959597eb007d2c1a18275779046f101c3ddc1d2623b6d` | 同左 |
| 全部轮的 `frames.jsonl` | 见当前金样 `rounds.files[].framesSha256` | 同左（照搬未变） |

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
