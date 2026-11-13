# 决策卡 2026-11-13（coord2/e7）



## #3056 fix-r4 续开（结构性轮；判据 4 与判据 1 冲突的 AI 裁定，AI 代批，待人复核）

- 卡点：`state/decisions-20261110-longtail-closure-rule.md` 判据 4（rN ≥ 3 修复轮出现任何 P0 → 不再开下一轮修复，停手转人拍板）与判据 1（同逃逸族第 2 次 → 续开必须换结构性方案）在 #3056 同时命中：fix-r3 评审 DS boundary#1 与 MM boundary#1 **双撞 P0**（TITLE_ECHO 收紧后 `#N` 与 `fix-rN` 之间有界间隔形全漏判 → 修复轮识别漏判 → fixtype/residual 整套跳过 = 闸门假 PASS），该洞是 fix-r3 自身引入的回归，且与 r2 的 DS boundary#1（首行识别）同属 TITLE_ECHO 逃逸族第 2 次出现。
- 谁受影响：#3056（PR #3074）修复链；一切依赖 `fixtype=`/`pdrs-residual:` 检查的修复轮 PR；长尾卡两条判据的后续适用。
- 能否撤回：可（只影响是否续开一轮，不改任何已合内容）。
- 动不动尺子：动（check-pr-gate.py 是闸门尺子），但修法是两家评审独立同开的处方（#N 与 fix-rN 间允许有界间隔 + `#\d+(?:\+#\d+)*` 多 issue 形 + 仓内词表核验 + 两向正负控），非为变绿改标准。
- 意向：全速收口 #1357 前置；11-10 人令「全部判断不再等人拍板，AI 判定后备案」。
- 选项与后果：
  - 停链转人（判据 4 字面）：PR #3074 带已知尺子假 PASS 洞挂起，#1357 前置收口被拖；且 11-10 人令已把「转人拍板」改为 AI 判定后备案——否决。
  - fix-r4 结构性轮（判据 1 路径）：两家谱系对修法已收敛（同处方），一处正则改动灭多洞（双撞 P0 + 多 issue P2 + 首行不对称），结构性轮期间计数继续累加、不触发判据 1 停修；采纳。
- 建议（已执行）：按判据 1 结构性路径开 fix-r4（任务书 `_tmp/20261113-3056-fixr4/task.md`，只修 P0 + P1 + 同文件 a 档 P2；3 s 时间窗握手与 fstat 桩作用域两条测试装置重设计开跟进单）。
- 可复用原则：**rN ≥ 3 出现的 P0 若为「本轮修复自身引入的回归」且两家谱系对修法收敛同处方，走判据 1 结构性轮而非判据 4 停链**；判据 4 的字面停链留给「跨轮存活 P0 / 修法不收敛且无收敛处方」的情形。能机检的断言：评审双撞 P0 + 双家同处方 + 回归引入（非存活）三件齐才可开结构性轮，登记在案（本卡）。
- 实测留档：假 PASS 机制已实测坐实（`## 概述` 首行 + `fix(scripts) PR #3047 fix-r3：…` 回显 → `fix_round_from_body` = None），协调者本机跑，证据 `_tmp/20261113-3056-fixr3-review-ds/`（DS boundary#1 行内要求实测，已由协调者补做）。

## #3056 fix-r4 修 1 停点裁定（换信号源：修复轮判定从正文行改取 PR head 提交标题；AI 代批，待人复核）

