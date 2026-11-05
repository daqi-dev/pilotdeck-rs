# gateway-parity 冻结登记「发车制」（车次批处理）

人批 2026-11-05（本条即授权）；决策卡 `state/decisions-20261105-frozen-batch.md`。
本文件规定「一次冻结登记周期承载 N 张单」的编排。它是**新增的可改载体**，不改任何冻结字节：

| 文件 | 状态 | 本文件能否改它 |
|---|---|---|
| `FROZEN.md` 哈希列表 | 由 `check_frozen.py --write` 生成，只有车长执行 | 否，只规定「谁在什么时候跑一次」 |
| `FROZEN.md` 哈希列表以外的叙述 | 相对 merge-base 原字节冻结（`check_registration`） | 否 |
| `accepted-diffs.md` 与其哈希行 | 原字节冻结（`check_append_only`） | 否 |
| `diffs/README.md`、`diffs/` 既有条目、`frozen/` 既有事件 | 相对 merge-base 原字节冻结 | 否 |
| `specs/gateway-parity/BATCH-RELEASE.md`（本文件） | 新增文件，不在任何哈希清单内 | 是 |
| `scripts/gateway-parity/README.md` | 装置的 `DATA_FILES` 白名单成员，无哈希行 | 是（只放指针） |

机器规则以 `check_frozen.py` 为准；本文件与机器规则冲突时以机器为准并另开单修本文件。

## 1. 不变量（批处理不削减任何一条）

- **I1 一单一初始事件**：每张单在 `specs/gateway-parity/frozen/` 有一个本次新增的**初始**事件文件 `YYYYMMDD-<ID>.md`，四个字段（`Calibration-Evidence` / `Independent-Reviews` / `Approved-By` / `Approval-Basis`）各自非空。修复 / 再登记按 §5 追加 `-r<n>` 修订事件（同单续号，编号跨日续算），已合入的事件不可改。
- **I2 一单两评审**：`Independent-Reviews` 必须是两条**不同谱系、不同引用**的真实非作者评审；机器只查格式与去重，不把格式通过当审批通过。
- **I3 一单真实复算**：单的「机器判据」必须能在车次复算产物里逐条指到具体测试名 / 报告字段 / 负控日志。**不接受**用「整车复算 PASS」代替单的证据。
- **I4 判分语义零漂移**：车次只合并「登记与复算的次数」，不改阈值、分母、映射、归一化、差异账本。判分语义变更须另有明确批准，且按 §3 A4 限制上车。
- **I5 `--write` 唯一执行人**：车长（协调者），且在车次 HEAD 固定后只执行一次；§5 失败处置里追加事件后的重跑仍由车长执行，不计入「一次」。
- **I6 原字节冻结**：不得为了「一次搞定」而改 `FROZEN.md` 叙述、`accepted-diffs.md`、`diffs/` 既有文件或 `frozen/` 既有事件。

## 2. 名词与产物

- **车次（train）**：一次冻结登记周期。产物 = 一条集成分支 `frozen/train-<YYYYMMDD>-<n>`（落为一个到 main 的 PR），含 N 个已收敛单的装置改动 + N 个初始事件文件（修复时另有 `-r<n>` 修订事件）+ 一次 `--write` 后的 `FROZEN.md` + 一次车次复算（失败处置会追加事件并重跑 `--write` / 复算，见 §5）。
- **单元（unit）**：一张单在车次内的增量（代码提交 + 事件文件 + 复算切片）。
- **车长**：协调者。负责组车、`--write`、复算、出车与失败处置，不承担单的实现。
- **冻结窗口**：从步骤 8 的登记提交（事件 + `FROZEN.md`）落地起，到合并完成、车次分支 HEAD 不再变化的时间段。步骤 3 固定的 HEAD 指**装置输入字节**（各单元代码提交），不是最终 PR HEAD；登记提交落地后不得再改装置输入字节或已写入的事件，任何变化按 §5 追加事件并重跑。
- **车次证据目录**：`_tmp/<YYYYMMDD>-frozen-train-<n>/`（不覆盖：撞名或复用既有归档会响亮失败，目录唯一性由车次编号分配保证）。

## 3. 上车准入

必须上车：改动落在 `check_frozen.py:manifest_inventory()` 任一哈希行的文件（当前 28 行：`acceptance.md`、`normalization.md`、`accepted-diffs.md`；`common.A2_SCRIPTS` 18 件；`fixtures/session-calibration.json`、`fixtures/session-expectations.json`；`scripts/a5-19/` 5 件）。

**不用上车（登记成本为零）**：只新增 `specs/gateway-parity/diffs/<ID>.md`；只改 `scripts/gateway-parity/README.md` 等未登记文件；只改产品代码 / Rust 测试 / 账本措辞。

