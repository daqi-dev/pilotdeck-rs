# negative-controls — 负控规约：变异必须证明杀得死

> 负控不是「多跑几遍测试」，是**证明每条防线真的会拦**：把防线改坏，绑定的测试必须当场死。
> 这里立的是负控产出该长什么样的规矩；怎么跑、跑多久，见开工手册。

## 变异开关契约（#996 起）

文本替换式负控（改产品源文件 → 跑测试 → 还原）每条变异都要重编译，且锚点漂移要靠人盯。
从 #988 试点、#996 起批次迁移为**编译期开关**，契约如下。

### 产品侧：只许加开关分支

- 产品代码里的变异点只允许 `crate::mutants::on("<kebab-case-名>")` 分支形态：
  守卫禁用写 `if !crate::mutants::on("名") && <原条件>`，值翻转写
  `x = !crate::mutants::on("名")`，分支选择写 `if crate::mutants::on("名") { … } else { … }`。
  **开关关闭时逐点还原原语义**，不得顺手改写正常路径。
- `pdrs-mutants` feature 不进默认图；无 feature 时 `on()` 是 `const false`，所有分支折叠，
  普通构建 / 发布构建零行为变化（`cargo test` 无 feature 全量通过 + `--features pdrs-mutants`
  编译通过是迁移的前置证据）。
- `mutants::on` 的两道边界由负控看住（`edgeclaw-service-facade/switch_negative.py`）：
  空名选择器恒 false；非 test 构建开 feature 仍恒 false
  （integration `tests/mutants_boundary.rs` 链接的就是这种构建形态）。

### 执行器侧：预编译、独立进程、三相