- 卡在哪：fix-r4 任务书处方正则对**任务书自己正控点名的六形只过 4 形**（`fix(scripts) PR #3047 fix-r3` 的 `PR` 顺序、无 #N 形、`01:45` 时间戳间隔三处结构不匹配），停点报告已量化：词表 1668 条中类型词开头 1657 条，横跨「结构化标题」与「类型词开头的散文」两群，**单一行内正则没有能把两群分开的判别量**（R3 也只 95.7% 覆盖 + 散文误判形仍在）。这正是 r2→r3→r4 同一处三次摆动的根源（memory「lexical-verdict-fix-nonconvergence」>2 轮须升结构性的实例）。
- 谁受影响：`scripts/check-pr-gate.py` 的修复轮判定（所有带 fix-rN 的 PR）；不改冻结尺子（specs/{parity,gateway-parity,ui-server-parity}）。
- 能否撤回：可（纯工具改动，取数路径可回退）。
- 动不动尺子/意向：不动冻结尺子；改闸门判分语义（正文行匹配 → head 提交标题判定 + 正文首行兜底）。
- 选项与后果：① 换信号源（head `git log -1 --format=%s <head>` + 词表判据，正文兜底，git 不可用 fail-closed）——结构性灭洞，代价是 check() 新依赖 git 可用性（指纹分支已有同款依赖）；② R3 + 差异登记——95.7% 覆盖，72 条残余 + board 散文误判形两向登记；③ 不修只登记——P0 假 PASS 面（间隔形/无 #N 形标题回显的修复轮 PR 整套跳过）全留。
- 建议（已执行）：**①**。词表数据证明两群不可分时，换结构化信号源而不是继续加词——与本仓「词汇枚举式修判读不收敛」规则同构。
- 可复用原则：**行内词表判据在同一处 ≥3 轮不收敛时，换结构化信号源（提交标题/机器字段），不再加词**。能机检的断言：新判据在 1668 条真实词表上覆盖 = 100%（提交标题是结构化正本），六正控 + 四负控全过，`## 概述` 首行假 PASS 机制实测转抓。
- 依据原件：`_build/wt-3056/_tmp/20261113-3056-fixr4/STOP-fix1-report.md`（wordlist-shapes.txt / wordlist-struct.txt / candidate-coverage.txt 可复跑）。

## #3056 fix-r6 信号源集合化 + #1087 批 3/4 范围扩展（2026-11-13 04:0x AI 代批，待人复核）

- 卡点：① #3056 fix-r5（head 单标题信号）DS 评审 P0（boundary#1）——多提交轮尾提交为 chore/typo/merge 时漏判修复轮；② #1087 批 1 DS 评审 P1×7 中 5 条落在批 1 范围外（6 表绿打印、_shared 两件、4 表早拒）。
- 谁受影响：PR #3074（闸门判定语义）、PR #3042（批 3/批 4 范围清单）。
- 能否撤回：可——集合语义有词表全量复算兜底；范围清单是协调者分批产物，可再调整。
- 动不动尺子：#3056 是闸门（尺子类）修复轮判定，沿 fix-r4 已批的「换结构化信号源」方向续轮（单点字段→同构集合），无判分语义变更；#1087 不动尺子。
- 选项与后果：A（选）主信号扩为 `git log --format=%s <base>..<head>` 标题集合、轮号取命中 fix-rN 最大 N——两角色共同建议，结构化不受散文影响；B 恢复 r4 正文回显扫描——重蹈词表不收敛。范围扩展选「登记进最近同形批次」而非开新单加轮——避免流程马拉松。
- 建议（已执行）：fix-r6 交付 b0155c649（词表 1675/1675，差量 +7 为链上新提交）；后续 fix-r7 修掉 r6 评审 P1（单标题 finditer+max 吞引用轮号→每标题首个+跨标题 max，词表 1677/1677，DS 增量 4/4 PASS P0=P1=0）；1087 批 3/4 扩展已登记进 PR #3042 评论。
- 可复用原则：**结构化信号源漏判时，把单点字段扩为同构集合（提交标题→标题集合），不回退到非结构化文本；评审点名的同族缺口按「登记进最近同形批次」处置**。

## #1087 装置缺陷 `_fd_is_dead` 漏判 POLLHUP（批 1 补修连锁；AI 代批，待人复核）

