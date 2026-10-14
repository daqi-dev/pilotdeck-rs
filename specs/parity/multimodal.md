# 原版多态场景登记（Issue #35 · 第 2 轮 · 待冻结）

方案 B 已由人批准；本轮按两家评审收紧：仅对登记场景，每一遍 Rust 必须与 **TS 某一确认类的全体成员**逐一得到冻结 G03 总判决 PASS。只命中类中部分成员为 FAIL。普通场景仍按第 i 遍 TS 对第 i 遍 Rust，五对全 PASS。类别与准入先由 TS 确定，再读取 Rust；Rust 不能提供类别、扩大池或改变阈值。

## 登记与每日重采池

登记场景仅为 `cancel_during_tool`。登记的是场景和准入流程；每日实际类数、成员和代表由当日 TS 全部两两 G03 重算，不沿用历史的 17/3 分组或默认日期目录。

1. 在同一 UTC 日先独立采集至少 20 遍 TS 参考，再另采 TS 五遍与 Rust 五遍。UTC+8 的 UTC 日分界为本地 08:00；跨日整轮作废重跑，池跨日拒绝定标。池每天重新采集，本离线工具不负责采集。
2. 归档每遍 `run.json`、summary 和原始 trace。钉住每份 trace 的 sha256、账本 `started_wall` 的 UTC 日、TS `expected_sha256`、`inputs_sha256`、`driver_sha256`、`node_sha256`，保存 TS-only 定标 JSON 的 `TS_calibration`（包含全部类成员、成员遍号、190 对矩阵和 trace 哈希）。参考池定标不读取 Rust。
3. 对照并评审当日登记，再计分。`--reference-registration FILE` 可将这份 TS-only JSON 作为登记锁：计分器在读取 Rust 前逐项核对重算的类、矩阵、成员、日期、同源字段和 trace 哈希；不同即 REFUSED。不能以旧分组替代重算。未提供登记锁的显式池仍只生成待评审证据，不代表已冻结。
4. 每次显式传 `--reference-pool TS_DIR...`；省略会 REFUSED，防止默用某天的旧池。归档定标 JSON、计分 JSON 与登记锁自身 sha256 后交协调者评审冻结。

## 准入条件

1. 至少 **20 遍独立 TS 捕获**。解析后的目录不得重复；去重指纹只由原始 trace sha256 构成，不包含账本哈希。异目录复制 trace 并修改账本仍算同一捕获，拒绝重复计数。
2. 每份参考必须为有效 TS 身份，账本和 proof 的二进制 sha256 一致、无 BLOCKED，并通过冻结 G03 自比、oracle、生产证据、适用 E 规则和健康检查。
3. 池内全部 TS，以及被计分的 TS 五遍，`expected_sha256`、完整 `inputs_sha256`、完整 `driver_sha256`、`node_sha256` 必须完全相同；缺失或不同即 REFUSED。二进制同源由账本 expected 与实际捕获 proof 核对，不用今天磁盘上的二进制替换归档证据。
4. 池目录与计分 TS 五遍不得重叠；即使目录不同，登记场景原始 trace sha256 重叠也 REFUSED。
5. 类别由 TS **全部两两 G03 总判决**聚类。类内全部 PASS、类间全部 FAIL；PASS 不具传递性时拒绝，不用连通分量合并桥接结果。至少两类，**每类至少 2 遍**；单例是未确认噪声，拒绝定标，不用 Rust 补足。
6. 池与被计分 TS/Rust 的捕获时 output 路径必须相同；不允许离线路径替换。仍遵守 [normalization.md](normalization.md)；请求体、prompt 和预算字段完整比较。

## 日期与证据

同日判定使用有效账本的数值型 `started_wall` 转为 UTC 日，记录 `capture_utc_day`。两侧各五遍的十份账本须同日，否则整轮 53 场景 REFUSED；池自身须同日，池日与计分日不同则登记多态场景 REFUSED。无效或缺失 started_wall 拒绝输入。prompt `now` 不作为采集日依据；双方 trace 的非空日期值集合不等时，按上下文不一致 REFUSED（第 3 轮修正）。

仍读取每份 trace 全部 `model.request.modelView.systemPrompt` 中的 `now: YYYY-MM-DD`，保存日期集合、缺日期 sequence 与说明。缺 now 的请求不导致拒绝，完整 prompt 继续交给 G03：双方同样部分缺日期可正常计分；只有一侧缺日期会因 systemPrompt 不同判 FAIL。一侧整份 trace 无日期而另一侧有日期，也只按账本日判断前提，账本同日则交 G03。只有双方 observed_dates 都非空且集合不等才因 prompt 日期上下文 REFUSED；集合相同（包括相同的多日期集合）仍交 G03。没有 model.request 也记 null；null 不能豁免账本同日核对。多个 prompt 日期不用于推断跨午夜采集。

