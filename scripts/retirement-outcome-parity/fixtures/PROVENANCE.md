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

## 切片规则（#1809）

- `driver/frames.jsonl`：**逐字节照搬**，未做任何重编码——线缆帧是判据的原始证据，
  `raw` 与 `frame` 保持自洽。因此 `rounds.files[].framesSha256` 与切片前**完全相同**。
- `driver/methods.json`：只保留判据实际消费的 5 个字段
  `method` / `shape` / `outcome` / `events` / `errorCode`；
  去掉四个**容差字段**（`check_outcome_parity.py` 一处都不读，grep 计数为 0）：

  | 去掉的字段 | 理由 |
  |---|---|
  | `elapsedMs` | 纯计时，95/101 跨轮变动，判据不比较（差异条目「容差与残余」） |
  | `lastEventType` | 判据只看 `events >= 1` 与线缆 `final: true` |
  | `finalSeen` | 同上，事件侧收尾由线缆 `final: true` 核 |
  | `error` | `error.message` 文本含轮次绝对路径与 provider 措辞，判据只要求线缆 message 非空 |

  去掉容差字段后 5 轮的 `methods.json` **内容相同**（这正是「5 轮 `(outcome, errorCode)`
  零漂移」的直接体现），因此 `rounds.files[].methodsSha256` 五轮一致；**逐轮可区分性由
  `framesSha256` 保留**（5 个互不相同）。审计时不要用 `methodsSha256` 判「是不是同一轮」。

## 体积（`du -sk` 原始值，2026-11-07 实测）

```
1540    scripts/retirement-outcome-parity/fixtures
 296    .../round-01/driver/frames.jsonl     (300624 B)
  12    .../round-01/driver/methods.json     (10914 B)
（round-02..05 同）
```

单文件最大 300624 B（约 294 KiB，上限 1 MiB），总量 1540 KiB（上限 5 MiB）。
预算由 `../self_check.py --budget` 机检，超限即红。

## 原始 ↔ 切片 sha256（审计链）

切片前 `ts-oracle.json` 登记的是**原始** `methods.json` 的 sha256；切片后登记的是本目录
文件。两者对照如下，可据此确认「切片没换数据，只掉了容差字段」：

| 轮 | 原始 `methods.json` sha256（切片前金样） | 切片后 sha256（当前金样） |
|---|---|---|
| round-01 | `c46f74315569d5009592a42e05b01d523c1710b7587263d64f4c55cdb43bca0e` | `d7e5e3e897cf3aab9849aa7c29a5a198028b20ac4d52aeb4457fe3aefcbed49d` |
| round-02 | `5b8398a91e3cd08c8697c81fa7f87076d0c8054be8b6f061b5a5731aca6acee0` | `d7e5e3e897cf3aab9849aa7c29a5a198028b20ac4d52aeb4457fe3aefcbed49d` |
| round-03 | `da8f21606c098f931530d7bc12de54a9819753e0471a45da53902bc89eb98ced` | `d7e5e3e897cf3aab9849aa7c29a5a198028b20ac4d52aeb4457fe3aefcbed49d` |
| round-04 | `820deb64fefdf44be769c5439fcc3382295b6b942a59fa61e1f1afd8849ebd03` | `d7e5e3e897cf3aab9849aa7c29a5a198028b20ac4d52aeb4457fe3aefcbed49d` |
| round-05 | `b6ef2848952fdc4e736959597eb007d2c1a18275779046f101c3ddc1d2623b6d` | `d7e5e3e897cf3aab9849aa7c29a5a198028b20ac4d52aeb4457fe3aefcbed49d` |
| 全部轮的 `frames.jsonl` | 见当前金样 `rounds.files[].framesSha256`（切片前后**未变**） | 同左 |

**金样的判据部分（`rounds` 之外的 101 行 `methods`）切片前后逐字节相同**——切片没有
改动任何一条判据，只改了输入指纹。

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
