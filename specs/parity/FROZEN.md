# 冻结记录

> **2026-10-30 fix-r1 重登记（#1067 同源三控制新检出 setUp 修复，**AI 代批，待人复核**）**：把上轮 A1 只修 `lock_release.py` 的同类病按 sibling 评审跟改到位：`controls/review_r1.py` setUp、`controls/run_all.py` run_controls、`controls/multimodal.py` run_controls 各自在 `TemporaryDirectory(dir=G.WORKTREE / '_tmp')` 前先 `(G.WORKTREE / '_tmp').mkdir(parents=True, exist_ok=True)`（与 `lock_release.py` 上轮已合并写法同款）。不改判分语义、阈值、分母、归一化、比较器、锁协议；`round.py`、`lock_release.py`、`README.md`、`accepted-diffs.md` 哈希不变。验证：`git archive HEAD` 全新检出（无 `_tmp/`、无 .git）上修复前 review_r1 9/9 setUp FileNotFoundError、run_all `ok:false` error（run_all 自身入口先报错）；只修 run_all 后其流程先建 `_tmp/`，未修的 multimodal 在该联合流程中被掩盖（实测：未修 multimodal 在 `_tmp/` 已被建时可建自己的临时目录，`rm -rf _tmp` 后同一未修文件在 multimodal.py:51 FileNotFoundError），multimodal 独立成病以直调 `run_controls` 证明；全修后 run_all 59 case 全 ok、review_r1 setUp 全过（self-compare.json 重生成后 9/9 OK）、multimodal 直调全 ok。删 mkdir 行的三个变异体各自绑定被点名控制 KILLED、字节还原（sha256 校验）后复过。`check_frozen` 复跑 PASS。原始输出 `_tmp/20261026-issue-1067/fix-r1/raw/`。

> **2026-10-30 重登记（#1067 A1 控制新检出 setUp 修复，**AI 代批，待人复核**）**：只改 `scripts/parity/controls/lock_release.py` 的 setUp：先 `(G.WORKTREE / '_tmp').mkdir(parents=True, exist_ok=True)` 再建 TemporaryDirectory（同文件 _assert_cleanup_blocks_round / test_interrupt 两条用例已有同写法）。`_tmp/` 被 .gitignore，新检出不存在，原 setUp 每用例抛 FileNotFoundError，宿主负控 17 项里 8 项 A1 全 INVALID「bound positive test did not pass」（与 #1056 同类）。不改产品 `round.py`、锁协议、判分语义、阈值、分母、归一化、比较器；`round.py` 与 `README.md` 哈希不变。验证：`git archive HEAD` 全新检出（无 `_tmp/`、无 .git）上修复前 9/9 setUp FileNotFoundError（原始输出 `_tmp/20261026-issue-1067/raw/01`），修复后 9/9 OK（raw/02）；删 mkdir 行的变异体 9/9 FAILED、恢复后 OK（raw/03、raw/04）。`check_frozen` 复跑 PASS。

> **2026-10-26 fix-r2 重登记（#209 A1 清理故障路径恢复，**AI 代批，待人复核**）**：只改 `scripts/parity/controls/lock_release.py` 的测试：保留 fix-r1 的「TERM/KILL 投递失败但最终 ESRCH 成功」用例，补回 fix-r1 删掉的 wait 超时与最终探测 OSError 两条故障路径，各自独立注入并走真实 `stop_group → _stop_group` 与 `round.main()` 清理门：断言 round BLOCKED、LIVE_GROUPS / 共享锁 / `cleanupPending` 保留。四条新分支各配负控（wait 错误被忽略、探测 OSError 被当作组已消失），本机 17/17 KILLED。产品 `round.py`、判分语义、阈值、分母、归一化、比较器均未动；`round.py` 与 `README.md` 哈希不变。`check_frozen` 复跑 PASS。