- 执行器共享 `_shared/mutant_switch.py`（fixture 的 `schemata.py` 只声明
  `switches = [(名, 源文件, 绑定测试, 断言锚点)]` 表）：
  - **预编译一次** `cargo test --lib --features pdrs-mutants --no-run --message-format=json`
    定位 feature 库测试二进制；每个开关一个独立进程（`--exact`）以
    `PDRS_MUTANT=<名>` 选择，跑完即弃，**全程不改工作区源码**。
  - **三相齐全**：positive（无选择器，绑定测试必须过）→ negative（开选择器，必须以
    绑定断言锚点死掉）→ restored（无选择器再过一遍）；任何一相失败记 INVALID，
    后续开关照跑，最后统一 summary，存在非 KILLED 则退出非零。
  - KILLED 的证据是**具名测试 FAILED + panicked + 断言锚点**，编译失败、超时、错测试
    一律不算杀。
  - `PDRS_MUTANT` 是执行器的输出，不是输入：子进程环境先清理再注入；
    summary 用 `summary.json.tmp` + `rename` 原子发布，跑一半被杀也不会留下半份结论。
  - 工作树用 tree_lock 互斥，源文件哈希记入 `source.json`（记录的是工作树哈希，
    不是 HEAD 等价声明）。
  - **分母核对（#996 fix-r4 起）**：执行器跑完后把实际执行的开关名集合与
    `anchor_selfcheck.py` 同一提取函数（`switch_bindings`）得到的静态声明集合比对；
    不等（含显式选择超出声明集合）即整套 `status=INVALID`、列出差集、非零退出。
    这是 `main()` 等执行期改表（静态白名单与导入期对账都看不到的地方）的结构性闭合。
  - **分母核对覆盖全部执行入口（#996 fix-r5 起）**：声明源不是 `switches` 表的
    执行入口走同一共享 `mutant_switch.enforce_denominator`，声明集合由检查器对
    legacy 变异表的同一静态提取（`static_mutant_names`）得到，**不取运行期表**：
    edgeclaw-service-facade 的预编译套件（`schemata.py`，声明源为其
    `negative.py` 的 `MUTANTS`，显式选择即用户请求的子集）与文本替换边界负控
    （`switch_negative.py`，声明源即自身的 `MUTATIONS`）。
  - **空执行表也是漂移（#996 fix-r6 起）**：运行期把整表清空时没有可标记的行，
    而 `any()` 对空结果列表为假——退出码与 summary 都会假通过。分母核对在
    drift 且执行行为空时补一条套件级 `switch-denominator` INVALID 行（reason
    即差集），使每个入口的最终失败判定与 summary 都带上漂移原因，非零退出。
  - **turn 族五个 switches 表入口全接入（#996-pr2 fix-r2 起）**：turn-runner-tail /
    turn-gates / turn-prompt-hooks / turn-start-notices / tool-budget-cwd 的
    `schemata.py` 一律以 `script=`（自身）与 `requested=`（用户请求）调共享 `run()`，
    不再漏过执行器侧分母核对。`_shared/test_mutant_switch.py` 的入口级自测对五个
    真实入口各覆盖运行期缩表 / 清空表（INVALID + 非零退出）与合法显式子集（保持
    绿）；`--negative-controls` 把 `enforce_denominator` 置 no-op（等价于入口不传
    `script`）后，缩表 / 清空表自测必须在 `#996-pr2` 断言处死。
  - **迁移不改判杀语义：nonce 回执在开关执行器上保留（#996-pr2 fix-r3 起）**：
    旧入口以 `matrix_main`（`negative_suite.ReceiptRun`/`judge`）判杀的控项，迁到
    共享 `mutant_switch.run` 后必须传 `receipts=True`——负控相子进程携带本轮回执
    配置，判杀只认被绑定断言写出的本轮 nonce 回执加具名 FAILED 行；setup 失败、
    诊断文本恰含锚点、回显 nonce 均判 INVALID（#889 replay 反例对迁移后入口重跑，
    `_shared/test_mutant_switch.py` 的 `ReceiptProtocolTests`）。#996-pr2 五个旧入口中
    仅 tool-budget-cwd 用回执判杀，已接回；其余四个旧入口为文本 classify，语义
    不变。#996 批次三按旧基线逐夹具判定，不按新入口偏好：live-session 旧
    `matrix_main` 表以回执判杀（基线 reason 为具名断言 + 本轮 nonce 回执），迁移后
    传 `receipts=True`；live-recording 旧脚本走 legacy `run_cases`/classify 判杀
    （基线 reason「bound test failed at its assertion」且无 `.receipts` 目录），不传
    回执。`--negative-controls` 把 `judge` 换成 prose 判杀（迁移丢回执的形状）后
    replay 自测必须死在 `#996-pr3` 断言；把回执配置改为不下发后，真实回执判杀
    自测必须死在 `#996-pr3` 断言。

### #749 原生 WS 的逐项编译及 nonce 回执执行形态

`native-ws-entry/negative_controls.py` 保留薄的逐项执行入口，以支持 `--compile-only`
（每个选择器分别执行 `cargo test --no-run`）和 `negative_suite.ReceiptRun/judge`
的专属 nonce 回执。没有源码替换；复用 `tree_lock`、`run_owned`、`wait_for_slot`，
以及共享 `clean_environment`、`record_sources`、`verify_unchanged`、`enforce_denominator`。
源记录是工作树 SHA，不声称等于 HEAD。异常后仍执行 restored，每行原子发布汇总；
全选项执行后任一 SURVIVED/INVALID 非零退出，编译失败不算 kill。
该脚本仍按 switches 表接受完整 anchor_selfcheck，不申请 UNSUPPORTED 豁免。

### 检查器威胁模型（2026-10-29 人定，#996）

`anchor_selfcheck.py` 的 switches 表审计承诺的是**防无意漂移**，不是对抗蓄意混淆：

- 承诺覆盖：负控作者改了表 / 锚点忘了同步；随手引入让执行表缩水的动态写法
  （别名、拼接键、遮蔽内置等，无论出现在模块顶层还是 `main()` 里——后者由
  执行器侧分母核对兜底）。审计子进程非零退出即 ERROR；唯一可接受记录须携带
  本轮随机 nonce（父进程生成、经 argv 传入、bootstrap 在执行被审模块前从
  `sys.argv` 移除，模块代码读不到）。