- 卡点：批 1 补修收货实测发现 `src/pilotdeck-gateway/tests/fixtures/negative_process.py` 的 `_fd_is_dead` 只查 `select.POLLERR(8)`；macOS poll 对「读端已关的管道写端」只报 `POLLHUP(16)`（Linux 报 POLLERR，实测 `[(1, 16)]`）⇒ fix-r5 引入的 `detach_dead_streams()` 在 macOS 上对真断管是 no-op；出厂 `DeadStreamDetach` 契约测试 4 条在 HEAD 态 3 红（2 fail + 1 error），打 1 行补丁后 4/4 OK。
- 谁受影响：PR #3042 批 1 的 9 表 detach 修法（Linux CI 绿、macOS 本机实效为假——「规约与实现相反」级 P1 面）；本机所有依赖该装置的验证。
- 能否撤回：可（1 行 + 注释）。
- 动不动尺子：negative_process 属对拍装置（产品侧），不改冻结件。
- 选项与后果：① 并入批 1 补修提交——违反该轮「9 表外零改动」收敛红线；② 单开装置小修单（1 行修 + 出厂契约 3 红→绿为判据 + A/B 负控）——与「装置挡住产品开发时最小修复」条款一致（选）；③ 只登记不修——批 1 修法在 macOS 实效为假、14 条 twin 的全绿前提落空。
- 建议（已执行）：②，交付 f5e2cc959（74/74 全绿、A/B 负控、批 1 抽 3 表 twin 在 HEAD 态全绿、已知限制「fd 启动后 close 可能被复用」登记进 docstring；b1fix+装置修 DS 增量 4/4 PASS P0=P1=0）。
- 可复用原则：**装置缺陷以「出厂契约测试红→绿」为判据单独开最小修单，不搭车产品批次；跨平台 poll 语义差异（POLLERR vs POLLHUP）写进装置注释**。
- 实测留证：`_build/wt-1087/_tmp/20261113-1087-r7b1fix/`（fd-defect-probe.log、deadstreamdetach-head-vs-patched.log、negative_process-pollhup.patch）与 `_build/wt-1087/_tmp/20261113-1087-fdisdead-fix/`（全套原始输出）。

## 量纲注记：长尾卡判据 5 计数单位与 fixround-ledger 键归并口径核对（2026-11-13 04:4x，AI 核对，待人复核）

- 核对结论：**口径一致**。`state/decisions-20261110-longtail-closure-rule.md` 新增判据 5（通道兜底）计数单位 = 谱系×轮（同链同谱系累计「修复轮不可用」数，阈值 2）；`scripts/fixround-ledger.py` 键 = 链×谱系×轮号，`count` 先按键归并（每键一次；计入类失败优先于补跑 PASS；同轮多条只计 1）再按链×谱系累计，≥2 退出码 1——两侧同维（轮），归并规则实现卡的三前提与「同轮一次」口径。
- 发现并已修：`scripts/README.md` fixround-ledger 节对判据 5 的概括「同一失败第三次同一形状」**失实**（那是钉族规则 / 判据 1 的措辞）——已在 #3074 增量 a77ff02da 按卡上正本改正为「通道兜底：同链同谱系累计 2 个修复轮不可用」（doc-only 1 行，DS 增量复核另档）。ledger 代码与测试无同款失实。
- 引用规约：对 `scripts/README.md` 的引用以节标题（「fixround-ledger.py — 长尾卡判据 5 的计数登记台账」）为准、不写行号（合入后行号漂移；memory decisions-pointer-heading 同则）。

## #1087 MM r9 P1 仲裁降级 P2c（AI 代批待人复核）

### 卡在哪

fix-r9 双谱系复核中 MM 给 P1×1：per-run marker 改名（`summary.<run_token>.json`）后，复用目录里上一轮的旧绿 verdict 文件仍在，无机制指认「本 run 的文件」，外部读者按 glob 读会读到旧判决。DS 同轮未撞此条。按「等级以评审给的为准」本应挡合并修当轮，但该条属可仲裁降级的判断题。

### 谁受影响

读 `summary.*.json` 的潜在外部消费者（人/脚本/CI）；当前负控 twin（断言双判决并排比对）不受影响。