> **2026-10-26 fix-r1 重登记（#209 A1 测试补强，**AI 代批，待人复核**）**：只改 `scripts/parity/controls/lock_release.py` 的测试断言：清理 OSError 用例改为「组已消失（最终 ESRCH 成功）但 TERM/KILL 投递失败」，使 `errors` 单独决定 `allStopped`；cleanupPending 接管用例的 `assertRaises` 专属消息即绑定失败点，去掉其内部与绑定消息不一致的 `self.fail`。产品 `round.py`、判分语义、阈值、分母、归一化、比较器均未动；`round.py` 与 `README.md` 哈希不变。`check_frozen` 复跑 PASS。

> **2026-10-26 重登记（#209 A1 共享锁释放门控，**AI 代批，待人复核**）**：`scripts/parity/round.py` 改为只在本次启动的会话组被 `killpg`+有界 `wait`+最终 `killpg(0)` 证实全部停止后才释放 `/tmp/pdrs-parity.lock`；spawn 与登记、清理和释放 guard 期间挂起 SIGINT/SIGTERM，SIGTERM 与 KeyboardInterrupt 走同一回收路径；`_stop_group` 的 kill/wait/探测错误不再被当成「已停止」，未证实时原子改写 `owner.json` 的 `cleanupPending` 为残留 pgid 并退出 BLOCKED(1)，带 `cleanupPending` 的锁不被 dead_pid/expired 自动接管。只改装置生命周期，不改判分语义、阈值、分母、归一化或比较器；`specs/parity/README.md` 未动、哈希不变（`shasum -a 256 README.md` = `f8958a3280a95df7…`）。新增 `scripts/parity/controls/lock_release.py` 覆盖信号挂起、残留组保留、cleanupPending 拒绝接管、清理 OSError 和中断后 BLOCKED 回收；负控 `src/pilotdeck-gateway/tests/fixtures/parity-lock-release/negative_controls.py` 在宿主提交后运行。真实数据复算：`self_compare --source /Users/da/ws/pilotdeck-rs/_tmp/20261014-ts5` 得 k=5、S=53、无排除；`controls/run_all --source /Users/da/ws/pilotdeck-rs/_tmp/20261014-exp001/parity-run1` 的 59 个 case 全部 ok（real_self_positive 53/53、G03 规则对照全 PASS），原始输出见本 worktree `_tmp/20261026-issue-209/raw/`。

> **2026-10-25 重登记（#694 冻结装置加固，PR 待填，**AI 代批，待人复核**）**：把 A5.4 第 2 包（`claude/a54-pkg2`）fix-r5/r6 已定稿的三项常规加固同步到 `scripts/parity`，只变严，不改判分语义、阈值、分母；`specs/parity/README.md` 未动、哈希不变。① `check_frozen.py` 不再排除 `__pycache__`/`.pyc`：装置目录内任何 `__pycache__` 目录（含空目录）、`.pyc`/`.pyo` 文件一律判未登记并使检查 FAIL（旧行为是跳过；PEP 552 unchecked-hash 缓存可在扫描前被 import 并覆盖检查器，属 P1 假 PASS）。② 8 个入口（`check_frozen.py`、`check_ruler.py`、`g03_compare.py`、`round.py`、`score.py`、`self_compare.py`、`controls/run_all.py`、`controls/review_r1.py`）与 `controls/multimodal.py` 在导入任何装置模块前断言 `sys.flags.isolated`，设置 `sys.dont_write_bytecode = True` 并把 `sys.pycache_prefix` 指向 `tempfile.mkdtemp()`；正常运行后装置目录不产生缓存。③ 入口与文档调用一律 `python3 -I`；`-I` 不把脚本目录加入 `sys.path`，故装置模块改由 `importlib.util.spec_from_file_location(name, path)` 显式加载并断言 `module.__file__`，不再 `sys.path.insert` 或裸 `import g03_compare`。负控：放 `x.pyc`、`__pycache__/y.pyc` 时 `check_frozen` FAIL 并列出未登记字节码，移除后逐字节还原 PASS；清空缓存后跑 controls/self_compare/check_ruler，装置目录 `find -name __pycache__ -o -name '*.pyc' -o -name '*.pyo'` 为空。真实数据复算：`self_compare --source _tmp/20261014-ts5`、`controls/run_all --source _tmp/20261014-exp001/parity-run1`、`check_ruler --self-compare ... --control-source ...`、`check_frozen` 的原始输出见主仓 `_tmp/20261025-issue-694/`；全量 53 场景对拍按指示未跑，由协调者持锁另跑。威胁模型同人批 R4：防意外漂移与仓库内常规改动，不防本机恶意者；刻意构造的解释器级绕过（专为欺骗加载器构造的 pyc、同名包遮蔽、无 `-I` 时不屏蔽标准库等）登记为已知边界，不在本单追。本次重登记 10 个 `scripts/parity` 装置文件哈希行与 `acceptance.md`（仅复算命令改 `-I`，判分语义/阈值/分母未变）；`bind.mjs` 未改，行不变。