- **明确不在范围内**：蓄意混淆——伪造审计输出、`os._exit` 抢跑、运行期动态改名
  之类刻意构造的绕过。负控脚本每轮经非作者评审，这类蓄意形态在评审层拒收，
  检查器不再逐形态追堵（Python 动态性下不存在完备静态解）。
- 分母的真相源在**执行期**：见上文「分母核对」。

### 迁移契约：先对表，再删旧表

- 迁移一个 fixture 前，先在**同一 HEAD** 跑旧文本替换机制与新开关机制各一遍，
  两份 KILLED 集合**逐条一致**后才允许删旧表（旧 `negative.py` 整脚本删除，不留双轨）。
- 迁移后 `anchor_selfcheck.py` 以 `switches` 表计数：`::mutants::on("<名>")` 在目标源文件
  中必须**恰好出现 1 次**；声明了 marker 的脚本若定义 `switches` 表，按 table 信号管住。
- 旧机制因避让（在途分支改同一文件）暂时不能跑的，推迟批次，不换文件、不放宽断言。

### 自管执行脚本的判决不变式（#1666 / #1734）

`tests/fixtures/*/negative_controls.py` 这类自己管循环、自己判杀的 Python 入口，判决路径
必须满足三条不变式。三条的共享实现都在 `tests/fixtures/negative_selection.py`，脚本不各写一遍。

**覆盖范围（重要，别当全集）**：本轮迁移的是 #1666 点名的第一批，加上同形的第二批——
`settings` · `memory-maintenance` · `upload-gateway` · `html-timing` · `interaction-bus` ·
`node-io` · `project-files-linux-dirent` · `network-timing`（含 `negative_private.py`）·
`sdk-settings` · `settings-gateway` · `interaction-values` · `turn-submit-admission` ·
`turn-runner/runner_negative.py` · `cli-parity-mock-gateway`，以及后批加入的
`memory-gateway` · `snapshot-projection` · `regex-proto-budget`（#1456、#1265）与
#297 r6 起的 `issue-297` · `issue-297-domain-fold`（迁移自原 main 形状，启动四拒
全走 `refuse()`）与 `sdk-mcp-session-tools`（#1433，`negative_selection` 契约 +
`EXPECTED_NAMES` 静态名册）、`mode-routing`（#1354 起同形自管）、`same-json-surfaces`
（#2422 起自管，#2425/#2431 硬化：CLI 对齐为 `OUT [--memory] [NAME ...]` 后进
`test_negative_optimize_guards.py` 三条拒跑矩阵）、`session-reservation-2391` ·
`session-delete-2392`（#2391 fix-r1 起即按 `negative_selection` 契约落形：
`EXPECTED_NAMES` + `claim_dir` + `verdict_exit`，`--check` 进 CI）、
`manager-gateway`（#2309 fix-r2 起完整落形：`EXPECTED_NAMES` + `registered` + `select` +
`claim_dir` + `missing_target`/`refuse_unusable_target` + `verdict_exit`，case 循环自管、
先核对 verdict 名册后发布，进 `test_negative_optimize_guards.py` 拒跑矩阵；fix-r3 起硬化：
变异写入与逐项恢复走 `write_source_atomic`、自持窗口装 SIGTERM/SIGHUP/SIGQUIT 还原处理器、
`pending.for_script` 以入口自身路径为身份键且作为启动前置、summary 载荷记 `selected` 执行集、
名册防线由 `test_negative_roster_guards.py` 机检并进 CI）与
`utf16-residual-sort`（#2131 起
同形自管：`EXPECTED_NAMES` 静态名册 + 启动拒跑全走 `refuse()` + 进 `mutating_sources`
之后的守卫失败作废已跑行、发布 INVALID 后按四分表退 1（延迟信号退 128+signum），
同批进三条拒跑矩阵），以及 `issue-2495`
（#2590 fix-r1 起同形：启动四拒 + `EXPECTED_NAMES` 静态名册 + stale 扫描入树锁；
fix-r2 起守卫段领锁（等待上界 60 s）超时本身也拒跑（rc 2，非 traceback）、
`--compile-only` 汇总打
`COMPILED=n/N`，并进 `test_negative_optimize_guards.py` 矩阵与 CI `--check`；
fix-r3 起变异段领锁超时同口径（rc 2 never-started；fix-r3b 起相位定界：
取得阶段的领锁超时或树脏 `SourceBaselineError` 都拒跑并撤走空的认领目录，
utf16 `startup_refusal` 先例，证据目录既不发 summary 也不留认领形状；
取得之后含还原窗口的失败作废已跑行、发布 summary 并退 1），
名册不符只发 `roster-mismatch.json`（原子写）并退出 1、`--check` 带位置
参数、空输出槽或 `--compile-only` 即拒跑）。
**其余同族入口尚未迁移**，
由 #1666 按目录分批推进，名单是机器可检的：`tests/fixtures/test_negative_optimize_guards.py`
的 `PENDING_BARE_ASSERTS` 登记每个仍含裸 `assert` 的 `*/negative_controls.py` 及理由，
该表只减不增——新脚本带裸 `assert` 会让 CI 红，迁移一个就在同一次改动里删掉它的条目。
登记表由 AST 守住、`rglob` 扫任意深度，另加三个已迁移的异名入口与共享 harness
`_shared/negative_control.py`（`mutation_suite` 与 `run()` 的判杀守卫）。
`negative.py` / `*_negative.py` 命名的同族入口仍不在扫描面内（#1666 后续批次）。

