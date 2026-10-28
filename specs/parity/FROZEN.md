# 冻结记录

> **2026-10-26 重登记（#209 A1 共享锁释放门控，**AI 代批，待人复核**）**：`scripts/parity/round.py` 改为只在本次启动的会话组被 `killpg`+有界 `wait`+最终 `killpg(0)` 证实全部停止后才释放 `/tmp/pdrs-parity.lock`；spawn 与登记、清理和释放 guard 期间挂起 SIGINT/SIGTERM，SIGTERM 与 KeyboardInterrupt 走同一回收路径；`_stop_group` 的 kill/wait/探测错误不再被当成「已停止」，未证实时原子改写 `owner.json` 的 `cleanupPending` 为残留 pgid 并退出 BLOCKED(1)，带 `cleanupPending` 的锁不被 dead_pid/expired 自动接管。只改装置生命周期，不改判分语义、阈值、分母、归一化或比较器；`specs/parity/README.md` 未动、哈希不变（`shasum -a 256 README.md` = `f8958a3280a95df7…`）。新增 `scripts/parity/controls/lock_release.py` 覆盖信号挂起、残留组保留、cleanupPending 拒绝接管、清理 OSError 和中断后 BLOCKED 回收；负控 `src/pilotdeck-gateway/tests/fixtures/parity-lock-release/negative_controls.py` 在宿主提交后运行。真实数据复算：`self_compare --source /Users/da/ws/pilotdeck-rs/_tmp/20261014-ts5` 得 k=5、S=53、无排除；`controls/run_all --source /Users/da/ws/pilotdeck-rs/_tmp/20261014-exp001/parity-run1` 的 59 个 case 全部 ok（real_self_positive 53/53、G03 规则对照全 PASS），原始输出见本 worktree `_tmp/20261026-issue-209/raw/`。

> **2026-10-25 重登记（#694 冻结装置加固，PR 待填，**AI 代批，待人复核**）**：把 A5.4 第 2 包（`claude/a54-pkg2`）fix-r5/r6 已定稿的三项常规加固同步到 `scripts/parity`，只变严，不改判分语义、阈值、分母；`specs/parity/README.md` 未动、哈希不变。① `check_frozen.py` 不再排除 `__pycache__`/`.pyc`：装置目录内任何 `__pycache__` 目录（含空目录）、`.pyc`/`.pyo` 文件一律判未登记并使检查 FAIL（旧行为是跳过；PEP 552 unchecked-hash 缓存可在扫描前被 import 并覆盖检查器，属 P1 假 PASS）。② 8 个入口（`check_frozen.py`、`check_ruler.py`、`g03_compare.py`、`round.py`、`score.py`、`self_compare.py`、`controls/run_all.py`、`controls/review_r1.py`）与 `controls/multimodal.py` 在导入任何装置模块前断言 `sys.flags.isolated`，设置 `sys.dont_write_bytecode = True` 并把 `sys.pycache_prefix` 指向 `tempfile.mkdtemp()`；正常运行后装置目录不产生缓存。③ 入口与文档调用一律 `python3 -I`；`-I` 不把脚本目录加入 `sys.path`，故装置模块改由 `importlib.util.spec_from_file_location(name, path)` 显式加载并断言 `module.__file__`，不再 `sys.path.insert` 或裸 `import g03_compare`。负控：放 `x.pyc`、`__pycache__/y.pyc` 时 `check_frozen` FAIL 并列出未登记字节码，移除后逐字节还原 PASS；清空缓存后跑 controls/self_compare/check_ruler，装置目录 `find -name __pycache__ -o -name '*.pyc' -o -name '*.pyo'` 为空。真实数据复算：`self_compare --source _tmp/20261014-ts5`、`controls/run_all --source _tmp/20261014-exp001/parity-run1`、`check_ruler --self-compare ... --control-source ...`、`check_frozen` 的原始输出见主仓 `_tmp/20261025-issue-694/`；全量 53 场景对拍按指示未跑，由协调者持锁另跑。威胁模型同人批 R4：防意外漂移与仓库内常规改动，不防本机恶意者；刻意构造的解释器级绕过（专为欺骗加载器构造的 pyc、同名包遮蔽、无 `-I` 时不屏蔽标准库等）登记为已知边界，不在本单追。本次重登记 10 个 `scripts/parity` 装置文件哈希行与 `acceptance.md`（仅复算命令改 `-I`，判分语义/阈值/分母未变）；`bind.mjs` 未改，行不变。