[score.py](../../scripts/parity/score.py) 接收两侧各五遍 `--ts DIR...` / `--rust DIR...`，以及显式池与可选登记锁。`--paired-only` 仅复算旧配对判法，不能与池或登记锁同传。JSON 保留逐遍真实 G03 `paired` 结果，即使外层因日期 REFUSED，paired 也不会改写为拒绝摘要；拒绝的多态遍 `comparisons=[]`。有效多态遍保存全部池比较、`matched_classes` 与每类的 `matched_members` / `unmatched_members`，遍号对应 `member_runs` 和钉住的 trace 哈希。

每遍 Rust 完整命中某一类才 PASS，五遍全 PASS 才为场景 PASS；不要求覆盖所有类，也不比较频率。部分命中或完全未命中为 FAIL。多态仍计入分子和分母；当前没有批准的原版不稳定排除，分母为 **53 − 0 = 53**，不得按失败或拒绝动态缩小。一旦任何场景 REFUSED（含批准排除的场景），整轮 `valid_score=false`、`score=null`，退出码 2；有效全 PASS 为 0、有效 FAIL 为 1。

## 历史池钉住的 trace（只读复算证据）

2026-10-14 UTC 的归档池：`/Users/da/ws/pilotdeck-rs/_tmp/20261014-flake35/ts-r{1..20}.d`，20 遍。本轮重算仍为类 0 共 17 遍（代表 ts1），类 1 共 3 遍（代表 ts2；ts2、ts7、ts11）；190 对中 139 PASS / 51 FAIL。它仅是这一天的待批证据，不能充当后续日期的默认池。TS-only 完整登记锁见 worktree `_tmp/round2-ts-pool.json`；历史计分日为 2026-10-13，不能与本池混用。各遍 `artifacts/cancel_during_tool.pilotdeck-current-sidecar.jsonl` 原始哈希如下。

| 捕获目录 | 重算类 | trace sha256 |
|---|---:|---|
| `ts-r1.d` | 0 | `f1ce9a45a97d7b034b7365704334cacb532077c412ed0f5d91ef741a889b79b8` |
| `ts-r2.d` | 1 | `6b0bf1398a2c27bf5f03caa4e76d8ad4f552a31d3b3f544f09fe0c416077d148` |
| `ts-r3.d` | 0 | `c4ee7230d2c829a637bc9acfd718102013b275df8f933d4099707e9874cfa7ae` |
| `ts-r4.d` | 0 | `390fb15c8974caff01fe37dbdba0b848193b3a79da098852aa5969e935238057` |
| `ts-r5.d` | 0 | `a196eb09e6e14295aece2bdedfc1dacc5ebbadce174f6a2fff95bdb597553c5a` |
| `ts-r6.d` | 0 | `e8abea093af05cc5342e7d87a474dd026f773f422893e38ca942e332419a7543` |
| `ts-r7.d` | 1 | `ec08c98acbf53db7a3f1bd276ef95cdb70533866d6abee1f43ab2f8ba2f1c00d` |
| `ts-r8.d` | 0 | `069836b4eb77f5ddb160bb9a4194924608f791780cef54b049e05d72ad28caf2` |
| `ts-r9.d` | 0 | `709744d14429a9b0f0cfcf7a67b7c5923f338a98967677458228597e14b7c154` |
| `ts-r10.d` | 0 | `15a2a13d3a9b5d01ad92361a882d4dc1b95b11368cc66239bb0f6cffc5de4db0` |
| `ts-r11.d` | 1 | `b069e6e3c2b685b56ebbf7fde7e5e13f32161d0126cf2cf3ead35ec15731b519` |
| `ts-r12.d` | 0 | `94280cad8b16e23aec7a064b8e5c651d833668ebe9bf8b8f3e066ac29e5f517b` |
| `ts-r13.d` | 0 | `c3cffe1bebd75650a2c22b5b7d7a17f497da25685682f7260975d530d4ce6fec` |
| `ts-r14.d` | 0 | `fada488bab02b17ed45d558b1bab665aace5ec3fb3b9d61459e7ff72be10c0cd` |
| `ts-r15.d` | 0 | `716fe7055724cd1dd28696ecc2e7adc5d2d9608546cefedaa3ebba716af19bce` |
| `ts-r16.d` | 0 | `4f9ebaff218709e5d3782ea3a7ac5b97c82b00e36e2b9ec5f348da3fa27dbe4f` |
| `ts-r17.d` | 0 | `c5d953f99a8905f1aa9c60a42ad0d48018cde1fcc099ea43d021ffac8150b31d` |
| `ts-r18.d` | 0 | `d9e34167b0eda49422dc79f574e1e2abc15b127537bc5927a75ea5e3806074e0` |
| `ts-r19.d` | 0 | `fbda9d70c6f766b7485f859cecb4bf4b7678f0ac16063168f0f6522c37d90565` |
| `ts-r20.d` | 0 | `e91949a8fae19d83abd4d12a34b7b85e76d7491057dffc461896e0d967a4af24` |

复算与限制见 [exp009](../../docs/experiments/exp009-multimodal-ruler/readme.md)。负例全部接入 [controls/run_all.py](../../scripts/parity/controls/run_all.py)，具体见 [controls/multimodal.py](../../scripts/parity/controls/multimodal.py)。G03、原归一化与冻结检查不改；本轮新增/修改的尺子哈希由协调者评审后登记。