1. **分母是静态字面量，不是运行期表长度**。表后写 `EXPECTED_NAMES`，启动即
   `registered(表, EXPECTED_NAMES)` 逐项（含顺序与重复）比对，漂移就拒跑。用 `len(表)`
   当分母时，删掉一行只是把这一轮悄悄变短：summary 仍报 `N/N killed`；整表被清空则打印
   `0/0 killed` 并退出 0——零分母的运行没有测过任何东西。
2. **空结果永不返回 0**。收尾用 `verdict_exit(rows, key=…)`：至少一行且全部 KILLED 才 0；
   空选择、SURVIVED、INVALID、SKIPPED 一律 1。这是 `0/0 killed` 假绿的正面堵口。
3. **守卫写成 `raise` / `refuse()`，一个都不依赖 `assert`**。`python3 -O` 与
   `PYTHONOPTIMIZE=1` 会把 `assert` 整条剥离：名册核对、锚点计数、恢复哈希、`assert killed`
   在优化解释器下一起消失，负控于是静默放行。`tests/fixtures/test_negative_optimize_guards.py`
   对每个脚本复算：同一个未知变异名，加不加 `-O` 都必须以 2 拒跑、在 stderr 点出该名字、
   且 stdout 不得出现 `0/0`；它先自证本解释器确实在 `-O` 下剥离 `assert`，否则整组结论无意义。

退出码四分（适用于上述自管脚本；共享执行器 `run_cases` / `mutation_suite` 的四分见
`tests/fixtures/README.md`，两者不互相替代）：

| rc | 含义 |
|---|---|
| 0 | 选中的变异全部 KILLED（`--compile-only` 时全部 COMPILED） |
| 1 | 确实跑过但没拿到全绿结论：SURVIVED / INVALID / SKIPPED / 基线重放不通过 / 收尾恢复校验失败 |
| 2 | 从未开跑，本轮没有判决可读：未知或重复的变异名、名册与静态清单不符、marker 越界或取自判杀条件自身的 token、输出目录已存在（`negative_selection.claim_dir`）、`CARGO_TARGET_DIR` 不可用（未设置、空串、纯空白，或 `target_environment` 拒绝的 dot / 文件系统或仓库根 / `src` 内）、树脏或领锁超时、oracle 读不了 |
| 42 | 全杀但基线不是 HEAD，只是非宿主证据（用到的脚本按自身规约） |