### 能否撤回

能。若日后出现真实消费者，按 #3100 第 1 条（latest.json 稳定指针 / stdout token 设计）补机制即可，不改既有文件名。

### 动不动尺子 / 意向

不动冻结尺子；不动判分语义。负控装置的拒绝契约（退出码 + stdout）未变。

### 选项与后果

1. **维持 P1 当轮修**：为不存在的读者加指认机制（latest.json 指针），改动面大、且无消费场景可验证，空转一轮。
2. **仲裁降 P2c 并入 #3100**（采纳）：静态核实仓内无消费者，退出码+stdout 是唯一在案契约；MM 评审块自己标注了该降级路径；各 runner 均独占新建 OUT_DIR，复用目录仅 refuse 路径。已登记 #3100 评论（issuecomment-6840568378，带 `<!-- coord -->`）。

### 依据（08:5x 现跑）

- `grep -rn 'summary\.\*\.json\|summary\.<token>' src/ scripts/`（.rs + .py）= **0 处消费者**。
- `_tmp/20261113-1087-review-mm-r9/supplement`（MM 自标降级路径）。

### 建议

采纳选项 2。原则沉淀：**装置输出格式的「读者消歧」问题，以仓内 grep 实际消费者为准绳——零消费者即降 P2 走设计单，不当轮空转。**

## #2476 fix-r5 收货判读（自决备案）

P1×4 / P2a×5 对照 `_tmp/20261113-2476-review-r4-both/supplement-r5.md` 逐条核实全落地，c 类四条登记齐（#3098 并入、冻结件行号陈旧、env 清单/锚点多副本、wecom unwired 待独立小单）。worker 宿主盘闸拒跑的三项（负控族实跑 / `cargo test --no-run` / clippy）按其登记转协调者补跑（盘恢复至 21 GiB 后已执行）。e9f67d5a3 已推送 PR #3097，gitleaks 净。

## 1087 r9/r10 当轮修项选择（自决备案）

r9 双谱系同撞条目（refuse_summary 缺目录噪音、陈旧注释）按「两撞即真问题当轮修」直接修；r10 追加 7 处空 glob 弱断言加钉与守卫正控 twin，均为 a 类机械修复，不另开单。

## 3056 r17 评审 7 条 P2 判定不修（AI 代批待人复核；12:4x）

### 卡在哪
#3074 fix-r17 增量 DS 四角色全 PASS（P0=0 P1=0），余 7 条 P2 去重后四簇：①`WS_GAP_MEMBERS` 无字面量钉死（sibling#1/boundary#1/concurrency#1 三撞）②`:996` 第三份手抄成员表未并入（sibling#2/simplify#1）③注释成员清单承诺与内容不符（sibling#3）④`_GAP_EDGE` 尾槽空白仅 ASCII 覆盖（boundary#2）。

### 谁受影响
仅协作工具 `scripts/test-review-roles.py` 自身的测试覆盖完备性；不影响产品代码、不影响对拍装置判分。

### 能否撤回
可撤回：若该工具某日挡住产品开发，按「最小修复」处理并引用本卡。

### 动不动尺子 / 意向
不动尺子（test-review-roles 是评审执行器，非冻结件）。

### 选项与后果
- 继续开 r18 逐簇修：本车道 r12→r17 连续六轮在同一词汇枚举位逐轮补洞逐轮出新洞（memory「lexical-verdict-fix-nonconvergence」实证，#2245 五轮/#1355 二十四轮同型），修复轮上限 3 早已超过，继续属流程马拉松。
- 判定不修 + 备案：P0/P1 全清即可合入；四簇按 2026-10-17「只做产品代码」人定，协作工具不新开跟进单（已有工具类跟进单可关），该四簇不挡产品开发。
- 开 P2-跟进单：违反「协作工具不新开单」人定，除非挡住产品开发。