| 文件 | sha256（前 16 位） | 冻结时间 | 说明 |
|---|---|---|---|
| `README.md` | `f8958a3280a95df7c9e2032247b3803dd383e30b033b49292540af302cb4a6e5` | 2026-10-14（人过目） | 比什么 · 四项对照 · 阈值程序（k = 5、S 规则、\|S\| ≥ 45、分母）· 差异分诊 |
| `acceptance.md` | `5e35715e01061778d1ab80a1f78c65cbd4a1427365c4a28d0bd3acfe687eea1c` | 2026-10-25 #694 重登记：仅复算命令改 `python3 -I`（**AI 代批，待人复核**） | k = 5、S = 53（无排除）、Rust 配对规则、批准记录 |
| `accepted-diffs.md` | `12ca6dbfc7f161a0bdc18e2a2c927152d0c45285f5a58398b9ee5e3dbc4873b2` | 2026-10-30 #1088（**AI 代批，待人复核**） | 历史账本原字节冻结；新条目只新增 diffs/<ID>.md |
| `multimodal.md` | `e8a7b4c4025a2a3cccbabc34b3f2b3c8542f58268ccf327f5e4937e19e890742` | 2026-10-14 #35 新增（**AI 代批，待人复核**） | 原版多态场景登记（cancel_during_tool：TS 20 遍 17/3 两类）与每日重采流程 |
| `normalization.md` | `5a0c10b70a89a42aa7808f2a0520116e557a4614545f3662fc19dccff1568450` | 2026-10-14（**AI 代批，待人复核**） | 两侧同路径硬前提、路径对齐、ID / 时间、seq、请求体 |
| `scripts/parity/bind.mjs` | `1b4a38037cf526c6225e69bb4faeb24835d9390f37bed3becb0f8b0f8fd6bfd3` | 2026-10-14（**AI 代批，待人复核**） | 比较器 / 驱动 / 对照 |
| `scripts/parity/check_frozen.py` | `9f55fd274395ffbd6481336766db1c59a40a027964cc729fc85b313caeedd93a` | 2026-10-25 #694 重登记（**AI 代批，待人复核**） | 比较器 / 驱动 / 对照 |
| `scripts/parity/check_ruler.py` | `34c8d0999aee2eff737401326f07e0ae93573953317e5a412c096aed43ad1cf0` | 2026-10-25 #694 重登记（**AI 代批，待人复核**） | 比较器 / 驱动 / 对照 |
| `scripts/parity/controls/README.md` | `36aee14ea36dcd70c07c3fc66b565108ad34b060da943dc1a32420f3b5b593b1` | 2026-10-25 #694 重登记（**AI 代批，待人复核**） | 比较器 / 驱动 / 对照 |
| `scripts/parity/controls/lock_release.py` | `571fe225db5ab6eb09d74c7c88f97cd510fe9cfae6dc0b90a16a1cb88de55ae8` | 2026-10-30 #1067 重登记（**AI 代批，待人复核**） | 信号挂起 / 残留组保留 / cleanupPending 拒绝接管 / 中断 BLOCKED；TERM-KILL 失败、wait 超时、最终探测 OSError 三条未证实路径与接管断言即负控绑定失败点；setUp 先建 `_tmp/`（新检出可跑） |
| `scripts/parity/controls/multimodal.py` | `8479d196bacf3f32b69e3a385d904057601c37a27efd9b4060721d4aa0668996` | 2026-10-30 #1067 fix-r1 重登记（**AI 代批，待人复核**） | 多态与日期前提的负例 / 正例；run_controls 先建 `_tmp/`（新检出可跑） |
| `scripts/parity/controls/review_r1.py` | `e47a3ca2bc05047e9943cb5b05335e914c07faf6d91b488b042b30dffc1bceb5` | 2026-10-30 #1067 fix-r1 重登记（**AI 代批，待人复核**） | 比较器 / 驱动 / 对照；setUp 先建 `_tmp/`（新检出可跑） |
| `scripts/parity/controls/run_all.py` | `95cdeec76ada020f47ace8fa55dc2b8149a849ffea3273086bfaf28898ccd57d` | 2026-10-30 #1067 fix-r1 重登记（**AI 代批，待人复核**） | 比较器 / 驱动 / 对照；run_controls 先建 `_tmp/`（新检出可跑） |
| `scripts/parity/g03_compare.py` | `ce60e7c3705f4ee1ebdd4d1f8c834bd2828dc3b52cabb1b468f70a38a696e9fb` | 2026-10-25 #694 重登记（**AI 代批，待人复核**） | 比较器 / 驱动 / 对照 |
| `scripts/parity/round.py` | `e70e6f3b63a814ebd69991cbd7603f67472f85731f13c57d99c29d282b3062c8` | 2026-10-26 #209 A1 重登记（**AI 代批，待人复核**） | 锁只在本次会话组证实停止后释放；清理期间挂起 SIGINT/SIGTERM |
| `scripts/parity/score.py` | `06a7d8040a661ca326c9b00bd1556b99c9f5026ac808db6ffea9b43777107812` | 2026-10-25 #694 重登记（**AI 代批，待人复核**） | 计分：普通第 i 对第 i；多态整类匹配；日期 / 同源 / 不重叠前提 |
| `scripts/parity/self_compare.py` | `a21f7a890733e59e9c23985cb8f3aa68ee77b1082ac46ff0a4623b791449a617` | 2026-10-25 #694 重登记（**AI 代批，待人复核**） | 比较器 / 驱动 / 对照 |