分界判据是「有没有真的开始变异」：完整形态下，`mutating_sources` 的取得阶段（领锁等待、
洁净检查、快照读——任何变异字节落盘之前）返回的都算 2，取得之后（含 `__exit__` 还原窗口）
被打断的算 1（各入口实现进度不一，见下与 #1666）。
分界的结构形态是取得旗标（`same-json-surfaces` / `utf16-residual-sort` 的 `started`），
不由异常类型白名单保证——#2590 fix-r3 实测教训：`TimeoutError` ⊂ `OSError` 且是
`errno.ETIMEDOUT` 的标准映射（PEP 3151），按类型把取得段超时收进拒跑的同时也会把
还原窗口的超时误收（fix-r3b 已修）。issue-2495 当前实现：取得段的 `TimeoutError`（领锁
等待）与 `SourceBaselineError` 映射为 2——后者除树脏（diff / 未跟踪 / assume-unchanged /
回读不一致 / 缺文件）外，还覆盖「oracle 不可读」的**已包裹形态**：`_head_bytes` 把 git
失败包成 `SourceBaselineError`，`_workspace_literal_inputs` 把扫描器运行期的
`(ValueError, OSError, SubprocessError)` 也包进去；取得段的**裸**异常——预检
`_assert_input_areas` 直接调 `_git` 的 `RuntimeError`、`_ensure_workspace_scanner` 冷启动
cargo 失败、被守文件 `read_bytes` / `stat` 的裸 `OSError`——仍按失败关闭落 1（不会假绿，
有实测钉：issue-2495 测试取得段裸 `RuntimeError` → 1 + summary 全 INVALID）。全取得段覆盖
与同族入口旗标化见 #2590 跟进（条目 22）；信号不走此表（128+signum，
还原窗口的 SIGTERM 不发 summary，见下）。
被信号直接杀死（SIGKILL 137）不在表内，看证据是否完整；以下退出码都以前台、未继承
SIG_IGN 为前提（nohup / 后台链式宿主把 SIGINT / SIGTERM 设成 SIG_IGN 传给子进程时，
信号被静默吞掉，进程既不退、证据也不终止，宿主需 SIGKILL / 超时兜底；实测：SIG_IGN
子进程对 SIGINT+SIGTERM 均存活）。没装取消处理器的入口（如 `settings-gateway`）
按信号默认处置退出：守卫回写窗口外被 SIGTERM 当场杀死、退出码 143（SIGHUP 129 /
SIGQUIT 131 同理），树可能停在变异中、证据不完整；SIGINT 由解释器自身转成
KeyboardInterrupt、未捕获退 130（实测 `raise KeyboardInterrupt` → 130；settings-gateway
顶层 `except BaseException` 在 `entered` 翻转前的启动段把它收成拒跑码 2，之后
record_failed_run 再抛、退 130）；落在回写窗口内则先完成还原与复核再退 128+signum
（守卫对 SIG_DFL 同样换 handler 延迟；继承来的 SIG_IGN 不换，窗内收 SIGINT 既不
延迟也不退）。装了取消处理器的自管脚本分两族，都在装好处理器之后才谈取消语义，
安装前的启动段一律按信号默认处置：`issue-297` 两入口（`issue-297`、
`issue-297-domain-fold`）在守卫回写窗口外的取消当「从未开跑」拒跑，退 2；
`same-json-surfaces` 与 `mode-routing` 的 `terminate_cleanly`（session-domain-cleanup
#892 r23 镜像）窗口外只对它注册的三个信号（SIGTERM / SIGHUP / SIGQUIT）退
128+signum，SIGINT 不在其列。落在共享守卫回写
窗口内的取消一律先完成还原与复核，随后按相位分码：issue-297 的编译预检
（`compile_controls` 自己的 `mutating_sources` 窗口）让守卫退出抛的
`SystemExit(128+signum)` 穿过 `except Exception` 成为进程退出码，且不写
compile-summary；same-json / mode-routing / 无处理器入口同样在守卫退出时抛
`SystemExit(128+signum)`；issue-297 的判定段（`negative_suite.run_cases`）则把
该退出归一化回 2、发布全 INVALID 总结——判定段窗外、窗内都是 2。经共享执行器
`mutation_suite` 的入口（如 `network-timing`）不套用本段规则，窗外取消记 rc=1
（见 `tests/fixtures/README.md` 的执行器既有语义）。
这套分界不依赖 `assert`，所以 `-O` 下不变。已知的唯一例外是 `regex-proto-budget`（#1265）：
它在 `claim_dir` 之后、第一个变异之前自己跑一遍 crate 的 compile_fail doctest 基线（CI 跑不了
特性门测试），红了也以 2 拒跑、`OUT/doctests.log` 留档——这一次 2 带证据目录，是刻意的。