| 文件 | sha256（前 16 位） | 冻结时间 | 说明 |
|---|---|---|---|
| `README.md` | `f8958a3280a95df7…` | 2026-10-14（人过目） | 比什么 · 四项对照 · 阈值程序（k = 5、S 规则、\|S\| ≥ 45、分母）· 差异分诊 |
| `accepted-diffs.md` | `653ccb1001d1b09a…` | 2026-10-14（人过目） | 表头；此后只增不删，AI 追加的条目状态为「待批」，追加不改表头 |
| `acceptance.md` | `5e35715e01061778…` | 2026-10-25 #694 重登记：仅复算命令改 `python3 -I`（**AI 代批，待人复核**） | k = 5、S = 53（无排除）、Rust 配对规则、批准记录 |
| `normalization.md` | `5a0c10b70a89a42a…` | 2026-10-14（**AI 代批，待人复核**） | 两侧同路径硬前提、路径对齐、ID / 时间、seq、请求体 |
| `scripts/parity/bind.mjs` | `1b4a38037cf526c6…` | 2026-10-14（**AI 代批，待人复核**） | 比较器 / 驱动 / 对照 |
| `scripts/parity/check_frozen.py` | `e062ed423ff3b9ce…` | 2026-10-25 #694 重登记（**AI 代批，待人复核**） | 比较器 / 驱动 / 对照 |
| `scripts/parity/check_ruler.py` | `34c8d0999aee2eff…` | 2026-10-25 #694 重登记（**AI 代批，待人复核**） | 比较器 / 驱动 / 对照 |
| `scripts/parity/controls/README.md` | `36aee14ea36dcd70…` | 2026-10-25 #694 重登记（**AI 代批，待人复核**） | 比较器 / 驱动 / 对照 |
| `scripts/parity/controls/review_r1.py` | `7466fdf193cb8a9c…` | 2026-10-25 #694 重登记（**AI 代批，待人复核**） | 比较器 / 驱动 / 对照 |
| `scripts/parity/controls/lock_release.py` | `9588c7c25a591b6e…` | 2026-10-26 #209 A1 新增（**AI 代批，待人复核**） | 信号挂起 / 残留组保留 / cleanupPending 拒绝接管 / 中断 BLOCKED |
| `scripts/parity/controls/run_all.py` | `3836225887515560…` | 2026-10-25 #694 重登记（**AI 代批，待人复核**） | 比较器 / 驱动 / 对照 |
| `scripts/parity/g03_compare.py` | `ce60e7c3705f4ee1…` | 2026-10-25 #694 重登记（**AI 代批，待人复核**） | 比较器 / 驱动 / 对照 |
| `scripts/parity/round.py` | `e70e6f3b63a814eb…` | 2026-10-26 #209 A1 重登记（**AI 代批，待人复核**） | 锁只在本次会话组证实停止后释放；清理期间挂起 SIGINT/SIGTERM |
| `scripts/parity/self_compare.py` | `a21f7a890733e59e…` | 2026-10-25 #694 重登记（**AI 代批，待人复核**） | 比较器 / 驱动 / 对照 |
| `multimodal.md` | `e8a7b4c4025a2a3c…` | 2026-10-14 #35 新增（**AI 代批，待人复核**） | 原版多态场景登记（cancel_during_tool：TS 20 遍 17/3 两类）与每日重采流程 |
| `scripts/parity/score.py` | `06a7d8040a661ca3…` | 2026-10-25 #694 重登记（**AI 代批，待人复核**） | 计分：普通第 i 对第 i；多态整类匹配；日期 / 同源 / 不重叠前提 |
| `scripts/parity/controls/multimodal.py` | `f20b3aed888d407c…` | 2026-10-25 #694 重登记（**AI 代批，待人复核**） | 多态与日期前提的负例 / 正例 |

校验：`python3 -I scripts/parity/check_frozen.py`（全部登记文件的 sha256 前缀）。**冻结后改任何一份都要另开 issue、两家评审同意、写明理由**（`CLAUDE.md` §五）。
