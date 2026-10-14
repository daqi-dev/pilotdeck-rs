# B2 装置与待定标采集

规约正本为 `specs/ui-server-parity/{acceptance,normalization,accepted-diffs,multimodal}.md`，均为 DRAFT。`FROZEN.md` 首行保留「待定标」，哈希检查通过也只能 DRAFT/2；去掉占位、登记具名人批之前不能宣称冻结。采集装置 hash 绑定四份规约、全部装置及 B1 共用依赖；审批清单 FROZEN.md 自身不参与 capture hash，避免补定标/评审/人批字段后反过来使本批采集失效。

离线验证（不监听）：

```sh
python3 -m unittest discover -s scripts/ui-server-parity -p 'test_*.py'
python3 scripts/ui-server-parity/controls/run_all.py --out _tmp/b2-controls.json
python3 scripts/ui-server-parity/check_frozen.py --self-test
python3 scripts/ui-server-parity/check_frozen.py  # 草稿哈希正确时退出 2
python3 scripts/ui-server-parity/benchmark_clock.py
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
