# gateway 对拍尺子冻结记录（阶段 A · exp006）

> **2026-10-25 重登记（A5.4 冻结前置 · 第 2 包：#85 85.1/85.2 + #52，PR 待填，AI 代批，待人复核）**：#52 把 `common.py:unknown_apparatus_files` 的未登记文件扫描从 `.py/.mjs/.mts` 扩到 `.py/.pyw/.sh/.bash/.zsh/.js/.mjs/.cjs/.jsx/.ts/.mts/.cts/.tsx`（依据：Python 直接运行 .py/.pyw 与 POSIX shell 脚本；Node/tsx 直接加载或经 loader 解析 .js/.mjs/.cjs/.jsx/.ts/.mts/.cts/.tsx），`__pycache__` 外的 `.pyc/.pyo` 一律视为未登记；**fix-r2**：扫描并入 CPython 扩展模块（`importlib.machinery.EXTENSION_SUFFIXES`，本机 `.cpython-314-darwin.so`/`.abi3.so`/`.so`；另含 `.pyd`/`.dylib`）、Node 原生插件 `.node` 与 `.pth`——实测 CPython 在同目录先解析扩展模块再回退 `.py`（放空 `compare.cpython-314-darwin.so` 时 `import compare` 直接对 `.so` 报 dlopen 错，不执行 `compare.py`），Node 扩展名解析顺序为 `.js`→`.json`→`.node`（`.node` 不遮蔽 `.js`，但显式 `require("x.node")` 会 dlopen），`.pth` 在 `-I` 下不被 site 读取；拒绝集合只增不减，未改判分。`apparatus_hashes` 在存在 `node_modules/` 时另记整树字节摘要。**85.2**：`check_frozen.py` 改用同一扫描，遇未登记文件**报错**（旧行为是只跳过），并新增「A2 白名单内且实际存在的脚本必须在 FROZEN 有哈希行」检查；装置入口与 CI 直接调用一律 `python3 -I`，各入口显式把装置目录加入 `sys.path`（隔离模式不自动加脚本目录）；保留的非 -I 仅两处且已逐条核对：`scripts/test-gateway-ledger.py` 的敌意 PYTHON* 环境 UTF-8 用例（`-I` 会忽略这些变量，故该夹具故意不加；装置入口本身的调用已 `-I`）与 `check_schema.py` 的文档示例（非调用点，且该文件受 schema-FROZEN 冻结）；grep 清单见证据。**85.1**：`run_round` / `check_ruler` / `session_domain` 记录并复核 `baselineDepsSha256`——按**实际文件字节**（非 package-lock integrity；integrity 不约束 Node 实际解析到的磁盘字节）哈希基线 `node_modules/tsx`、`node_modules/ws` 及二者 `package.json` 声明的传递依赖（本机集合：tsx、esbuild、@esbuild/darwin-arm64、fsevents、ws）；E1 calibrate 报告同记该字段并在结尾复核。cargo 构建前（`run_round --side rust`、`check_ruler`、新增 `build_proxy.py`）检查并拒绝：本树未跟踪的 `.cargo/config[.toml]`、祖先目录的 `.cargo/config[.toml]`、`$CARGO_HOME/config[.toml]`，命中打印 `BLOCKED` 并列路径。**边界（威胁模型同 `specs/ui-server-parity/acceptance.md`：防意外漂移与仓库内改动，不防本机恶意者）**：① 基线依赖只覆盖 tsx/ws 及其声明的传递依赖，不覆盖基线 dist 动态 require 的其它包，也不校验基线 package-lock；嵌套符号链接只记链接目标字符串、不追随其字节；② 装置 `node_modules/` 整树摘要仅在存在时记录，未逐包登记（跨平台安装字节不同），不校验 npm integrity；③ cargo 不校验 `$CARGO_HOME/registry/src` 或 `.crate` 缓存字节，不校验 `RUSTFLAGS`/`CARGO_ENCODED_RUSTFLAGS` 等环境，也不证明一次绕过 `build_proxy.py` 的 `cargo build` 如何被调用；④ 未登记扫描按脚本/字节码/扩展模块后缀与 `__pycache__` 规则（CPython 扩展模块由 `importlib.machinery.EXTENSION_SUFFIXES` 判定，另拒 `.pyd`/`.dylib`/`.node`/`.pth`）；无后缀但带 shebang 的可执行文件不在覆盖内（不会被装置自动加载）。未改阈值/分母/判分语义，未改 `accepted-diffs.md`、`specs/parity/`、`scripts/ui-server-parity/`。真实数据复算：E1 calibrate `status=PASS` / 72 例 / 360 次 / frozen `exit 0` / `e1Controls PASS`（`_tmp/20261025-a54-pkg2/calibrate/report.json`），controls `--source` 全过（`_tmp/20261025-a54-pkg2/controls-source/results.json`），`check_frozen.py --base origin/main` PASS（`_tmp/20261025-a54-pkg2/final-checks.log`）；负控（未登记 .js/.cjs/.ts/.cts/.sh、影子模块、根 `.pyc`、登记文件改一字节、装置 node_modules 换字节、基线 tsx 换字节、未跟踪/持久 cargo config）见 `_tmp/20261025-a54-pkg2/negative/`，逐项「有检查 FAIL / 去掉检查 PASS（旧实现或只删该检查的副本）」并逐字节还原。本次更新 10 个既有文件哈希 + 新增 `build_proxy.py` 一行；fix-r2 再重登记 `common.py` 与 `test_safety.py` 两个哈希（拒绝集合加严 + 对应单测），其余行不变。**A2 真实数据复算（fix-r2 补登记）**：协调者按 #167 卡 1 用 A1 纯透传二进制（896b37f，sha256 `7ee87f0f1f9a394a1276f7f18cb8335aca9872f53450950d8613258554d4ebc3`）执行 `check_ruler.py --output _tmp/20261025-a54-pkg2-ruler-r3`，退出 2（机器项全过、待独立复核）：`k=5`、`self_compare`、`distinct_ts_rounds`、`positive_controls`、`prerequisite_controls` 均为 true，5 个不同 gateway 进程（`gateway_pids` 24620/25184/25608/26140/26287，`status=PENDING_REVIEW`）；原件在主仓 `_tmp/20261025-a54-pkg2-ruler/r3/`，本树只读副本在 `_tmp/20261025-a54-pkg2-ruler-r3/`。此前一次用 A4 本地化二进制的 FAIL 属复算方法错误（被测物与 A2 全透传判据不匹配），不是装置回归——fix-r1 已用 origin/main 在同一份数据上复现同一 FAIL（`_tmp/20261025-a54-pkg2/fix-r1/RESULT.md` 与其 `logs/`）。因 `common.py`/`test_safety.py` 字节已变（装置哈希随变），A2 复算需协调者在本包合入后重跑；本包未自行重跑 `check_ruler`。两家非作者评审由协调者续办。

