# Fixtures 负控

## 待重锚（#1048）

产品 PR 仅让既有**源码文本锚点**漂移时，可以在 `pending-reanchor.txt` 追加一行登记：

```text
file-history-cwd/negative_controls.py · processor-message-unscoped · #1234 · #1235 · 2026-10-29
```

上例为格式示意，不是实际登记。五列以 `·` 分隔，依次为相对于本目录的 fixture **脚本路径**、精确变异名（或仅本脚本全部文本变异的 `*`）、漂移来源 PR、重锚 issue、登记日期。只追加本 PR 实际影响的项，不批量登记历史漂移；不允许路径越界、缺项、非法日期、重复或通配符重叠。重锚单必须写明**前置于对应阶段收尾**。

`anchor_selfcheck.py` 对登记的文本计数失配输出 `WARN`，`SUMMARY` 的 `pending-reanchor=N` 为涉及的不同变异数（一个变异有多个锚点只计一次）。未登记失配仍为 `ERROR`。登记项的全部锚点仍按原声明匹配时也输出 `WARN ... remove registration / 删除登记`，提示在重锚单处理遗留登记；不会因仍匹配而自动恢复执行。登记阶段只追加，重锚完成后的清理由对应重锚单处理。登记无法对应已检查变异、解析/清单/环境错误、编译期开关型负控均不享受降级。

宿主跳过逻辑位于 `negative_suite.run_cases`、`negative_suite.matrix_main`、`_shared.negative_control.mutation_suite`：登记项保留在结果中，状态为 **`SKIPPED(pending-reanchor)`**，计数单列 `pending-reanchor`；不变异、不运行该项正控/负控/恢复测试，不计 `KILLED` 或 `COMPILED`。同套件其他变异继续执行，源码与有效登记仍必须通过 `mutating_sources` 的 HEAD 守卫。**有任意 SKIPPED 时 rc=1**（包含全跳过、混合通过、`--compile-only`），只有全部实际判杀或 compile-only 全部实际编译才可 rc=0。无登记时原有判杀与退出规则不变；格式/身份错误非零退出，绝不能借 rc=0 掩盖未跑。

登记脚本必须已接入上述共享跳过入口并列入 `pending_reanchor.TEXT_HOSTS`（本单核验 20 个现有调用方）。尚自管执行或有先行编译/变异、名称展开的脚本，登记会报 `requires a shared skip entry`，须先在对应产品 PR 接通这些路径；本单不批量迁移。`mutant_switch` 编译期开关不适用。普通 CLI 以 `sys.argv[0]` 定位 fixture；导入调用 `run_cases` / `mutation_suite` 时可显式传 `script=__file__`；`matrix_main` 已有显式 script 参数。

## 退出码契约（#1076 fix-r1 / fix-r2）

`negative_suite.run_cases` 与 `_shared/negative_sqlite_header.py` 的退出码三分，调用方按此处理，不得把 2 当成 1 的替代：

| rc | 含义 |
|---|---|
| 0 | 全部选中项 KILLED（`--compile-only` 时全部 COMPILED） |
| 1 | 有 SURVIVED / INVALID / SKIPPED，或守卫**拒跑**（HEAD/baseline 不一致、锁忙、参数错误、锚点过期）；这是既有契约，调用方对 rc=1 的断言保持不变 |
| 2 | 取消（SIGINT/`SystemExit`，`SystemExit(256)` 归一化为 2）或恢复核验失败（守卫退出/逐行恢复无法复核字节）导致作废；**异常展开期间**守卫恢复/核验再失败同样为 2，即使 body 已先抛出拒跑异常、后一个异常覆盖前者（fix-r2 按异常对象区分：`error` 不是 body 抛出的那个对象即作废） |

`summary.json` 只在守卫成功退出后原子发布一次；取消或恢复失败时发布的是全 INVALID 总结，第二次中断允许没有总结——**缺失即失败关闭**，不得把“没有 summary”读成通过。