### 建议（已执行）
**判定不修**。处置登记在 #3074 body「r17 后 P2 处置」段（跟踪载体 = PR 正文，非独立单）。原则沉淀：协作工具类 P2 的默认处置是判定不修，除非挡住产品开发；触发条件 = 该工具假 PASS/假 FAIL 影响到当轮产品单的评审结论。

## #3110 开立依据（1087 变异体截断式原地写；自决备案；12:5x）

MM r14 concurrency#2：frozen-manifest 负控 `negative_controls.py:153` 用 `path.write_text(mutated)` 截断式原地写把变异体发布到共享工作树真实源文件。触发需「进程被杀 + 校验窗口重合」双条件（c 类），但同族先例已有两单（#3034、memory「worker-bg-negctl-killed」/「negctl-stale-pyc-swap-payload」），且 #1087 关单收口须逐条处置——先开单挂前置，修法（原子写助手）与 #3109 发布块提 helper 可同批做。查重过：#3108（.FROZEN-* 残留）不覆盖此项。

## #2459 数组原型修法拍板（五分叉 AI 代批待人复核；14:5x）

依据探针报告 `_tmp/20261113-premise-probes/2459.md`（TS 语义 21 格实测 + origin/main 缺口定位：`js_value.rs:123` validate_coercion 对象臂遇 Array 原型断链，三挂载点消费）。分叉拍板：

- **F1 采纳**：len 数值化用现成 `to_number` 取值，堵死「own length 只判抛不取数值少读元素」的假 PASS 口。
- **F2 采纳**：String 值残留（Rust 产 `"[object Object]"` vs TS join 结果如 `"1,2"`）开 P2-跟进单承载（开前查重）；不并入本单。
- **F3 采纳**：对抗性 own 影子键（length:0、非 callable join 等两个反直觉负例）全数进单测。
- **F4 采纳**：本单限定 `src/pilotdeck-gateway/src/js_value.rs`；与 #2464（loader own-only，另探针在飞）不同文件、可同批并行派。
- **F5 采纳**：合入后撤销 `A8.3-R1:64-66` 已知缺口披露句须走冻结程序 `-R2` 更正件，独立于本单、并入冻结件流程（与 #3108 同口径，先例 be54f9ea7）。

修法定向：选项 1（js_value.rs 本地补数组原型 join 投掷判定，约 60-100 行，三挂载点零改动）；选项 2（全量值级对齐，3-5 人日）不做；选项 3（账本登记）不满足登记条件（形态实测可达），仅备档。排期：mirror-prs 积压清偿后随 premise-lanes-8 派单，任务书按本卡写。

## #2448 loader 链感知修法拍板（cfg 层方案选择 AI 代批待人复核；15:0x）

依据探针报告 `_tmp/20261113-premise-probes/2448.md`（冻结基线真实模块实测：extra 装原型 TS 读 1234 vs Rust own-only 全链 Undefined；yaml 2.9.0 实测产出 own 可枚举 `__proto__` 数据属性，形态真实可达；cfg 层继承 token/enabled/extra TS 全可见、Rust snapshot `platform()`（snapshot.rs:1367-1396）own-only 在 snapshot 处即丢）。

- **修法定向**：`loader.rs::extra_field` 内层改链感知（复用 changes.rs:303/316 现成 get_property/property_value）；**`field` 保持 own-only 是对齐不是遗漏**（TS loader 读 parse 产全新字面量 own 键），任务书写明防评审误报。
- **cfg 层拍板：方案 1（同批修 snapshot `platform()` 五个读取位）**。理由：形态实测可达、实测证据在手，不满足账本登记条件（同 #2459 选项 3 口径）；方案 2（冻结账本登记）仅备档。
- 验收用例抄报告 6 组实测格（含 C11 单源负例防修过头——「双源都定义 extra」前置必要）；邻接位点 `api_server/mod.rs:150-152` 第二读取源任务书顺带核对。
- 排期：mirror-prs 积压清偿后随 premise-lanes-8 派单；文件面 loader.rs+snapshot.rs，与 #2459（js_value.rs）不同文件可同批并行。