实现进度按行读：**已迁移脚本**对未知 / 重复变异名、名册漂移、输出目录已存在
（走 `claim_dir`，`FileExistsError` 不再落成 1）、`CARGO_TARGET_DIR` 不可用
（未设置 / 空串 / 纯空白走 `negative_selection.missing_target()`，dot / 根 / `src` 内走
`negative_selection.refuse_unusable_target()` —— 后者不重列取值，直接问
`negative_process.target_environment`，取值规则只有那一处）都返回 2；
`TARGET_OPTIONAL` 登记的四个入口按模式各自豁免：`interaction-values` 与
`turn-submit-admission` 只在**变量未设置**这一档豁免（按 #1166 用共享默认 target），
空串与纯空白由各自的 `blank_target()` 前置守卫以 2 拒跑，dot / 文件系统或仓库根 /
`src` 这类目标同样在领取证据目录之前以 2 拒跑——这两个入口是直接 `try:
target_environment(...) except RuntimeError: refuse(...)`，效果等价于
`refuse_unusable_target()`（同一份取值规则，只是没有包成谓词）；
`cli-parity-mock-gateway` 与 `same-json-surfaces` 自身源码不读 target（登记表守卫断言无任何
读取形态），**四档全豁免**——但两者委托的 `mutating_sources` 窗口会经 `_ensure_workspace_scanner`
读 `CARGO_TARGET_DIR`（未设置取共享默认、空串/纯空白/dot 在共享规则处拒绝）且冷主机上可能 cargo 编译
扫描器，故「不编译 Cargo」不成立（#2552）：「未设置」会启动矩阵 300s 子进程跑不完的完整 guarded 批；
空串/纯空白/dot 在 same-json-surfaces 经 #2431 started 旗标以 rc 2 拒（#2552 实测：rc 2、stderr 点名
target、无证据目录，实际可矩阵承载——纳入矩阵待登记表支持「自身源码不调 target_environment」的
形态，#2552 跟进）、在 cli-parity-mock-gateway 以启动异常逐行收成 INVALID 证据后退 1（证据目录已领，
留档不撤），均非入口自身的 target 守卫；
**树脏 / 领锁超时 / oracle 读不了**三条，自管入口里 `settings-gateway`（`entered` 旗标）、
两个 `issue-297` 入口（编译预检的 `guard_failed` → 2，只覆盖源码守卫、不含 oracle 读取）、
`same-json-surfaces`（#2422 起的 `started` 旗标，#2431 落了 rc2 回归钉）与
`utf16-residual-sort`（#2131 起的 `started` 旗标 + `startup_refusal` 撤回空证据目录，
覆盖 enter 两边；无 oracle 读取）已实现
（`sdk-signals` 的相位路由 #1885 r8 也实现；`_negkit/contract.py` 的 `entered` 旗标
口径相反——取得段失败退 1、进守卫后失败退 2，见下「另」段，不算本条已实现；
#2590 fix-r2 起 `issue-2495` 的守卫段领锁超时同样以 2 拒跑、非 traceback（fix-r3 起
变异段领锁超时同口径；fix-r3b 起变异段取得时的树脏 `SourceBaselineError` 同样拒跑
并撤走空认领目录）；
本句枚举的是 `test_negative_optimize_guards.py` 登记面，非全集），
其余脚本是未捕获异常 → 1，仍按「失败关闭」处理（不会假绿），登记在 #1666 的后续批次。
宿主分诊时不要把这三种边当成已覆盖。
另：已迁移脚本一律先跑完启动守卫再 `claim_dir`，所以「从未开跑」的拒跑不会留下证据目录
（例外：`regex-proto-budget` doctest 基线——认领 OUT 之后的红基线；`issue-297` /
`issue-297-domain-fold` 的 `guard_failed` 在退 2 前先写 `compile-summary.json`、
`settings-gateway` 的启动分支在 mkdir 之后才退 2（`entered` 旗标翻转前的
`BaseException` 收成拒跑码 2，见上）。`_negkit/contract.py` 是另一种口径，**不并入**
上句：它先 `claim_output`（mkdir 并写 `.claim`，认领目录**非空**）后进守卫，
`entered` 只在 `guard().__enter__` 成功返回后翻转，退出码按
`0 if success else (2 if entered else 1)` 计算——守卫进不去（含树脏）退的是 **1**
而非 2、进守卫之后的失败退 **2**，与本节四分表「从未开跑 → 2 / 跑过 → 1」两侧都相反
（它自己的 docstring 即如此声明）；失败也不发布 summary、认领目录连同 `.claim` 留在盘上。
撤目录与退码口径对齐登记 #1666 跟进）。
`network-timing` 的正式路径进共享执行器 `mutation_suite`（它先占目录再取环境），因此缺
`CARGO_TARGET_DIR` 的拒跑放在 `main()` 里、早于名册与源码守卫；共享执行器自身遇到启动期的
`refuse()`（rc 2）时不发布 `summary.json`：目录仍为空则撤掉（目录已被外部删掉时按「没有残留」
静默），已有文件则留下并在 stderr 打出残留路径——「从未开跑」既不发布看起来像跑完的证据，
也不把别的写入者留下的文件删掉。两条子分支各由 `test_negative_adaptive.py` 的
`test_startup_refusal_is_never_started_and_leaves_no_evidence` 与
`test_startup_refusal_leaves_written_evidence_in_place` 覆盖（`test_negative_optimize_guards.py`
覆盖的是各入口自己的启动守卫，两者不互相替代）。
这些都由 `test_negative_optimize_guards.py` 机检：不存在的变异名、`CARGO_TARGET_DIR` 不可用
（未设置 / 空串 / 纯空白 / `.` 四档）、目录已存在三条矩阵覆盖全族。豁免按**模式**登记
（`interaction-values` / `turn-submit-admission` 只豁免「未设置」，由
`negative_process.target_environment` 补共享 target；`cli-parity-mock-gateway` 与
`same-json-surfaces` 自身不读 target、四档全豁免——两者委托的 `mutating_sources` 窗口实际会读
`CARGO_TARGET_DIR` 并可能冷启动 cargo 编译扫描器，非「不编译 Cargo」（#2552），其中 same-json 的
空串/纯空白/dot 实为 rc 2 可承载、纳入矩阵待登记表新形态（#2552 跟进），逐条理由见登记表），
每条带理由与「理由所依赖的源码事实」断言（`target_environment` 必须是真调用，
AST 判；豁免多档的入口不得出现任何 target 读取形态），加个名字不能静默缩小矩阵。
共享执行器的四分数值仍以 `tests/fixtures/README.md` 为准，不与本节互相替代。

### 负控发现锚点 `DISCOVERY_ANCHORS`（#2133）

与 `PENDING_BARE_ASSERTS` 同级的表级机器可检约定：宿主按 PR diff 给每个改动的
`src/**/*.rs` 与 `fixtures/**` 文件生成键，再用字面 `git grep` 决定排程哪张表，
因此表里构建的路径必须让键字面出现；做不到的表在模块级声明 `DISCOVERY_ANCHORS`
逐条登记（没有这类拼接的写空元组），`src/pilotdeck-gateway/tests/fixtures/
test_negative_discovery_anchors.py` 逐表断言，并由 CI 步「网关负控发现锚点自检
（#2133）」执行（该步的同形接线守卫在
`src/pilotdeck-gateway/tests/test_metadata_io_driver.py`）。键的推导规则、等价
写法与 `KNOWN_BLIND` 跟进清单以 `src/pilotdeck-gateway/tests/fixtures/README.md`
的「负控发现锚点 `DISCOVERY_ANCHORS`」节为正本，本节不复制第二份。