> **2026-10-25 重登记（A5.4 冻结前置 · #170 / #165，含 fix-r1 更正，PR 待填，AI 代批，待人复核）**：#170 补 Node 侧直接 oracle 对预注册标签的核对——`scripts/a5-19/observation_rules.mjs` 新增 `checkObservationLabels`，由 `checkObservation` 在按 id 取观察时按 `<cell>/<variant>/<phase>/<repetition>` 逐项核对 `repetition` / `phase` / `variant.name`，不一致以 `COVERAGE` 拒绝；与 `session_domain.py:validate_calibration`（205-210 行）的三项同名检查在 `phase`、`variant.name` 与整数 `repetition` 上同口径（Python 仍额外以 `type(...) is int` 强制 repetition 为整数，并强制 72 例 × 5 次完整 ID 集合与 cell/registry 归属；Node 只拿 id 与标签，不能替代该分母检查）。**已知 Node 结构性不可检（fix-r1 更正）**：JSON `"repetition": 1.0` 经 `JSON.parse` 后就是数字 1，与整数无法区分，故 Node 侧无法拒绝该 float 情形；该情形由 Python `validate_calibration` 的 `type(...) is int` 兜底——E1 calibrate 在调用 Node 桥（226 行）之前先跑该 Python 检查，`controls/session_domain.py` 的 `calibration-repetition` 也以 float 变体逐次回放；只有不经 Python、单独直跑 `node scripts/gateway-parity/session_observations.mjs` 的手写 JSON 路径会接受 1.0，不构成现网闸口假 PASS。fix-r1 起 `controls/session_domain.py` 的直接路径标签用例扩为 13 条：12 条负控（repetition 的 zero / wrong-round / out-of-range / boolean / string / null，phase，variant 的 wrong-name / null / array，五段 id，以及 id 的 repetition 段非规范正整数），每条都在同一输入上验证「保留核对即拒绝、仅去掉对应子句即接受」；float 另立 1 条「已知接受」钉住（`observation-repetition-float-known-accept`，若 Node 将来能拒绝它即转红并提示更新登记），不伪装成负控；`run_all` 仅对该钉住按期望接受判定，其余负控仍要求 exit 1 / FAIL，未放宽任何既有检查。未改任何判分/阈值/分母。**只变严，不改判分语义**。#165 把 `scripts/gateway-parity/fixtures/session-calibration.json` 与 `session_domain.py:FIXTURE_NAMES` 的全部 `scripts/a5-19/*`（run.py、six_cells.mts、observation_rules.mjs、append-observer.mjs、cleanup-observer.mjs）纳入白名单；`check_frozen.py` 无需改动即可逐字节校验（负控：改一字节即 FAIL，恢复即 PASS）。真实数据复算：E1 calibrate status PASS / 72 例 / 360 次 / frozen exit 0 与 controls 全过见 `_tmp/20261025-a54-pkg1/fix-r1/calibrate/report.json`、`_tmp/20261025-a54-pkg1/fix-r1/controls-source/results.json`；12 条标签负控的逐子句移除对照与 float 钉住（Node 接受 / Python 拒绝）对照见 `_tmp/20261025-a54-pkg1/fix-r1/negative-labels/`，FROZEN 负控见 `_tmp/20261025-a54-pkg1/fix-r1/frozen-negative.log`。本次更新 `scripts/a5-19/observation_rules.mjs` 与 `scripts/gateway-parity/controls/session_domain.py` 两个哈希（前者含注释更正，核对逻辑未改）；其余行不变。两家非作者评审由协调者续办。

> **2026-10-23 修复轮 2 重登记（#581 / PR #582，人批范围内）**：merge-base 等于 HEAD 且 base 解析提交不同于 HEAD 时拒绝无效的只追加对照；base 本身等于 HEAD 时保留工作树字节核验并注明（如空推送）。显式使用 UTF-8 读取文本及解码 Git 输出；仅更新 check_frozen.py 哈希。真实账本复算与回归、负控证据见 `_tmp/20261023-issue-581-ledger-split/RESULT.md`「修复轮 2」。

> **2026-10-23 修复轮 1 重登记（#581 / PR #582，人批范围内）**：修补 gate1 boundary P1；--base 对照 merge-base 同时强制历史 accepted-diffs.md 逐字节一致（失败报告首个差异行）及其 FROZEN 哈希行原字节不变，既有 diffs/ 只追加保护继续生效。仅重登记 check_frozen.py 哈希，不改历史账本及其哈希行；真实 origin/main 账本复算、正负控原始输出见 `_tmp/20261023-issue-581-ledger-split/RESULT.md`「修复轮 1」。独立评审由协调者接续。

> **2026-10-23 重登记（#581，人批账本拆文件规则）**：历史 accepted-diffs.md 原字节冻结、此后不再追加（其哈希行不变）；新条目与更正只新增 diffs/<ID>.md / <ID>-R<n>.md，六字段与 original / 决策文件存在性静态校验，--base 对照 merge-base 强制已有 diffs/ 文件逐字节不变。新增条目不改任何已有文件、不维护清单、不改 FROZEN。本次只重登记 check_frozen.py 哈希；模板与回滚见 diffs/README.md，真实复算由 #581 交付证据与两家非作者评审接续。

> **2026-10-23 重登记（#553 pr1 PR）**：accepted-diffs.md 末尾只追加 A6.5-BACKGROUND-PERSIST-001「当前状态：输出 spill 与读回部分已实现，快照与启动恢复仍未对齐」；不改其他条目、不改判分语义；只重登记 accepted-diffs.md 哈希。