`negative_suite.matrix_main` 的启动拒跑同属「从未开跑」（#1734 fix-r7）：缺参数 usage、未知或
重复变异名、`CARGO_TARGET_DIR` 未设置 / 空串 / 纯空白 / 被共享 target 规则拒绝的取值
（dot / 文件系统或仓库根 / `src` 内）、输出目录已存在，一律 `refuse()` → rc 2
且不创建证据目录；它不发布 `summary.json` 失败总结，因为这些分支一行变异都没跑。其余运行期结果
仍按该函数的既有语义（SURVIVED / INVALID → 1）。

契约范围仅上述两个共享执行器。`shared-runner-failclosed/negative_controls.py` 是手工顶层脚本，其 `main()` 对所有守卫失败一律返回 rc=2（含进入守卫前的拒跑）；本仓没有调用方解析它的退出码，且任何失败都发布全 INVALID（失败关闭），故不纳入三分契约。同一仓内自管执行脚本（`edgeclaw-*`、`anchor_selfcheck.py` 等）「锚点过期 → 2」的约定同样不在本契约范围。

`_shared.negative_control.mutation_suite` 另有一条启动期规则（#1666/#1734）：它先占证据目录再取环境，
所以 `CARGO_TARGET_DIR` 不可用（未设置 / 空串 / 纯空白走 `negative_selection.missing_target()`；
dot / 根 / `src` 内走 `negative_selection.refuse_unusable_target()`，后者只把
`negative_process.target_environment` 的判据折算成 rc 2，不另列取值）
的拒跑放在占目录之前，并同样走 `negative_selection.refuse()`——rc=2、
原因进 stderr；若启动期 `refuse()` 从更深处（如脚本自己的 oracle 读取）抛出，且**目录仍为空**，
执行器不发布 `summary.json` 并撤掉它；目录已有文件时撤不掉，此时在 stderr 打出残留路径，按 rc=2
（从未开跑）退出、不发布 `summary.json`（目录已被外部删掉则按「没有残留」静默）。三条子分支分别由
`test_negative_adaptive.py` 的 refuse / refuse-residue / refuse-vanished 用例覆盖。这条守卫是整族的
兜底：`background-output/negative.py` 这类直接调用 `mutation_suite` 的入口自身没有前置守卫。其余失败沿用本执行器既有语义，**不套用上面的三分**：
取消（`KeyboardInterrupt` / `scratch` 的 `SystemExit('<string>')`）与恢复核验失败都记 rc=1 并发布
全 INVALID 总结（`test_negative_adaptive.py` 的 interrupt / restore 用例锁定这一行为）。

### 自管执行脚本（#1666 / #1734）

自己管循环、自己判杀的 `*/negative_controls.py` 走另一套四分（0 全 KILLED / 1 跑过但没全绿 /
2 从未开跑 / 42 全杀但基线非 HEAD），规约与不变式见
[`specs/negative-controls/README.md`](../../../../specs/negative-controls/README.md)
「自管执行脚本的判决不变式」。三条硬要求，改这类脚本时逐条核：

- 分母是源码里的静态 `EXPECTED_NAMES`，启动用 `negative_selection.registered` 比对，
  **不取 `len(表)`**；删行 / 改名 / 清空表都必须在第一次变异之前拒跑（rc=2）。
- 收尾用 `negative_selection.verdict_exit`：零行永不返回 0。`0/0 killed` 从来不是通过。
- 全部守卫写成 `raise` / `refuse()`，**一个都不依赖 `assert`**。`python3 -O` /
  `PYTHONOPTIMIZE=1` 会整条剥离 `assert`，靠 `assert` 的负控在优化解释器下静默放行。