一个单元可同车，当且仅当全部满足：

- **A1 已收敛**：两家非作者评审到无 P0/P1，评审引用可追（写进事件）。
- **A2 不打架且链可被机器接受**：单元之间文件面不冲突；同文件面的单元按顺序叠加成链，每个单元的增量 diff 可独立复核。**上车前须核该文件的整条链机器可接受**：不得回环、不得净零变化（同文件 `a→b→a` 会让新增事件变成陈旧边而被 `check_frozen.py --base` 判 FAIL）；不满足就改单或拆车，不能靠「顺序叠加」的措辞兜底。
- **A3 同语义域**：都在 `gateway-parity` 装置内。跨装置（`scripts/parity`、`scripts/ui-server-parity`）不得同车——三套登记按 #1089 各自独立。
- **A4 判分语义**：改变判分语义的单元必须已有明确批准记录并写进事件的 `Approval-Basis`；**未获批的判分语义变更禁止上车**（先写决策卡）。即使已获批，改判分语义的单元也**独占一车**，不与其它单混装。
- **A5 规则语义独占**：改 `check_frozen.py` 的**检查规则语义**（新增 / 收紧 / 放宽检查）的单元独占一车，不与其它单混装。
- **A6 并发/生命周期不同车**：CLAUDE.md 人定「并发包不合成 PR」继续生效。
- **A7 规模 1–5 单元**：超过 5 拆车（冻结窗口越长，后续单被拖得越久）；全部单元都下车（0 单元）时取消车次，不发空车。

## 4. 组车步骤（车长按序执行）

1. **起车**：从 `origin/main` 建 `frozen/train-<YYYYMMDD>-<n>`。
2. **上车**：按 A2 的文件面分组顺序把各单元分支的增量提交 `cherry-pick`/rebase 到车次分支；同一文件面的单元严格顺序，后一单元在前一单元落地后重放。
3. **固定 HEAD**：所有单元代码已提交、工作树干净（`git status --porcelain` 为空）。`--write` 要求登记文件的工作树字节等于 HEAD 字节，所以这一步不能省。
4. **写事件**：每单元一个 `frozen/<YYYYMMDD>-<单号>.md`（模板见 `frozen/README.md`）。`Change:` 的 old/new 分两种情况：**单一变化**（该文件在本车次只有一个单元改）old 取 `merge-base(origin/main)` 的 blob 完整 SHA256、new 取当前树；**同文件多单元**按落地顺序形成 `a→b→c` 唯一链——链首单元 old 取 merge-base 的 blob，后续单元 old 取前一单元落地后的树摘要（即前一单元 new）、new 取本单元落地后的树。新增文件 old 用 `-`，删除用 new `-`。两条边共用同一个 old 会被机器判重复边而 FAIL。
5. **一次 `--write`**：`python3 -I scripts/gateway-parity/check_frozen.py --write`（车长执行；`--write` 不能与 `--base` / `--print-ledger` 组合）。
6. **自检**：`check_frozen.py`（无 `--base`）、`check_frozen.py --base origin/main`、`test_safety.py` 全过；装置目录无 `__pycache__`。
7. **一次车次复算**：在车次 HEAD 跑一次全量真实复算（本装置当前口径：`check_ruler.py` k=5 + E1 `--suite calibrate`），产物落车次证据目录。每单元从这份产物摘自己的判据切片，写进事件的 `Calibration-Evidence` 与单的证据文件。多车次 / 多装置同时复算共用 `/tmp/pdrs-parity.lock`，但两个入口的等锁能力不同：`check_ruler.py` 一次复算在**同一把锁里串 6 轮**（单轮 timeout 上限 900 s、默认 180 s，整程无机器封顶），等锁默认 600 s、可用 `--lock-timeout` 调大；E1 `--suite calibrate` 的等锁**硬编码 30 s 且无旗标**，并行时只能错峰。其它装置（`scripts/parity`、`cli-parity`）合法持锁时间可能更长，`--lock-timeout` 应按最大竞争者持锁时间设置。等锁超时按失败处置（换证据目录重跑），不改判据。
8. **提交登记**：事件文件 + `FROZEN.md` 作为一个独立提交（例：`chore(frozen) 车次 <n>：N 单登记`），提交信息逐个列单号。此后进入冻结窗口。
9. **出车**：PR 正文列单元表（单号 / 改动文件 / old→new 摘要 / 两家评审引用 / 复算切片路径），过常规合并闸门（两家非作者覆盖当前 head；登记正确性按 §6 增量复核）后入 merge queue。

## 5. 失败处置