> **2026-10-23 重登记（#552 PR r2）**：accepted-diffs.md 末尾只追加 A6.5-BASH-OS-001 当前状态更正二（glob 不 spawn ripgrep、MCP stdio 不设进程组），原文保留；不改判分语义；只重登记 accepted-diffs.md 哈希。

> **2026-10-23 重登记（#552 PR 修复轮 1）**：accepted-diffs.md 末尾只追加 A6.5-BASH-OS-001 当前状态更正（grep / command hook 同为 detached 已改，sidecar / MCP stdio 不是），原文保留；不改判分语义；只重登记 accepted-diffs.md 哈希。

> **2026-10-23 重登记（#552 PR）**：accepted-diffs.md 末尾只追加 A6.5-BASH-OS-001「当前状态：setsid 部分已消除」及机器判据，原条目保留；不改判分语义、不新增豁免；只重登记 accepted-diffs.md 哈希。

> **2026-10-23 重登记（#563 PR 修复轮 1）**：accepted-diffs.md 末尾只追加 A6.6-EDGECLAW-ENTRY-PATH-CONTAIN-001 范围补充更正二（写 / 删时 TS 的 MEMORY.md 落盘、逐个测试名），原文保留；不改判分语义；只重登记 accepted-diffs.md 哈希。

> **2026-10-23 重登记（#563 PR）**：accepted-diffs.md 末尾只追加 A6.6-EDGECLAW-ENTRY-PATH-CONTAIN-001 范围补充的更正（TS 读返回空、删为无操作）与机器判据；不改其他条目、不改判分语义；只重登记 accepted-diffs.md 哈希。

> **2026-10-23 重登记（#551 PR）**：accepted-diffs.md 末尾只追加 A6.5-MCP-HTTP-LIFETIME-001「当前状态：代理部分已对 MCP 消除」与三条已知未对齐（未批准）：A6.5-MCP-PROXY-HTTPS-001、A6.5-MCP-PROXY-SOCKS-001、A6.5-MCP-PROXY-FALLBACK-001；不改其他条目、不改判分语义；只重登记 accepted-diffs.md 哈希。

> **2026-10-23 重登记（PR #531 修复轮 3，判据补充）**：accepted-diffs.md 末尾追加 A6.6-EDGECLAW-IMPORT-SWAP-ROLLBACK-001 的机器判据补充（新增 cfg(unix) 单测覆盖单线程 copy 回退失败，原条目「尚无 Rust 测试」一句过期），不改原条目、不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希。

> **2026-10-23 重登记（PR #531，透明披露）**：accepted-diffs.md 末尾新增「已知未对齐（未批准，待人批）· A6.6 EdgeClaw export / import」一节，登记 A6.6-EDGECLAW-IMPORT-SWAP-ROLLBACK-001（import 换目录失败后一律恢复备份，向安全偏离；含并发与单线程 copy 回退失败两种情形，按 codex r2 实测更正范围），状态未批准，不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希，其余不变。两家评审见 PR。

> **2026-10-23 重登记（#554 PR 修复轮 3–4）**：accepted-diffs.md 末尾只追加 A7-HOST-POST-CANCEL-EXIT-001-R1（收敛说明与判据更正），首版保留；不改判分语义；只重登记 accepted-diffs.md 哈希。

> **2026-10-23 重登记（#554 PR 修复轮 2）**：accepted-diffs.md 末尾只追加 A7-HOST-POST-CANCEL-EXIT-001（取消后 5 s 有界退出，向安全偏离，AI 代批待人复核）与前一条状态追加的类型名更正；不改其他条目、不改判分语义；只重登记 accepted-diffs.md 哈希。

> **2026-10-23 重登记（#554 PR）**：accepted-diffs.md 末尾只追加 A7-HOST-RESOURCE-001「当前状态：本地 watchdog 部分已消除」及机器判据，原条目保留；不改判分语义、不新增豁免；只重登记 accepted-diffs.md 哈希。

> **2026-10-23 重登记（#558 PR）**：accepted-diffs.md 末尾只追加 A7.2-SUBMIT-FAILURE-GUARD-001「当前状态：原能力拒绝已消除」及机器判据，原条目保留；不改判分语义、不新增豁免；只重登记 accepted-diffs.md 哈希。

> **2026-10-23 重登记（#361-3 PR r3）**：accepted-diffs.md 末尾只追加 A6.5-MCP-HTTPS-001 状态更正二（金样哈希与场景数）与 A6.5-MCP-CONNECT-HOSTNAME-001（主机名 URL 的拒连详情地址，未批准）；不改其他条目、不改判分语义；只重登记 accepted-diffs.md 哈希。

> **2026-10-23 重登记（#361-3 PR 修复轮 1）**：accepted-diffs.md 末尾只追加 A6.5-MCP-HTTPS-001 状态更正（判据措辞与批准记录指针）及三条已知未对齐（未批准）：A6.5-MCP-TLS-TRUST-001、A6.5-MCP-SCHEME-001、A6.5-MCP-TLS-CAUSE-001；不改其他条目、不改判分语义；只重登记 accepted-diffs.md 哈希。

> **2026-10-23 重登记（#361-3 PR）**：accepted-diffs.md 末尾只追加 A6.5-MCP-HTTPS-001「当前状态：已实现」及机器判据，原条目保留；不改判分语义、不新增豁免；只重登记 accepted-diffs.md 哈希。

> **2026-10-23 重登记（PR #544 gate6）**：accepted-diffs.md 末尾只追加 A6.3-PLUGIN-MARKDOWN-CYCLE-001-R4（机器判据更正：1 s 墙钟改为 20 s 防挂起 + 确定性展开计数），原文保留；不改判分语义；只重登记 accepted-diffs.md 哈希。

> **2026-10-23 重登记（PR #544 修复轮 4）**：accepted-diffs.md 末尾只追加 A6.3-PLUGIN-MARKDOWN-ROOT-001-R1（根外不可读 Markdown 的错误路径范围与判据），首版保留；不改判分语义、不新增豁免；只重登记 accepted-diffs.md 哈希。

