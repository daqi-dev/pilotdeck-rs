# B2 装置与待定标采集

规约正本为 `specs/ui-server-parity/{acceptance,normalization,accepted-diffs,multimodal}.md`，均为 DRAFT。`FROZEN.md` 首行保留「待定标」，哈希检查通过也只能 DRAFT/2；去掉占位、登记审批与依据之前不能宣称冻结。`Approved-By:` 接受具名人，或精确的「AI 代批，待人复核（协调者）」；两种方式都必须同时有唯一的 `Approval-Basis:`，写明定标报告路径及两家独立评审路径（至少三个不同的具体文件路径）。空值、占位与重复字段不能批准。采集装置 hash 绑定四份规约、全部装置及 B1 共用依赖；审批清单 FROZEN.md 自身不参与 capture hash，避免补定标/评审/审批字段后反过来使本批采集失效。

威胁模型（协调者自决，**AI 代批，待人复核**）：防意外漂移，以及 worker / 开发者在仓库内对装置的改动，包括未跟踪文件、影子模块和旧构建产物。不防拥有本机写权限、能够同时篡改装置、哈希清单与审批的恶意者；模型外发现按 P2 跟进。Git 跟踪清单、FROZEN 哈希和审批材料是本模型的信任依据。

装置目录 `scripts/ui-server-parity/` 与 `scripts/exp008/` 的实际文件必须**恰好等于 Git 跟踪内容**，两目录的全部跟踪文件都进入 FROZEN 和 capture hash。任何多余/未跟踪文件（含被 .gitignore 忽略的文件）、缺失跟踪文件、符号链接（目录/文件/断链）、`.pyc`、`__pycache__` 或非普通文件均 FAIL。递归扫描不跟随链接，不以排除缓存或影子模块的方式继续运行。审批清单 FROZEN.md 自身不进入 capture hash。

所有 B2 Python 入口和 Python 子进程使用 `python3 -I`，隐含 `-E -s`；入口先断言 isolated、ignore_environment、no_user_site 和 `sys.pycache_prefix is None`，不带 `-I` 或显式 `-X pycache_prefix` 均 FAIL。`PYTHONPATH`、`PYTHONHOME`、`PYTHONUSERBASE`、`PYTHONPYCACHEPREFIX` 等环境变量由解释器忽略。入口在执行装置预检源码前先校验其 FROZEN 哈希；清单缺失时只允许与 HEAD 相同的装置源码，checker 仍返回 DRAFT/2。本目录模块由显式绝对路径的源码加载器校验哈希后执行，不走 pyc，也不把装置目录加入 sys.path。B1 compare/run 同样先验哈希，加载结束恢复 sys.path 和 compare 模块绑定；reconcile 子进程也带 `-I`。关闭字节码写入；unittest 的第一个测试由标准库 discovery 导入，因此测试命令额外带 `-B`，避免在入口代码执行前写缓存（`-I` 会忽略 PYTHONDONTWRITEBYTECODE 环境变量）。

正式定标开始、每遍前后及续跑均要求装置路径 `git status --porcelain --untracked-files=all` 为空（含四份规约及 FROZEN），并在 batch.json 记录 HEAD；HEAD 与批次记录不同即拒绝，必须另建批次。每批独占全新 `batch/cargo-target/`，已存在的 target 一律拒绝；Cargo 使用该目录构建，记录 target 路径、产物路径与 SHA256。每遍 run 只使用本批校验通过的产物，续跑不重建、不复用 `_build` 的旧产物；独立 run 也在其新输出目录内构建新 target。

`Approved-By:` 拒绝 TODO、模型名（Claude/GPT/GLM 等）和零宽/不可见字符；精确的协调者代批串仍可用。`Approval-Basis:` 至少三项不同的真实文件，按 realpath 去重，路径必须落在当前仓库或共享主仓的 `_tmp/`，不得借别名、不存在文件或符号链接逃逸凑数。

离线验证（不监听）：

```sh
python3 -I -B -m unittest discover -s scripts/ui-server-parity -p 'test_*.py'
python3 -I scripts/ui-server-parity/controls/run_all.py --out _tmp/b2-controls.json
python3 -I scripts/ui-server-parity/check_frozen.py --self-test
python3 -I scripts/ui-server-parity/check_frozen.py  # 草稿哈希正确时退出 2
python3 -I scripts/ui-server-parity/benchmark_clock.py
bash -n scripts/ui-server-parity/calibrate.sh
bash scripts/ui-server-parity/calibrate.sh --preflight
```

`calibrate.sh` 与 `calibrate.py` 入库并登记 hash，便于固定装置和复现；`_tmp/` 仅保存输出。正式采集须在**允许 loopback listen 的环境**运行，使用 Node 22 和已经备好依赖的只读 TS 基线，不触碰用户运行的 PilotDeck 或用户 home。

```sh
# 在当前 checkout 根执行；可显式传 --baseline 和 --size-reference。
bash scripts/ui-server-parity/calibrate.sh
# 输出 batch 路径；按 batch.json 的同一 UTC 日/同源绑定续跑。
bash scripts/ui-server-parity/calibrate.sh --batch /absolute/path/to/batch
# 某遍 REFUSED 时只重采该遍；例如 ref7。然后用上一条命令继续。
bash scripts/ui-server-parity/calibrate.sh --batch /absolute/path/to/batch --retry ref7
```

脚本持有 `/tmp/pdrs-parity.lock`，依次采 20 ref、5 TS，再先执行 TS-only `check_ruler`，通过后才采 1 proxy 并执行带 `--proxy` 的 `check_ruler`。每遍调用生产 `run.py` 的 ClockMonitor，保存并重算 `clock_regression_check`。所有运行使用该 batch 的固定 `slot/direct` 路径，服务停止后才改名归档；不改 capture metadata。结果为 `summary.json`，机器门全过的目标是 `PENDING_REVIEW`、TS 10/10、proxy PASS、26 遍时钟 PASS，独立评审仍 null。

REFUSED 原档和日志保留在 `attempts/`；重采只能替换那一个尚未接受的轮次。PASS 轮次不可覆盖；计分开始后不可替换参考池，池与计分身份/录制 hash 不重复、区间不重叠。跨 UTC 日、源码/装置变更、已有 slot 尚未归档、采集 FAIL、池分类 REFUSED 等需查看证据，不能用改 metadata 或 replay 标记补救。跨日/变更必须新建完整 batch。因中断留下 slot 时脚本拒绝覆盖，先按 audit 核实自有进程清理再人工归档。

磁盘按 git common dir 找到共享主仓，统计其 `_build` + `_tmp`（包括 worktree 内输出），上限保守取十进制 30 GB。默认只读查找历史 live3 单遍目录，取实际占用最大的完整遍作尺寸参考；也可显式 `--size-reference` 指定。预计每遍尺寸为历史尺寸 ×1.5，另预留构建 1 GB、报告 1 GB；开始、构建后、每遍前后和尺子检查前后重查，并校验文件系统可用空间。估算是启动门，若输出增长耗尽预算则停止并保留证据。历史 live3 只作磁盘估算，不进入新 TS 参考池。