校验：`python3 -I scripts/parity/check_frozen.py`（全部登记文件的 sha256 前缀）。**冻结后改任何一份都要另开 issue、两家评审同意、写明理由**（`CLAUDE.md` §五）。

## 2026-10-30 #1088 账本一条一文件重登记

AI 代批，待人复核。依据 #1085 人定 D / #1088 PR-1，只移植 gateway 的 diffs/ 六字段校验、组合阅读与 --base 只新增保护；不跨装置 import。历史 accepted-diffs.md 原字节保留，新条目与更正分别新增文件；不改比较器、阈值、分母、场景、定标输入和判分路径。

旧登记只绑定表头，本次将当前全文 SHA256 登记为 12ca6dbfc7f161a0bdc18e2a2c927152d0c45285f5a58398b9ee5e3dbc4873b2，移除表头回退。--base 仅放行源码中固定的旧行→本次全文行这一对迁移，仍核对历史账本原字节；以后账本哈希行逐字节冻结。

仅重登记本次 check_frozen.py 哈希。旧新真实登记逐项对照、负控、两家非作者评审及待验项目见本树 `_tmp/20261026-issue-1088/RESULT.md`；既有定标和历史批准依据不改。本次重登记的最终冻结仍须两家非作者评审完成及人复核，不以旧批准记录代替本次评审。

本次账本存放规则也取代现有 acceptance.md / README.md 中“追加到 accepted-diffs.md”的旧指令；这些冻结规约的字节与判分语义保持不变。完整账本的阅读与更正规则以 diffs/README.md 为准。