> **2026-10-23 重登记（PR #544 修复轮 3）**：accepted-diffs.md 末尾只追加 A6.3-PLUGIN-MARKDOWN-CYCLE-001-R3（去掉 Markdown 候选预算与 64 层深度上限、R2 ② 的差异消除），首版、R1、R2 原文保留；不改判分语义、不新增豁免；只重登记 accepted-diffs.md 哈希。

> **2026-10-23 重登记（#484 PR #544 修复轮 2，更正）**：accepted-diffs.md 末尾追加 A6.3-PLUGIN-MARKDOWN-CYCLE-001-R2（预算只计 .md 候选、超限诊断、更正 R1 对 TS 环行为的描述、补指数别名 DAG），首版与 R1 原文保留；AI 代批待人复核、未批准，不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希。两家评审见 PR。

> **2026-10-23 重登记（#484 PR #544 修复轮 1，更正与新增）**：保留 A6.3-PLUGIN-MARKDOWN-CYCLE-001 首版原文、追加更正 A6.3-PLUGIN-MARKDOWN-CYCLE-001-R1（首版只按祖先防环、引用已移走的测试、误称别名与 TS 一致，按 Grok r1 与真实 TS 实测更正为全局去重 + 总预算并列出三处可观察差异），新增 A6.3-PLUGIN-MARKDOWN-ROOT-001（不经符号链接读插件根外文件）；均向安全偏离、AI 代批待人复核、未批准，不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希，其余不变。两家评审见 PR。

> **2026-10-23 重登记（#484 PR，透明披露）**：accepted-diffs.md 末尾新增「已知未对齐（未批准，待人批）· A6.3 插件 loader」一节，登记 A6.3-PLUGIN-MARKDOWN-CYCLE-001（markdown 遍历跳过符号链接祖先回边，向安全偏离，AI 代批待人复核），状态未批准，不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希，其余不变。两家评审见 PR。

> **2026-10-23 重登记（PR #537 r6）**：accepted-diffs.md 末尾只追加 A6.6-EDGECLAW-SNAPSHOT-CPSYNC-ERROR-001（快照 cpSync 回退失败时错误形状，已知未对齐、未批准）；不改其他条目、不改判分语义；只重登记 accepted-diffs.md 哈希。

> **2026-10-23 重登记（PR #537 修复轮 6）**：accepted-diffs.md 末尾只追加 A6.6-EDGECLAW-ENTRY-PATH-CONTAIN-001 的范围补充（根内悬挂符号链接一律拒绝），原条目保留；不改判分语义、不新增豁免；只重登记 accepted-diffs.md 哈希。

> **2026-10-23 重登记（PR #537 修复轮 5）**：accepted-diffs.md 末尾只追加 A6.6-EDGECLAW-ENTRY-IDS-SHAPE-001（四个按 ids 批量入口的参数形状接口边界，已知未对齐、未批准）；不改其他条目、不改判分语义；只重登记 accepted-diffs.md 哈希。

> **2026-10-23 重登记（PR #537，透明披露）**：accepted-diffs.md 末尾新增「已知未对齐（未批准，待人批）· A6.6 EdgeClaw memory entries」一节，登记 A6.6-EDGECLAW-ENTRY-PATH-CONTAIN-001（entry id 不得越出 store 根，向安全偏离，AI 代批待人复核），状态未批准，不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希，其余不变。两家评审见 PR。

> **2026-10-23 重登记（A7.2-pr5 PR，状态更新）**：accepted-diffs.md 末尾追加「A7.2 turn replay 条目状态更新」：A7.2-REPLAY-RETENTION-TIMER-001 在生产 TurnGateway 宿主内已消除（追加说明，不改原条目；裸 TurnReplayStore 仍保持原行为）。不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希，其余不变。两家评审见 PR。

> **2026-10-23 重登记（#508 PR）**：accepted-diffs.md 末尾只追加 A6.5-MCP-IMAGE-READ-SANDBOX-001（MCP 工具结果自动读图向安全偏离，人批 2026-10-23 采纳建议 A）；不改其他条目、不改判分语义；只重登记 accepted-diffs.md 哈希。

> **2026-10-23 重登记（PR #536 修复轮 5）**：accepted-diffs.md 末尾只追加 A7.2-READ-LASTSEQ-DEPTH-001 判据更正四（更正三的范围句不成立：文件 registry 路径同样可观察；两条有界负控已进仓），原文保留；不改判分语义、不新增豁免；只重登记 accepted-diffs.md 哈希。

> **2026-10-23 重登记（PR #536，透明披露）**：accepted-diffs.md 末尾新增「已知未对齐（未批准，待人批）· A7.2 turn 读面」一节，登记 A7.2-READ-LASTSEQ-DEPTH-001（不复刻 V8 栈溢出临界层数），按决策卡 card-v8-stack-depth 登记，状态未批准，不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希，其余不变。两家评审见 PR。

> **2026-10-23 重登记（board PR #550，人批记录）**：accepted-diffs.md 末尾只追加「人批记录 · #505（#502）补登的 32 条」一节，记录人对 32 条的批复（1 条关闭、5 条要求修、26 条接受为已知差异，接受不豁免判分）；不改任何条目原文、不改判分语义；只重登记 accepted-diffs.md 哈希。

> **2026-10-23 重登记（PR #539，透明披露）**：accepted-diffs.md 末尾追加「A6-JSVALUE-DEPTH-001 追加说明」：fingerprint 合一后 runtime 路径保持 2000 门限、host 路径保持无门限显式栈，两路径相对 TS 的剩余差异写明；不改原条目、不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希。两家评审见 PR。

> **2026-10-23 重登记（PR #535 修复轮 2，更正）**：accepted-diffs.md 末尾追加「A7.4 user_dialog HTTP store 条目更正」：MAX-CONNECTIONS、431-408-SHAPE、KEEPALIVE、CLIENT-ERROR-CLASS、CLIENT-BODY-UNITS、AUTH-BEFORE-BODY 六条各追加 -R1（原条目保留），均未批准，不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希。两家评审见 PR。