`test_negative_optimize_guards.py` 对已迁移的二十八个入口脚本（二十七个夹具目录，
`network-timing` 计 `negative_controls.py` 与 `negative_private.py` 两条）逐一复算三条拒跑矩阵：未知变异名
（加不加 `-O` 各一遍）、`CARGO_TARGET_DIR` 不可用（未设置 / 空串 / 纯空白 / `.` 四档）、输出目录已存在
——都要求 rc=2 且 stderr 点名、且不留下证据目录；它同时自证本解释器确实剥离 `assert`，否则结论
无意义。（四个入口按模式在 `TARGET_OPTIONAL` 显式登记：两个只豁免「未设置」；`cli-parity-mock-gateway`
与 `same-json-surfaces` 自身源码不读取 target（登记表机器断言无任何读取形态），但它们委托的
`mutating_sources` 窗口会经 `_ensure_workspace_scanner` 读取 `CARGO_TARGET_DIR`（未设置取共享默认、
空串/纯空白/dot 在该共享规则处拒绝）且冷主机上可能 cargo 编译扫描器，故「不编译 Cargo」不成立（#2552）；
四档全豁免的实际理由是矩阵暂不可承载：「未设置」会启动一个 300s 矩阵子进程跑不完的完整 guarded 批，
空串/纯空白/dot 在 same-json-surfaces 经 #2431 started 旗标以 rc 2 拒（#2552 实测 rc 2、stderr 点名、
无证据目录——实为可承载，纳入矩阵待登记表支持「自身源码不调 target_environment」的形态，#2552 跟进）、
在 cli-parity-mock-gateway 以启动异常逐行收成 INVALID 证据后退 1（证据目录已领，留档不撤），
均非入口自身的 target 守卫；豁免理由与所依赖的源码事实都有断言，不静默漏测。）
同一文件里的 `PENDING_BARE_ASSERTS` 是**只减不增**的存量登记：仍带裸 `assert` 的
`*/negative_controls.py` 必须逐条列出并写理由，新脚本带裸 `assert` 直接让 CI 红。因此
「已迁移清单」不会被误读成全集——这个**表**只覆盖 `negative_controls.py` 命名的入口；
`negative.py` / `*_negative.py` 等同样式脚本不在表内，属 #1666 batch 3，另行分批推进
（它们当中带裸 `assert` 的不会被这张表挡下）。
`settings-gateway/test_negative_controls.py` 用「真实源码 + 一处注入编辑」复算启动守卫，
不需要 cargo、不需要已提交的树。

## 自测与负控

```sh
export CARGO_TARGET_DIR="$PWD/../../_build/cargo-target"
python3 -B tests/fixtures/pending-reanchor/test_controls.py
python3 -B tests/fixtures/anchor_selfcheck.py --anchors-only
# 先提交；复用调用方的 CARGO_TARGET_DIR，不另建 git 或构建目录。
python3 -B tests/fixtures/pending-reanchor/negative_controls.py NEW_OUTPUT_DIR
```

以上命令在 `src/pilotdeck-gateway` 下运行。四格及边界自测也由 `tests/negative_anchor_selfcheck.rs` 纳入 Cargo 测试。负控只改 Python 共享件，逐项先 `compile()` 验证语法，再执行绑定自测与恢复自测；不运行 cargo，因此不占 cargo slot。锚点失配、HEAD 不一致、语法错误或非绑定断言失败记 `INVALID`，所有选择项汇总后非零退出；把 WARN 降级逻辑改为 no-op 必须杀于 `#1048 downgrade` 断言。

## 共享输入扫描器（#1166 fix-r4）

`mutating_sources` 和 `build_snapshot` 在取得源码树锁并通过基础输入 pristine 检查后，按需以 `--locked` 构建缺失的 `workspace_inputs` example（冷注册表允许按已提交 lockfile 下载依赖，锁文件本身不得变更；CI 全新 runner 依赖这一点，#1166 fix-r6）；先排 cargo 槽位（900 秒），构建总期限 1800 秒、静默期限 600 秒，失败或取消即拒绝进入变异/快照。已有 helper 不自动重建，锁内每次扫描仍核对源码摘要、仓库根和 HEAD；摘要不符须由持有人重建，不能默许陈旧或别树的 scanner。纯 Python 负控的“无 Cargo”只指变异/绑定测试本身，首次共享入口可能构建 scanner。

