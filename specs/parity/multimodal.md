# 原版多态场景登记（Issue #35 · 待批）

方案 B 已由人批准：仅对本表登记的场景，每一遍 Rust 必须与 **TS 参考池中至少一遍**的冻结 G03 总判决为 PASS。普通场景仍按第 i 遍 TS 对第 i 遍 Rust，五对全 PASS。类别、参考池和准入条件先由 TS 数据确定，再读取 Rust 结果；Rust 不能提供类别、扩大池或改变阈值。

## 登记表

| 场景 | TS 参考池路径与遍数 | 观察到的类别数与各类代表遍 | 依据 | 状态 |
|---|---|---|---|---|
| `cancel_during_tool` | `/Users/da/ws/pilotdeck-rs/_tmp/20261014-flake35/ts-r{1..20}.d`，20 遍；只用 TS | 2 类：类 0 共 17 遍，代表 `ts-r1.d`；类 1 共 3 遍，代表 `ts-r2.d`（成员 ts2、ts7、ts11） | TS 两两冻结 G03 共 190 对：139 对 PASS、51 对 FAIL；类内全部 PASS、类间全部 FAIL。E5 acknowledged 的 34 / 36 为观察标签，不能替代完整 G03 判决。20 遍均通过单遍 G03 自比、身份与健康检查。复算与边界见 [exp009](../../docs/experiments/exp009-multimodal-ruler/readme.md) | **待批**（方案 B 人已批，具体登记待协调者评审冻结） |

## 准入条件

1. TS 参考池至少 **20 遍独立捕获**。目录解析后不得重复；同一账本与同一 trace 的复制品不得重复计数。池可只捕获登记场景，无须补跑其他场景。
2. 每份参考必须是 TS 身份，账本有效、身份 proof 与 sha256 匹配、无 BLOCKED，并通过冻结 G03 的自比、场景 oracle、生产证据、适用 E 规则与 trace 健康检查。
3. 类别由 TS **全部两两 G03 总判决**聚类得出。类内所有对 PASS，类间所有对 FAIL；若 PASS 关系不具传递性，拒绝聚类，不用连通分量把桥接结果合并。
4. 至少观察到两类，**每类至少 2 遍**。某类只有一遍时记录成员与依据，保守视为未经确认的噪声，**拒绝结果集合计分**，不把它并入其他类或用 Rust 补足；补充 TS 证据后重新评审登记。
5. 仍遵守 [normalization.md](normalization.md) 的同输出路径硬前提。参考池与被计分的 TS/Rust 捕获时输出目录须一致，不能传路径替换参数。请求体、prompt 与预算字段仍完整比较；不同日期或输入配置可能使旧 trace 无法匹配当前池。

## 离线入口与判决

[score.py](../../scripts/parity/score.py) 接收 `--ts DIR...`、`--rust DIR...`，两侧各五遍，顺序由调用者给定；`--reference-pool TS_DIR...` 可显式指定 TS 池，省略时使用本表的 20 遍。显式池仍须通过以上 TS 准入，类别只从该池重算。登记名单只包含 `cancel_during_tool`，不能把其他场景隐式改成结果集合判法。

输出 JSON 包括逐场景判决与原因、逐遍 G03 规则与差异路径、Rust 匹配到的 TS 遍号、TS 全部两两判决与类成员、输入和代码 sha256、总分。`--json FILE` 保存完整报告。退出码：全场景 PASS 为 0，有场景 FAIL 为 1；输入、身份、BLOCKED、池规模或单例类导致 **REFUSED 为 2**，此时总分为 null。`--paired-only` 只供复算原判法，不使用多态池，不能与 `--reference-pool` 同传。

每遍 Rust 有匹配即 PASS，五遍全部匹配才是场景 PASS；不要求 Rust 覆盖每个 TS 类，也不比较两类频率。任一 Rust 遍与所有 TS 遍均不匹配，场景 FAIL，即使该形状在 Rust 重复出现。多态仍计入分子和分母，与「原版不稳定」排除不同。目前没有批准的原版不稳定排除，分母 **53 − 0 = 53**，不根据 TS/Rust 本轮失败动态缩小。

负例见 [controls/multimodal.py](../../scripts/parity/controls/multimodal.py)，已接入 `controls/run_all.py`。冻结 G03、原归一化和原冻结检查未改；本登记与新增计分器须由协调者评审后登记 sha256。

## 日期上下文一致前提（Issue #35 第 2 步）

TS 参考池必须与被测 Rust 轮次在**同一 UTC 日采集**，并继续使用完全相同的捕获时输出路径。一轮回归（两侧各五遍）不得跨 UTC 午夜；UTC+8 的本地分界为 **08:00**。跨日的轮次**整轮作废重跑**，不得选择部分遍数、删掉日期或归一化 prompt 后继续计分。参考池自身也必须在同一 UTC 日采集，不能把不同日期聚成合法结果类。

计分器在比较前从每份 trace 的所有 `model.request.modelView.systemPrompt` 中读取 `now: YYYY-MM-DD`，记录到 JSON 的 `date_context` 和逐遍证据中。找不到时记 `date: null` 并说明是没有模型请求，还是 systemPrompt 没有日期；如仅部分请求缺日期，也记录对应 sequence。双方均为 null 的无日期场景保留原 G03 判法，不能由 null 推断采集日；一侧有日期、另一侧为 null 时拒绝比较。采集者仍须保证整轮采集日期符合上述规则。

普通场景的同序号 TS/Rust 日期不同，该遍与该场景为 **REFUSED**；登记多态场景的 Rust 遍与参考池日期不同，该遍与该场景为 **REFUSED**，不尝试寻找 G03 匹配，也不能计为 PASS 或 FAIL。一个 trace 或同一侧的五遍回归中出现多个 UTC 日期，整轮为 REFUSED；参考池出现多个日期则拒绝定标。任何场景 REFUSED（包括获批排除的场景）都使整轮 `valid_score=false`、`verdict=REFUSED`、`score=null`，退出码为 2，同时保留逐场景理由、日期证据与输入哈希。日期符合前提后，G03 仍完整比较原 prompt，不改变冻结归一化。

本节补充并覆盖前文把日期差异当作普通请求体不匹配的计分表述：日期差异属于前提不满足，历史轮次须同日重采，不能据此报告产品回归 FAIL。