> **2026-10-23 重登记（PR #535，透明披露）**：accepted-diffs.md 末尾新增「已知未对齐（未批准，待人批）· A7.4 user_dialog HTTP store」一节，共九条（鉴权先于读 body、请求体 2 MiB、两段超时、未实现 keep-alive 5 s、连接上限 64、431/408 形状、键序、客户端错误类名、客户端上限计量单位），全部未批准，不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希，其余不变。两家评审见 PR。

> **2026-10-23 重登记（p2-hooks PR，透明披露）**：accepted-diffs.md 末尾追加「A6.7 hooks 执行器状态更新与新增条目」：A6.7-HOOK-HTTPS-001 当前状态为已实现（追加说明，不改原条目）；新增 A6.7-HOOK-OUTPUT-001（command hook 输出封顶）、A6.7-HOOK-TLS-TRUST-001（信任源与代理），均未批准，不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希，其余不变。两家评审见 PR。

> **2026-10-23 重登记（PR #533，透明披露）**：accepted-diffs.md 末尾新增「已知未对齐（未批准，待人批）· A7.2 submit 编排」一节，登记 A7.2-SUBMIT-FAILURE-GUARD-001（FailureGuard 运行时未移植，启用时显式拒绝，推迟到 A7.2-pr4b），状态未批准，不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希，其余不变。两家评审见 PR。

> **2026-10-23 PR #513 修复轮 5 重登记（透明披露）**：按 GLM r3 P2 #1（实现方在修复轮 5 中补登，协调者复核后保留），accepted-diffs.md 只追加 D2-CHATSEARCH-REGEX-GIANTQUANT-001（巨量词 V8 饱和语义与 fancy-regex 编译边界），两次真实 TS 重采及 Rust 分歧复算见 RESULT「修复轮 5」。状态为已知未对齐、未批准，不新增豁免、不改变冻结判分语义；仅更新 accepted-diffs.md 哈希，独立评审由协调者续办。

> **2026-10-23 重登记（PR #513，透明披露）**：accepted-diffs.md 末尾新增「已知未对齐（未批准，待人批）· D2 CLI chat search」一节，登记 D2-CHATSEARCH-REGEX-BACKTRACK-001（TS 正则灾难性回溯长时间不返回，Rust 回溯上限有界失败）。状态为未批准，不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希，其余不变。两家评审见 PR。

> **2026-10-23 重登记（#518 PR，透明披露）**：accepted-diffs.md 末尾新增 A7.5-REPLACEMENT-ACTIVE-GETTER-PANIC-001（超时回滚时 hasActiveTurn 端口 panic，Rust 告警后重新约时，TS 拒绝），状态未批准，不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希，其余不变。两家评审见 PR。

> **2026-10-23 重登记（#516 PR，条目格式更正）**：accepted-diffs.md 末尾只追加「A7.3 interaction 边界条目更正（#516）」小节，内含 A7.3-INTERACTION-BOUNDARY-001-R1（把原条目按标准六字段重述，原条目逐字保留）；仍为边界登记、非豁免，不改判分语义；只重登记 accepted-diffs.md 哈希，其余不变。两家评审见 PR。

> **2026-10-23 A7.2-pr1 修复轮 8 重登记（PR 待开，sink 所有权条目承诺范围更正）**：按本单 codex r8 两条 P1 的要求（评审原件 `_tmp/20261022-a72-pr1-replay/rv-codex-r8.md`），对 accepted-diffs.md 的 A7.2-COORDINATOR-SINK-OWNERSHIP-001 做**只追加更正**（原条目文字未删改，范围以追加块为准）：① 轮 7「最后一个注册 drop 后 emit 拒收」并未成立——emit 以 sink 强引用存活代理注册存活，在途交付与队列均持强 sink；轮 8 起注册存活标记与交付用强 sink 分离（map 弱持注册记录、队列只持内层 sink）；② 「泄漏已从设计上消除」收窄为仅 coordinator 侧环——宿主侧 `Host → registration → sink → Host` 环任何库 API 无法阻止，本轮删除代码注释与轮 7 登记注中的过度承诺，改为文档化所有权约定（sink 回调宿主须弱捕获；会话结束显式 drop 注册或调用新增显式注销 `unregister`），宿主侧残余环按条目登记为已知残余。新增五条钉住测试（断言消息含编号），negative.py 各配专属变异（实测转红）。只重登记 accepted-diffs.md 哈希，其余文件哈希不变。

> **2026-10-23 A7.2-pr1 修复轮 7 重登记（PR 待开，sink 所有权模型差异补登）**：按本单 gate7 P1 #1 的要求（评审原件 `_tmp/20261022-a72-pr1-replay-gate7/`），对 accepted-diffs.md 的 A7.2 节做只追加变更：新增 A7.2-COORDINATOR-SINK-OWNERSHIP-001（sink 所有权模型：coordinator 只持 `Weak` sink、`start` 返回强所有者 `TurnSinkRegistration`，最后一个注册 drop 后 emit 拒收——TS 的 sink 存 `Map` 由 GC 管生命周期；类别实现选择、未批准，引用循环泄漏本身已按 gate 优先项从设计上消除、不登记）。coordinator.rs 模块文档加账本指针，新增两条钉住测试（断言消息含编号），negative.py 各配专属变异（实测转红）；`with_run_id` 非对象事件改为照 TS `{ ...event, runId }` 展开（真实 TS 金样新增 `with-run-id-non-object-spread`，oracle.json 重生成两次逐字节一致，含 number/string/array/null/boolean 五类与缺会话负控）。只重登记 accepted-diffs.md 哈希，其余文件哈希不变。（轮 8 注：本条中「已从设计上消除」的表述已由轮 8 重登记注收窄，原文保留。）