冷、暖 scanner target 下 `PDRS_NEGATIVE_BASELINE` 逃生口的可用条件（bootstrap 字面 `HEAD` 校验 / 身份 5 文件字节比对、HEAD 移动本身不作废 scanner / 身份失配的 scanner 拒绝所有候选）见 [`_negkit/README.md`](./_negkit/README.md) 的「现状（#2106 实测）」段。

负控脚本仍使用调用方的 `CARGO_TARGET_DIR`，允许树外但须本树独占。共享入口将相对值按仓库根解释并传绝对值给 Cargo/扫描器；普通 `mutating_sources` 调用未设置时，scanner 使用本树 `_build/cargo-target`。测量 `build_snapshot` 仍要求显式 target，并在树锁内校验 HEAD、重新调用 Cargo、从本次 artifact JSON 取 executable 后独占复制；target 位置不是来源证明。共享 target 归一化在副作用前拒收空白串、`.`、文件系统根、仓库根及 `src/` 内目录（显式空值同样拒收）。构建前在 target 原子发布 root / HEAD / 随机 nonce 标记；复制后只复核 owner 标记未变、artifact 身份未变（不比较 mtime：复用早于本次构建的产物正常，Cargo 命中旧缓存产物不因 mtime 拒收），再发布摘要与 build-owner.json。

默认模块扫描对 tracked `src/**/*.rs` 种子同时覆盖 crate 根与普通模块两种所有权目录（含该集合内的自定义 target），递归边使用精确目录。该保守输入闭包可能因非实际上下文中的歧义额外拒绝；它不替代 Cargo 编译有效性检查。`src` 外未由 include/path/mod 连接的自定义根不属于该种子发现规则。

### #1180：并集替换方向（登记的已知限制）与边上下文仲裁

**并集替换（已知限制）**：种子的 `mod x;` 只要在任一所有权目录里恰好命中一个文件就通过。普通模块世界里真实上下文（stem 目录）缺文件、而父目录存在同名文件时，扫描器不报错并把父目录文件收进闭包——rustc 对该树按普通模块编译会 E0583 拒绝（即使父目录同名文件存在），而同一文件形状作为自定义 target 根又是完全合法的 Cargo 工程（自定义 `[[bin]]/[[test]] path` 可指向任意位置）。不做 Cargo target 元数据解析就无法区分两个世界，故登记为已知限制：方向保守（被替换文件仍被守卫，闭包偏大不偏小，不会假 PASS），由 `test_union_substitution_keeps_substituted_module_guarded` 锁定替换文件仍受 pristine 守卫。

**边上下文（仲裁为无缺陷）**：include! 被含文件与 `#[path]` 装载文件的 `mod` 解析目录是**该文件自身的父目录**，与包含方上下文无关——这是 rustc 1.99.1 实测语义（覆盖 crate 根、普通模块、mod.rs、inline `mod` 块、fn 体、嵌套 include!、`#[path]` 名字相同 / 指向子目录 / mod.rs 目标等形状；原始输出见仓根 `_tmp/20261026-issue-1180/rustc_matrix{,2,3,4,5}.log`）。`push_target` 的 `path.parent()` 因此是精确语义而非并集近似；种子双上下文与边单上下文的「不一致」来自 rustc 本身：边的来历确定上下文，种子的来历未知才需要并集。该语义由 `test_included_module_context_is_the_included_file_parent`、`test_path_attribute_module_context_is_the_loaded_file_parent`、`test_inline_module_does_not_rebase_included_edges` 与对应变异（`edge-context-includer-parent` 等）锁定，防止未来按「包含方上下文」方向改动而偏离 rustc（那会漏掉真实输入）。

共享构建目录的产物守卫只对协作写者负责，对抗写者归已知边界（见 [`state/decisions-20261031-1166-threat.md`](../../../../state/decisions-20261031-1166-threat.md)）。