- **F1 上车前未收敛**：该单元不上车，其余照常发车；下车单重挂下一车，不阻塞整车。
- **F2 上车后、固定 HEAD 前单元自检红**：指的是 §4.3 固定 HEAD 之前（工作树尚可改）各单元自己的测试与装置自检。能定位到单文件就把该单元摘掉后重排；定位不到就整车挂起（`state/board.md` 记缺口）。
- **F3 已 `--write` / 已推送后发现问题**：**不改写已合入的事件**（append-only 只约束 merge-base 已有事件；车次内未合入的事件可改写或摘掉，见 F4）。分两种：
  - **普通修复**（判分语义未变）：在同一分支追加 `<单号>-r<n>` 事件（old 取当前树、new 取修复后树）→ 重跑 `--write`、§4.6 与 §4.7（装置字节已变，换新车次证据目录，切片更新）。
  - **触及判分语义 / 需回滚该单元**：整车停；回滚用**新提交**（不 force push）。回滚必须把机器边一起收拾，否则 `check_frozen.py --base` 必然 FAIL（链从 merge-base 起算，机器不会自动消解）：
    - **摘掉该单元未合入的事件**（append-only 只约束已合入事件）；
    - 若同一文件上还有**后续单元**，把它们重放到回滚后的树上并重建各自的 `Change:` 边——中间断一环，后续边全部失效；重放后的单元坐标变了，须**增量重审**并更新其 `Calibration-Evidence` 切片；
    - 只回滚该文件**最后一个**单元时才会出现「回滚后 old==new、残留事件被判 stale」的情形；中间单元回滚是**断链**。两种都不改已合入事件。
    收拾完重跑 `--write`、§4.6、§4.7，其余单元在新 HEAD 重跑复算后再出车。
- **F4 rebase / 合并队列冲突**：解决冲突后必须重跑 §4.6。若冲突改了任一登记文件字节，除重跑 `--write` 外，还须在解冲突后的 HEAD **换新车次证据目录重跑 §4.7**（旧产物的装置字节摘要已失配、不可复用），并更新受影响单元事件里的 `Calibration-Evidence` 与判据切片。车次内**尚未合入**的事件可改写 old/new，已合入的事件不可改（另开 `-r<n>` 事件）。
- **F5 同一处修 3 次不过**：按 CLAUDE.md 挂起该项，其余单元照常出车。

## 6. 评审强度（不降，只换聚合粒度）

- 每单元仍是**两家非作者**，评审对象是该单元的增量 diff；两份引用写进事件，机器校验两条不同谱系、不同引用（只校格式，不校内容）。
- 车次 PR 的闸门只增加**登记正确性**增量复核，四项：① 单元 diff 与已评审 head 逐字节一致（`git diff` 相同，防止评审后偷改）；② 事件 `Change:` 的 old/new 与树摘要一致；③ `FROZEN.md` 行由 `--write` 生成（重跑 `--write` 应无差异）；④ 每个复算切片可点开且指向真实产物。这**不是**重审语义。
- 判分语义变更、检查规则语义变更仍需人复核 / 签字；车次不改变这一点。

## 7. 与既有文件的关系

- `frozen/README.md` 描述的机器事实不变：一次 `--write` 重写哈希列表，事件按文件建边。本文件规定的是**流程**：一次 `--write` 可承载 N 个事件（N 张单），每个事件仍各自两评审、各自证据。
- 机器侧本来就支持：`check_registration` 对每个变化文件独立建边，多事件可拼成唯一链，缺边 / 重复边 / 陈旧边一律失败。本次不改检查器。
- 实测证据（两单两事件一次 `--write` PASS；摘掉一单的事件只报该文件；分叉边「同 old 不同 new」FAIL；逐字节重复事件 FAIL；同文件两车链 PASS）：`_tmp/20261105-frozen-batch/proof/proof.log`。
- 历史佐证：`64635d201` 已以 3 个事件（#1194 / #1206 / #1221）+ 一次 `--write` 合入 main——机器早已支持，缺的只是流程与验收口径。

## 8. 快查

| 场景 | 上车？ |
|---|---|
| 只新增 `diffs/<ID>.md` | 不用，直接按账本规则提 PR |
| 改 `compare.py` / `session_domain.py` / `common.py` 等 A2 脚本 | 上车 |
| 改 `check_frozen.py` 的检查规则语义 | 上车，且独占一车 |
| 改 `scripts/parity/` 或 `scripts/ui-server-parity/` | 不上这车，走各自装置 |
| 改判分阈值 / 分母 / 映射 | 先决策卡；未获批不上车 |
| 改 `FROZEN.md` 叙述或 `accepted-diffs.md` | 不是车次问题，走原批准程序 |
| 并发 / 生命周期类改动 | 不同车 |

## 9. 车次记录（追加，不倒序）

| 车次 | 日期 | 单元 | 结果 | 证据 |
|---|---|---|---|---|
| — | — | 尚无车次运行 | — | — |