> **2026-10-22 A7.2-pr1 修复轮 6 重登记（PR 待开，交付语义差异补登 + 状态行统一）**：按本单 gate6 sibling P1 与 Grok r6 P1 的要求（评审原件 `_tmp/20261022-a72-pr1-replay-gate6/sibling.md`、`_tmp/20261022-a72-pr1-replay/rv-grok-r6.md`），对 accepted-diffs.md 的 A7.2 节做只追加变更：① 在 A7.2-REPLAY-RETENTION-TIMER-001 条目末尾追加「当前状态（以本行为准）」一行（已知未对齐、人批 2026-10-22、前置于 A7.2-pr5、非已接受差异），消除同条目内「未批准/待人定」与「人批」的表述冲突，原有文字未删改；② 新增 A7.2-COORDINATOR-DELIVERY-001（多线程宿主下的事件交付边缘与 TS 单线程同步交付不一致：(a) `retain_terminal`/替换性 `start` 返回后旧 sink 的在途交付、(b) sink panic/`dispose` 静默丢弃已接受未交付事件、(c) 重入 `emit` 的 panic 归因、(d) 无界交付队列无背压；类别实现选择、未批准、前置于 A7.2-pr5），补登依据 gate6 sibling P1——此前只在 `coordinator.rs` 模块文档自我披露、账本无条目；③ coordinator.rs 模块文档加账本指针，并新增四条钉住测试（断言消息含编号），negative.py 各配一条专属变异（实测转红）。只重登记 accepted-diffs.md 哈希，其余文件哈希不变。

> **2026-10-22 人批重登记（PR #487）**：A7.2-REPLAY-RETENTION-TIMER-001 人批为已知未对齐，前置于 A7.2-pr5 对齐（采纳决策卡建议 B）；只重登记 accepted-diffs.md 哈希。

> **2026-10-22 A7.2-pr1 修复轮 5 重登记（PR 待开，范围与证据更正）**：按本单 codex r5 P1 #2/#3 与 Grok r5 P1 的要求（评审原件 `_tmp/20261022-a72-pr1-replay/rv-codex-r5.md`、`rv-grok-r5.md`），对 accepted-diffs.md 的 A7.2 节做**只追加更正**：在原 A7.2-ZERO-RETENTION-TIMER-001 条目下追加「更正」一行（该条范围写错——差异不限于 `terminalRetentionMs=0`，生产 `ThreadTimer` 下所有保留时长的到期回收都不等调用方当前同步段与微任务；「不可避免」归类依据不足，`with_timer` 已提供宿主调度注入口），再新增 A7.2-REPLAY-RETENTION-TIMER-001（类别：实现选择，待人定——接受，或在 A7.2-pr5 接宿主串行计时器后对齐；基线证据换为真实 timer 探针：不替换 `setTimeout`，retention 0/5/30000 的同步段与 Promise 微任务内均可见、下一个 timer 回合才回收，`_tmp/20261022-a72-pr1-replay/probe-retention-real-timer.mts`，两次运行一致）。原条目文字未删改。同步更正代码注释与 fixture README 的范围表述，Rust 测试宣称收窄为可证部分（生产忙转测试只断言最终回收、不对「忙转期间已回收」做宣称；边界改由注入受控计时器 + 回调执行标记钉住，覆盖 0/5/30000）；`ThreadTimer` 回收行为未改。只重登记 accepted-diffs.md 哈希，其余文件哈希不变。另：本分支已 rebase 到 main，A6.4 stats 节与 A7.2 节已在同一文件并存，修复轮 4 注中「合并时须 reconcile」的提示已完成使命（Grok r5 P3）。两家评审见 PR；人批另行。

> **2026-10-22 A7.2-pr1 修复轮 4 重登记（PR 待开，透明披露补齐）**：按本单 codex r4 P1 #2 与 gate4 sibling P1 的要求（评审原件 `_tmp/20261022-a72-pr1-replay/rv-codex-r4.md`、`_tmp/20261022-a72-pr1-replay-gate4/sibling.md`，两评审均指出 A7.2-ZERO-RETENTION-TIMER-001 差异此前只在代码注释与 fixtures README 自我披露、账本无条目），在 accepted-diffs.md 末尾新增「已知未对齐（未接受，待人批）· A7.2 turn replay」一节：A7.2-ZERO-RETENTION-TIMER-001（零保留终态的生产回收时机：真实 TS 的 `setTimeout(0)` 依赖宿主事件循环，同步忙转 20 ms 仍可见；Rust 生产 `ThreadTimer` 取 Node 自身 1 ms 下限、到期即回收）。状态为「未批准，非已接受差异」，不新增豁免、不改判分语义、不改门禁；登记流程沿用 PR #476 对 gate3 sibling P1 的同类补登（只追加待批条目，check_frozen 通过，人批前不算已接受）。只重登记 accepted-diffs.md 哈希，其余文件哈希不变。两家评审见 PR；人批另行。注：本分支基线早于 PR #476 的 A6.4 stats 节，合并时须与其追加节 reconcile 后一次性重登记哈希。

> **2026-10-23 重登记（PR #510，边界登记）**：accepted-diffs.md 末尾追加 A7.3-INTERACTION-BOUNDARY-001（pending 输入类型域与线程映射），类别为边界登记、非豁免，不新增已接受差异、不改判分语义；只重登记 accepted-diffs.md 哈希，其余不变。两家评审见 PR。

> **2026-10-22 人批重登记（PR #476）**：A6.4-STATS-RETENTION-001、A6.4-STATS-BASELINEMODEL-001 两条接受为已知差异（用户本人在协调者会话批准），并要求 Rust 补实现（见 accepted-diffs.md 该节末尾的跟进 issue）；只重登记 accepted-diffs.md 哈希。

> **2026-10-22 修复轮 3 重登记（PR #476，透明披露补齐）**：按本单 gate3 sibling P1 的要求（`_tmp/20261022-a64-pr9b-gate3/sibling.md`，工作指令「gate3 的 P0/P1 一并修」），在 accepted-diffs.md 末尾新增「已知未对齐（未接受，待人批）· A6.4 stats」一节：A6.4-STATS-RETENTION-001（retention 只解析 `toISOString` 规范形，真实 TS 删除 8 类非规范行而 Rust 保留）与 A6.4-STATS-BASELINEMODEL-001（未知 `stats.baselineModel` ref 的 fatal 诊断未实现）。两条状态均为「未批准，非已接受差异」，不新增豁免、不改判分语义、不改门禁；真实 TS 探针（固定时钟与 retention）与 Rust 侧机器判据见该节。只重登记 accepted-diffs.md 哈希，其余文件哈希不变。两家评审见 PR（gate4）；人批另行。

> **2026-10-21 重登记（PR #391，人批）**：accepted-diffs.md 追加 A6.5-MCP-OUTSCHEMA-REGEX-001（正则长串预算，人批 13:10）与 A6.5-MCP-OUTSCHEMA-DEPTH-001（深度 / 宽度预算 2048，人批 14:25），见 state/decisions-20261021.md；其余文件哈希不变。两家评审见 PR。

> **2026-10-21 重登记（PR #406，人批）**：accepted-diffs.md 追加 A6.5-WEBFETCH-DEPTH-001（web_fetch HTML→Markdown 深度上限 256，向严，人批 2026-10-21 15:25，state/decisions-20261021.md）；其余文件哈希不变。两家评审见 PR。

> **2026-10-21 修复轮 5 重登记（PR #406，表述准确性修正）**：依据本 PR 引入 A6.5-WEBFETCH-DEPTH-001 的人批（2026-10-21 15:25），将「深度 ≤ 256 两侧逐字节一致」收窄为除 pr9c 两条解析器版本差异外，并在 accepted-diffs.md 末尾透明披露真实 TS / Rust 输出与测试名。两条均「未批准，非已接受差异」，不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希，其余不变。两家评审见 PR。

> **2026-10-17 四登（#187，只改路径解析、不改判分语义）**：run_round.py / session_domain.py 的默认 baseline 由主仓绝对路径改为「显式参数 > PDRS_BASELINE > 本 checkout 的 resources/PilotDeck」，缺失时两者都 BLOCKED（exit 1 / 2）并提示、不建输出目录（session_domain 按 six_cells.mts 实际导入的 10 个 E1 源码入口核对，不要求 dist）；driver.mts 去掉主仓回落，缺 PDRS_BASELINE 即报错（run_round 总会设置）。不再在 worktree 里静默用主仓 baseline。真实数据复算：同树两轮 TS 自比 PASS（`_tmp/20261017-issue-187/r2/compare-ts-a-b.json`，含 driver.mts 改动）；无 baseline 负控两入口均 BLOCKED（`r2/negative.log`：run_round exit 1、session_domain exit 2 且不建输出目录）；E1 calibrate 正向：status PASS、72 例 360 次测量、frozen exit 0、e1Controls PASS（`r3/calibrate/report.json`）；E1 前检正/负控（`r3/precheck-probe.log`、`r3/negative.log`）。两家评审见 PR。**AI 代批，待人复核**。

> **2026-10-17 重登记（两类，均见 PR）**：① accepted-diffs.md 追加 A4-ARCHIVE-SEC-001/002/003（#142，人批 2026-10-17「批准 #134 的登记范围」，state/decisions-20261017.md）；② A5.3 新增 E1 适配脚本三件（session_domain.py / session_observations.mjs / controls/session_domain.py）入白名单并登记哈希（plan 005：A5 PR 带理由+双评审登记新 hash）。

> **2026-10-17 三登（#167，P1 缺口修复）**：validate_calibration 新增 repetition/phase/variant 与预注册 ID 一致性核对及空账本拒绝（audit 原件见 PR）；负控 24 条含新两条。计划 005 程序：理由+双评审+实证复算。

> **2026-10-15 重登记（issue #68，人批）**：accepted-diffs.md 新增 A4-FILE-TOCTOU-001（project_file_read 检查后符号链接替换，原版安全缺陷）；其余文件哈希不变。两家评审见 PR。

> **2026-10-14 重登记（A4 前置 #45，PR #50）**：时钟归一收紧到容器组合、装置哈希改 12 件显式白名单（未登记脚本即 FAIL）、具名键前置写入 normalization；两家（GLM、Claude 替补 DeepSeek）两轮均无 P0/P1；rebase 后正式重跑 `check_ruler` 机器项全 true。**AI 代批，待人复核**。
> 2026-10-14 冻结。**AI 代批，待人复核**（依据：`check_ruler.py` 机器项全 true——TS 自比 k=5 十对、五个不同 gateway 进程、阳性对照 33 项主规则命中、两侧同坏拒绝、代理一致；双谱系评审第 3 轮 GLM、DeepSeek 均 `independent_review: true`，无 P0/P1，P2 转跟进 issue）。
> 冻结后改任何一份都要另开 issue、两家评审同意、写明理由（`CLAUDE.md` §五「尺子不许自己改」）。A3 的 schema 产物与脚本另见 `schema-FROZEN.md`，不在本表。
> 校验：`python3 -I scripts/gateway-parity/check_frozen.py`（下面每行「sha256 两空格 相对本目录的路径」）。

886d12cdd36a5ecac53d314fabd52fed9ecd093b2a07939bbcf553d88b37e519  acceptance.md
122eb3f8f75300144f76445bb7f9576ae06c1ae7795481c13d50b0f92376fcfa  normalization.md
3e641ee353effae94c3751b198cb08f1728bf225bc9963356befea1d722df49b  accepted-diffs.md
53f3ffd39178c5b61337e924f807aae2e476bf43deb5b95de48b163c54ff087f  ../../scripts/gateway-parity/build_proxy.py
53d66178ebe1a368a97dca37283795a4979efb7537eac57e918bd798393d0752  ../../scripts/gateway-parity/check_frozen.py
c7af644c482abd6a0771cd9a040794e98a94b3b7ca0e3df2c4c45c6026cd1774  ../../scripts/gateway-parity/check_ruler.py
d87d2a0feefdfb8da17c168ee5e64e75b590e2870672f8715e742b3cd5008b72  ../../scripts/gateway-parity/common.py
6c09b13a6a021b8846e94648c1dd1ce1df6347aa57f659b2a46c08baf2190f1b  ../../scripts/gateway-parity/compare.py
cc49ab21c970b7d53797a775375f52c4621089b4205d0b3b0bc2993fda20f776  ../../scripts/gateway-parity/controls/prerequisites.py
877c61bd280b479d80acda7d5a5b93f7bf64deaf8a52164cd14615cc1a4c3d25  ../../scripts/gateway-parity/controls/run_all.py
5f8e906fa532a58b994dff4f5e6438b07e31f14195b8142b75b398b6778e9e63  ../../scripts/gateway-parity/driver.mts
95fe265a7b7512f1969f19b7ee2d14133ce56916fb25c0953a84b5ec05568f74  ../../scripts/gateway-parity/mock.mjs
3a758aa57cbb5d8129c2e9b73c0baa04028a87aa7bae5dba6d8519f713869692  ../../scripts/gateway-parity/observer.py
93b1258e03202e6342f98b2d505edbfe7ee76916630663a852237c0cae90eda5  ../../scripts/gateway-parity/run_round.py
493962b18821f4dde42f375817a1c02631e547f89516ce3628fe0ab3d1241b15  ../../scripts/gateway-parity/test_safety.py
e1dfeac23d5cfd0777fd2836cb1011430c73d6b3b10fe44724738194f49901df  ../../scripts/gateway-parity/watcher.mjs
41a9347543d9907f58fd2b075e8b83343e03326e95b1aa663ea87fdc72d43222  ../../scripts/gateway-parity/session_domain.py
855136b60c4579eda257224fe75c8faaf89ad54eeaa85abff9eb369e02cc6661  ../../scripts/gateway-parity/session_observations.mjs
4007a0bc9742605282c21ab64f7455b629069bd5df455795dea0e621f007d6a9  ../../scripts/gateway-parity/controls/session_domain.py
41b1313fc047e8334a1655b69e277c4bcc451edaf15b79efd37e403e7005dab2  ../../scripts/gateway-parity/fixtures/session-calibration.json
8ea02eadd41cc6bbf75c1e67c22152f37f6b729007ad1e3a34a190c9ab60188c  ../../scripts/a5-19/observation_rules.mjs
0248507e617648bd8e81e1a54ae8b448cd7b7a2f14f1542424adc3a83b398c84  ../../scripts/a5-19/run.py
b7373b5838370c43f5b35217f95ce688a0183b5494eb276a25d868a0c18517dd  ../../scripts/a5-19/six_cells.mts
8ebd5ebaf0a189f9bf6727eb51765858abfe0afc94698b09ee53d6b707b7ec64  ../../scripts/a5-19/append-observer.mjs
c2f69c000b4dd6e5f2912b05dbc846641d5cb7bac82976a6b1a1cf0e16fdee84  ../../scripts/a5-19/cleanup-observer.mjs

> **2026-10-22 修复轮 8 重登记（PR #406，透明披露补齐）**：按本轮明确授权，accepted-diffs.md 的「已知未对齐（未接受，待 pr9c）」表格只追加第三条（240 层格式化元素 + 宽 P 子树在真实 TS 抛 `Invalid array length`、Rust 成功返回 Markdown），保留原有条目原文，并追加适用范围说明；本条属 V8 数组上限运行时行为，与深度批准无关。真实 TS 重测 2 / 4 MiB 并二分至 256 KiB 区间，Rust 2 / 4 / 8 MiB 原值与输出 SHA256 见 `_tmp/20261021-a65-pr9b/RESULT.md`「修复轮 8」。状态为「未批准，非已接受差异」，不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希，其余不变。两家评审见 PR。

> **2026-10-22 修复轮 9 重登记（PR #406，select 家族透明披露补齐）**：按本轮明确授权，在「已知未对齐（未接受，待 pr9c）」表只追加 select 内标签家族一行，保留已有条目原文；真实 TS（默认 Turndown + domino）与正式 Rust 实测 11 例，其中 9 例不一致录入 `parserGaps`，2 例一致仅记 RESULT。追加说明当前为 3 类解析器差异、1 类宽子树运行时差异，共 4 条已知未对齐；select 家族未穷举。第三条补入 2 MiB 完整真实 TS 金样与 Rust 逐字节回放、4 MiB Rust 成功断言。两侧原值、负控与验证见 `_tmp/20261021-a65-pr9b/RESULT.md`「修复轮 9」。状态仍为「未批准，非已接受差异」，不新增豁免、不改判分语义；只重登记 accepted-diffs.md 哈希，其余不变。两家评审见 PR。

> **2026-10-22 修复轮 4 重登记（PR #476，issue #493）**：按用户本人明确指令，在 A6.4-STATS-BASELINEMODEL-001 节末追加「已实现，差异消除（本 PR 修复轮 4）」及实现/金样依据；Rust 补齐 baselineModel 与 modelPricing 键的引用校验，保留原账本条目。只重登记 accepted-diffs.md 哈希，不新增豁免、不改判分语义；两次真实 TS 生成一致与负控证据见 `_tmp/20261022-a64-pr9b/RESULT.md`「修复轮 4」，两家评审由协调者续办。

> **2026-10-22 人批重登记（issue #496）**：按用户本人选 B 的明确授权，accepted-diffs.md 只追加 A6.4-STATS-SIDECAR-001（降级追加写旁路文件，向安全偏离 TS），修复 retention 锁超时追加与压缩 rename 交错丢账；保留判断、账目行格式与聚合输出不变。真实 TS 两次生成逐字节一致、原版丢账实测及各类变异负控见 `_tmp/20261022-issue-496/RESULT.md`；仅重登记 accepted-diffs.md 哈希，其余哈希与判分语义不变，独立评审由协调者续办。

> **2026-10-22 issue #494 补实现重登记**：按本单用户明确授权，只在 A6.4-STATS-RETENTION-001 末尾追加 V8 共享解析器接入、三个时区真实 TS 回放依据与仍保留的未验证本地 TZif 子集；未宣称全量消除，PR 号待协调者创建后补记。仅重登记 accepted-diffs.md 哈希，不改阈值、装置与判分语义。金样两次一致、变异负控及测试证据见工作区 `_tmp/20261022-issue-494/RESULT.md`，独立评审由协调者续办。

> **2026-10-23 issue #502 只追加补登重登记**：按本单用户明确授权，全仓自述差异审计后，accepted-diffs.md 追加 32 条「已知未对齐（未批准）」六字段记录。既有账本原文逐字节保留，AI 决策不冒充人批，既有人批条目只在代码加编号引用；不新增豁免、不改阈值或判分语义。五处低成本对齐（count URL、managed env、合法扩展年份、transcript/run registry JSON 语法文案）的真实 TS 金样两次一致及独立变异负控见 `_tmp/20261022-issue-502-diff-backfill/RESULT.md`。仅重登记 accepted-diffs.md 哈希；未改在途 turn/edgeclaw 文件。独立评审由协调者续办，本记录不声称已完成双评审。
